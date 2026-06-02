# ChronusGateway-RS

ChronusGateway-RS (Chronus-GS) is an asynchronous, physics-validated Telemetry and Command
(TMTC) ground-station gateway written in Rust. It ingests raw spacecraft downlink frames,
parses them against open CCSDS standards, cross-checks each frame against the spacecraft's
computed orbital physics, and distributes validated telemetry to web-based mission control
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
 (UDP/TCP)        (Tokio)                  (validated frames)         Co-Validation engine
                                                                            │
                  OrbitalPropagator trait ◀── range-rate / look-angles ─────┤
                  (Ephemerust today, nyx-space later)                       ▼
                                                          Axum WebSocket ──▶ NASA Open MCT
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion and a pool of
  WebSocket connections, scaling to many concurrent telemetry channels and operator screens.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
  `OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
  library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
  clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.

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
│   │   └── main.rs         Entrypoint (runs the ingestion server)
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
cargo run        # run the developer/demo gateway
cargo test       # unit + integration + doctests
```

If the default toolchain is older than the workspace MSRV, install/use a newer stable toolchain:

```bash
rustup toolchain install stable
cargo +stable test
```

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Current runtime workflow (M1-M4)

The binary is intentionally still a developer/demo gateway; external configuration files and the
Open MCT distribution API arrive in later milestones. Today `cargo run` executes this path:

1. Bind the UDP ingest socket to `127.0.0.1:7301` (`IngestConfig::default`).
2. Forward each datagram as a cheap-clone `RawFrame` over a lossy broadcast channel.
3. Parse telemetry datagrams as CCSDS Space Packets with `ccsds::parse_telemetry`.
4. Compute a station-relative `TrackingState` through the throttled `TrackingProvider` when
   Ephemerust can propagate the configured TLE at the frame timestamp.
5. Apply `validate::apply_physics_validation`, setting `TelemetryFrame::physics_flags`, then log
   the parsed frame. The current binary passes `RfMetadata::default()`, so Doppler validation is
   skipped until measured carrier metadata is wired; elevation validation runs when physics state
   is available.

Send one synthetic CCSDS telemetry packet to the default listener:

