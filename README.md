# ChronusGateway-RS

ChronusGateway-RS (Chronus-GS) is an asynchronous, physics-validated Telemetry and Command
(TMTC) ground-station gateway written in Rust. It ingests raw spacecraft downlink frames,
parses them against open CCSDS standards, cross-checks each frame against the spacecraft's
computed orbital physics, and prepares validated telemetry for web-based mission control
dashboards such as NASA Open MCT — in a single, memory-safe, garbage-collection-free executable.

Its distinguishing feature is a **Physics-Telemetry Co-Validation** engine: rather than checking
telemetry only against static limits, the gateway uses a live orbital propagator to derive the
expected Doppler shift, look-angles, and link geometry for the spacecraft, and flags frames whose
measured RF and signal parameters disagree with the physics.

> **Status:** Early development. Ingestion (M1), CCSDS parsing (M2), station tracking (M3), and the
> Physics–Telemetry Co-Validation engine (M4) are implemented and tested. Open MCT WebSocket
> distribution is Milestone 5 (see [`BUILD_PLAN.md`](BUILD_PLAN.md)).

---

## Architecture

The gateway is built around two principles: an asynchronous, lock-free network core and a clean
abstraction boundary between the pipeline and any astrodynamics backend.

```
Raw RF / SDR ──▶ Async UDP ingestion ──▶ CCSDS zero-copy parser ──▶ Physics-Telemetry
 (UDP today)       (Tokio)                  (TelemetryFrame)         Co-Validation engine
                                                                            │
                  OrbitalPropagator trait ◀── range-rate / look-angles ─────┤
                  (Ephemerust today, nyx-space later)                       ▼
                                                             Structured logs today
                                                             WebSocket/Open MCT in M5
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion and a lossy broadcast
  channel so slow consumers never block the socket. WebSocket fan-out is the next milestone.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
  `OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
  library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
  clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.
- **Runtime path today.** `cargo run` binds UDP `127.0.0.1:7301`, parses CCSDS telemetry packets,
  computes a throttled `TrackingState`, applies available physics validation, and logs the decoded
  frame. HTTP, WebSocket, metrics, and Open MCT JSON contracts are planned work.

The reasoning behind these and other choices is recorded in [`Methodology.md`](Methodology.md).

---

## Repository layout

```
chronus-gateway/
├── Cargo.toml              Workspace manifest (centralized dependency versions, MSRV 1.88)
├── crates/gateway/         The gateway binary + library
│   ├── src/
│   │   ├── lib.rs          Crate documentation and module wiring
│   │   ├── config.rs       Ingestion configuration
│   │   ├── ingest.rs       Asynchronous UDP ingestion loop (RawFrame, stats, shutdown)
│   │   ├── ccsds.rs        CCSDS Space Packet parsing (TelemetryFrame, validation)
│   │   ├── validate.rs     Physics–Telemetry Co-Validation (Doppler, elevation, physics_flags)
│   │   ├── propagator.rs   OrbitalPropagator trait + Ephemerust-backed implementation
│   │   └── main.rs         Entrypoint (UDP ingest → parse → validate → structured logs)
│   └── tests/
│       └── ingest.rs       Milestone 1 integration tests
├── AGENTS.md               Project constitution (compliance, attribution, security, testing)
├── Methodology.md          Decision log: the reasoning behind major choices
├── BUILD_PLAN.md           Iterative, stage-gated implementation roadmap
└── TEST_PLAN.md            Companion test plan and tolerance register
```

---

## Building and running

The project targets Rust 1.88 or newer and consumes the Ephemerust library as a sibling
checkout. The expected on-disk layout places both repositories next to each other:

```
…/Rust/
├── chronus-gateway/
└── Ephemerust/
```

```bash
cargo build      # compile the workspace
cargo run        # bind 127.0.0.1:7301 and run the M1-M4 telemetry pipeline
cargo test       # unit + integration + doctests
```

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

### Runtime workflow and public interfaces

The current executable uses default in-code configuration only; there is no CLI, environment
config, HTTP API, or WebSocket endpoint yet.

| Stage | Public API / type | Current behavior and constraints |
|-------|-------------------|----------------------------------|
| Ingest | `ingest::bind`, `ingest::run`, `RawFrame`, `IngestStats` | Binds UDP, copies each datagram into `Arc<[u8]>`, broadcasts on a bounded lossy channel, and stops on Ctrl-C. |
| Parse | `ccsds::parse_telemetry`, `TelemetryFrame`, `CcsdsError` | Parses the CCSDS Space Packet primary header via `spacepackets`; rejects short, truncated, malformed, or TC packets without panicking. |
| Track | `StationConfig`, `EphemerustPropagator`, `TrackingProvider` | Resolves an inline/file TLE and caches look-angle/range-rate recomputation for the configured interval. |
| Validate | `apply_physics_validation`, `RfMetadata`, `physics_flags` | Clears and sets anomaly bits: bit 0 Doppler anomaly, bit 1 below-horizon, bit 2 reserved for RSSI/link budget. |

Default configuration values are intentionally development-safe:

| Setting | Default | Notes |
|---------|---------|-------|
| UDP bind address | `127.0.0.1:7301` | Loopback only; bind to a NIC or `0.0.0.0` in future config plumbing for off-host SDR traffic. |
| Broadcast capacity | `1024` frames | Lossy by design; lagging consumers see `RecvError::Lagged`. |
| Max datagram size | `65_542` bytes | Fixed receive buffer: CCSDS 64 KiB packet data field plus 6-byte primary header. |
| Default station | `35.0°N, 116.0°W, 1000 m` | Synthetic/demo ground station. |
| Default TLE | Public ISS (ZARYA) reference TLE | Kept inline for deterministic local runs; use only public/reference data. |
| Nominal carrier | `437_500_000 Hz` | Used by Doppler validation when measured carrier metadata is present. |
| Doppler tolerance | `±150 Hz` | `T-DOPPLER` in `TEST_PLAN.md`; rationale in `Methodology.md` D-012. |
| Minimum elevation | `0°` | Frames below the mathematical horizon set `physics_flags` bit 1. |
| Recompute throttle | `10 ms` | Approximately 100 Hz; `0` disables caching. |

### Local loopback runbook

1. Start the gateway with structured logs:

   ```bash
   RUST_LOG=info cargo run
   ```

2. From another terminal, send a synthetic CCSDS telemetry packet to the default UDP port:

   ```bash
   python3 - <<'PY'
   import socket

   # TM packet, APID 0x02A, sequence 7, unsegmented, 5-byte payload "hello".
   packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
   sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
   sock.sendto(packet, ("127.0.0.1", 7301))
   PY
   ```

3. The gateway should log `telemetry frame parsed` with `apid=42`, `seq=7`, and
   `payload=5`. If the propagator returns a state for the frame timestamp, the log also includes
   azimuth, elevation, range, range-rate, and `physics_flags`.

### Troubleshooting and operational pitfalls

- **Missing `../Ephemerust`:** the workspace depends on Ephemerust as a sibling path checkout.
  Build errors about the `ephemerust` dependency usually mean the expected directory layout is
  missing.
- **No off-host packets by default:** `127.0.0.1:7301` accepts only local loopback traffic.
- **Doppler is skipped in the default binary:** `main.rs` currently passes `RfMetadata::default()`
  because SDR carrier metadata is not wired yet. Elevation validation still runs when tracking
  state is available.
- **Slow consumers can lose frames:** the broadcast channel favors freshest telemetry over
  guaranteed delivery. Check logs for `consumer lagged; dropped frames`.
- **Oversized UDP datagrams are OS-specific:** Windows reports and drops oversized datagrams;
  Unix may truncate them, after which CCSDS length validation rejects malformed packets.
- **No HTTP/WebSocket server yet:** `GET /health` and `GET /telemetry/openmct` are planned for
  Milestone 5, not shipped behavior.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP, planned in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- **CCSDS Space Packet Protocol** — open CCSDS packet framing standards; the current parser handles
  the 6-byte Space Packet primary header and validates declared packet data length.
- **[`spacepackets` crate](https://crates.io/crates/spacepackets)** — CCSDS parsing dependency
  selected in `Methodology.md` D-010.
- **[Ephemerust](https://github.com/IsomorphicAlgo/ephemerust)** — SGP4 look-angle and range-rate
  backend used through the `OrbitalPropagator` trait.
- **CODATA speed of light constant** — `299_792_458 m/s`, used by the non-relativistic Doppler
  calculation in `validate.rs`.
- **Project planning docs** — [`BUILD_PLAN.md`](BUILD_PLAN.md), [`TEST_PLAN.md`](TEST_PLAN.md),
  [`Methodology.md`](Methodology.md), and [`AGENTS.md`](AGENTS.md) are the canonical roadmap,
  tolerance register, decision log, and project constitution.

---

## Acknowledgements

ChronusGateway-RS builds directly on prior work, and credit is given accordingly:

- **[Ephemerust](https://github.com/IsomorphicAlgo/ephemerust)** — the SGP4-based orbital
  mechanics and satellite-tracking library that provides the look-angle and range-rate
  computations underpinning the co-validation engine. Authored by the same maintainer.
- **Rusty_Server** — an earlier asynchronous networking and REST service by the same maintainer,
  whose Tokio/Axum architecture and integration patterns informed this gateway's design.
- **[`sgp4`](https://crates.io/crates/sgp4)** — the validated SGP4/SDP4 propagator that
  Ephemerust delegates to for numerical orbit propagation.
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the CCSDS parsing crate used for
  Space Packet primary-header decoding and validation.
- **[Tokio](https://tokio.rs/)**, **Tracing**, **Serde**, **Chrono**, **Anyhow**, and
  **Thiserror** — the Rust infrastructure crates used for async execution, logging,
  serialization-ready data types, time handling, and structured errors.
- **[Axum](https://github.com/tokio-rs/axum)** — the planned HTTP/WebSocket framework for the
  Milestone 5 distribution layer, following Rusty_Server-inspired patterns.
- **[CCSDS](https://public.ccsds.org/)** — the open international standards for space packet
  framing and protocols that define the gateway's wire formats.
- **[NASA Open MCT](https://nasa.github.io/openmct/)** — the open-source mission-control
  framework targeted by the planned distribution layer.
- **[NeXosim](https://github.com/asynchronics/nexosim)** — the discrete-event simulation
  framework planned for hardware-in-the-loop validation.

The broader Rust aerospace ecosystem — including `sat-rs`, `spacepackets`, and `nyx-space` —
informed the design analysis.

---

## License and compliance

Licensed under the MIT License.

This project is designed strictly around open international standards (CCSDS) and is published
openly to comply with the Public Domain and Fundamental Research exclusions of ITAR/EAR. See
[`AGENTS.md`](AGENTS.md) for the project's compliance, attribution, and security policies.