```bash
# Terminal 1
RUST_LOG=chronus_gateway=info cargo run -p chronus-gateway

# Terminal 2
python3 - <<'PY'
import socket

# TM packet: APID 0x02a, sequence 7, unsegmented, 5-byte payload "hello".
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

Expected result: the first terminal logs a parsed telemetry frame with `apid=42`, `seq=7`, and
`payload=5`. If physics state is available, the log also includes azimuth/elevation/range,
range-rate, and `physics_flags`.

---

## Public interfaces and current defaults

| Interface | Purpose | Current defaults / contract |
|-----------|---------|-----------------------------|
| `IngestConfig` | UDP bind, channel capacity, datagram ceiling | `127.0.0.1:7301`, capacity `1024`, max datagram `65_542` bytes. |
| `RawFrame` | Raw downlink datagram passed between stages | `Arc<[u8]>` payload, UTC receive timestamp, source address. |
| `TelemetryFrame` | Validated CCSDS TM packet | APID, sequence count, secondary-header flag, zero-copy `payload()`, source/timestamp, `physics_flags`. |
| `StationConfig` | Ground station + tracked object setup | Synthetic/public ISS TLE fixture, lat `35.0`, lon `-116.0`, altitude `1000 m`, nominal carrier `437.5 MHz`, recompute throttle `10 ms`. |
| `TrackingProvider` | Shared propagator front-end | Reuses cached state for frames inside `min_recompute_interval_ms`; `0` disables caching. |
| `apply_physics_validation` | Sets anomaly bits from physics checks | Doppler bit 0 if measured carrier exceeds tolerance; elevation bit 1 if below `minimum_elevation_deg`; RSSI bit 2 reserved. |

`physics_flags` is a stable bitfield for downstream consumers:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Doppler anomaly: measured carrier is outside `doppler_tolerance_hz` from expected. |
| 1 | `0x02` | Horizon/elevation anomaly: predicted elevation is below `minimum_elevation_deg`. |
| 2 | `0x04` | Reserved for future RSSI/link-budget validation; not set by current code. |

---

## Operational notes and common pitfalls

- **No runtime config file yet.** Defaults are constructed in code (`IngestConfig::default` and
  `StationConfig::default`). Library users can build their own configs; the demo binary does not
  yet load CLI flags or TOML.
- **Use only synthetic or public reference data.** The default ISS TLE is a public fixture for
  deterministic development. Use a current public TLE for meaningful physics validation; do not
  commit real mission keys, frequencies, or controlled operational parameters.
- **Backpressure is lossy by design.** Slow broadcast subscribers receive `Lagged`; they must
  resynchronize to the latest telemetry rather than expecting gap-free replay.
- **Oversized UDP datagrams are bounded.** The receive buffer is fixed. Windows reports an
  oversized datagram as `WSAEMSGSIZE`; Unix may truncate it, after which CCSDS length checks reject
  malformed packets.
- **The ingest path accepts telemetry, not telecommands.** CCSDS TC packets are rejected with
  `CcsdsError::NotTelemetry` and dropped by the demo consumer.
- **Doppler needs measured RF metadata.** The library supports `RfMetadata::measured_carrier_hz`;
  the current binary passes `None`, so only the elevation flag can be set during the demo path.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- [`crates/gateway/src/ingest.rs`](crates/gateway/src/ingest.rs) and
  [`crates/gateway/tests/ingest.rs`](crates/gateway/tests/ingest.rs) — source and loopback tests
  for UDP ingestion, shutdown, oversized datagrams, and lossy backpressure.
- [`crates/gateway/src/ccsds.rs`](crates/gateway/src/ccsds.rs) — CCSDS Space Packet parsing and
  `TelemetryFrame` contract.
- [`crates/gateway/src/propagator.rs`](crates/gateway/src/propagator.rs) and
  [`crates/gateway/src/config.rs`](crates/gateway/src/config.rs) — station configuration,
  Ephemerust adapter, and throttled tracking provider.
- [`crates/gateway/src/validate.rs`](crates/gateway/src/validate.rs) — Doppler/elevation
  validation model and `physics_flags` bit definitions.
- [`Methodology.md`](Methodology.md) — decision log, dependency rationale, and attribution record.
- [`TEST_PLAN.md`](TEST_PLAN.md) — test gates, fixture policy, and tolerance register.
- [CCSDS Space Packet Protocol](https://public.ccsds.org/) — open packet standard used for
  telemetry framing.
- [`spacepackets`](https://crates.io/crates/spacepackets) — Rust CCSDS/ECSS packet parsing crate
  used behind the local `ccsds` module.
- [Ephemerust](https://github.com/IsomorphicAlgo/ephemerust) and
  [`sgp4`](https://crates.io/crates/sgp4) — SGP4/look-angle/range-rate sources behind the
  `OrbitalPropagator` implementation.
- [Tokio](https://tokio.rs/) — asynchronous UDP, tasks, signals, and broadcast channels.
- [NASA Open MCT](https://nasa.github.io/openmct/) — planned dashboard target for Milestone 5.

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
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the Rust CCSDS/ECSS packet
  library used behind the gateway's local `ccsds` parsing boundary.
- **[Tokio](https://tokio.rs/)** and **[Axum](https://github.com/tokio-rs/axum)** — the
  asynchronous runtime and web framework that form the network core.
- **[CCSDS](https://public.ccsds.org/)** — the open international standards for space packet
  framing and protocols that define the gateway's wire formats.
- **[NASA Open MCT](https://nasa.github.io/openmct/)** — the open-source mission-control
  framework targeted by the distribution layer.
- **[NeXosim](https://github.com/asynchronics/nexosim)** — the discrete-event simulation
  framework planned for hardware-in-the-loop validation.

The broader Rust aerospace ecosystem — including `sat-rs` and `nyx-space` — informed the design
analysis.

---

## License and compliance

Licensed under the MIT License.

This project is designed strictly around open international standards (CCSDS) and is published
openly to comply with the Public Domain and Fundamental Research exclusions of ITAR/EAR. See
[`AGENTS.md`](AGENTS.md) for the project's compliance, attribution, and security policies.
