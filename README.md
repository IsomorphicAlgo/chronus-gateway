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
cargo run        # run the current loopback ingestion demo
cargo test       # unit + integration + doctests
```

`cargo run` starts the current gateway pipeline on `127.0.0.1:7301`:

1. bind the UDP ingestion socket,
2. broadcast each datagram as a cheap-clone `RawFrame`,
3. parse CCSDS telemetry packets into `TelemetryFrame`,
4. compute a station-relative `TrackingState`, and
5. set `physics_flags` for implemented co-validation checks.

The runtime configuration is still code-level (no external config file or CLI flags yet). Defaults
live in `IngestConfig::default()` and `StationConfig::default()`:

| Setting | Default | Notes |
|---------|---------|-------|
| UDP bind address | `127.0.0.1:7301` | Loopback-only for development. |
| Broadcast capacity | `1024` frames | Lossy by design; slow consumers see lag instead of blocking ingest. |
| Max datagram size | `65_542` bytes | Fixed receive buffer; covers one maximum CCSDS Space Packet. |
| Station | `35.0°, -116.0°, 1000 m` | Synthetic development station, not an operational site. |
| TLE | Public ISS (ZARYA) reference TLE | Development fixture; replace for meaningful current geometry. |
| Nominal carrier | `437_500_000 Hz` | Synthetic/default value used by Doppler arithmetic. |
| Doppler tolerance | `150 Hz` | See `TEST_PLAN.md` T-DOPPLER and `Methodology.md` D-012. |
| Minimum elevation | `0°` | Frames below the mathematical horizon set bit 1. |

Synthetic CCSDS telemetry can be sent with only the Python standard library:

```bash
python3 - <<'PY'
import socket

# CCSDS TM packet: APID 0x02A, unsegmented sequence 7, payload "hello".
packet = bytes([
    0x00, 0x2A,  # version/type/sec-header/APID
    0xC0, 0x07,  # sequence flags + sequence count
    0x00, 0x04,  # data length = payload_len - 1
]) + b"hello"

socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(packet, ("127.0.0.1", 7301))
PY
```

When the default demo path is running, valid packets are logged with APID, sequence count, payload
length, tracking state, and `physics_flags`. `RfMetadata::measured_carrier_hz` is not wired to the
UDP ingest path yet, so the Doppler bit is skipped in `main`; elevation validation still runs when
tracking state is available.

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Implemented workflow and public interfaces

The implemented M1-M4 path is intentionally small and explicit:

```
UDP datagram
  └─ ingest::run → RawFrame
      └─ ccsds::parse_telemetry → TelemetryFrame
          └─ TrackingProvider::tracking_state → TrackingState
              └─ validate::apply_physics_validation → physics_flags
```

Key public types and contracts:

| Module | Interface | Contract |
|--------|-----------|----------|
| `config` | `IngestConfig` | UDP bind address, lossy channel capacity, and fixed receive-buffer size. |
| `config` | `StationConfig`, `TleSource` | Validates station coordinates, carrier, thresholds, and inline/file TLE sources before propagation. |
| `ingest` | `RawFrame`, `IngestStats`, `bind`, `run` | Non-blocking UDP receive loop with explicit counters and shutdown future. |
| `ccsds` | `parse_telemetry`, `TelemetryFrame`, `CcsdsError` | Parses CCSDS Space Packet primary headers, rejects TC packets on the TM path, and exposes payload zero-copy. |
| `propagator` | `OrbitalPropagator`, `EphemerustPropagator`, `TrackingProvider` | Keeps astrodynamics behind a trait and throttles repeated look-angle recomputation. |
| `validate` | `apply_physics_validation`, `RfMetadata`, `FLAG_*` | Clears and sets stable anomaly bits for Doppler and elevation checks. |

`physics_flags` is the downstream anomaly contract:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Measured carrier differs from expected Doppler-shifted carrier beyond tolerance. |
| 1 | `0x02` | Propagated elevation is below the configured minimum elevation. |
| 2 | `0x04` | Reserved for future RSSI / link-budget validation; not set today. |

---

## Operations and troubleshooting

For local development:

1. Keep `../Ephemerust` checked out next to this repository.
2. Run `cargo test` before and after behavior changes.
3. Run `RUST_LOG=info cargo run` to start the loopback ingest demo.
4. Send synthetic CCSDS TM packets to `127.0.0.1:7301`.
5. Stop with `Ctrl-C`; final ingest counters are logged on shutdown.

Common pitfalls:

| Symptom | Likely cause | Resolution |
|---------|--------------|------------|
| `failed to bind UDP socket` | Port `7301` already in use or unavailable. | Change `IngestConfig::bind_addr` in code for now; external config is pending. |
| `no orbital propagator; running without physics state` | Invalid station fields, unreadable TLE file, or unparsable TLE. | Call `StationConfig::validate()` / `resolve_tle_text()` in tests and use public, non-operational TLE fixtures. |
| `dropping invalid/non-telemetry datagram` | Packet is too short, truncated, malformed, or TC rather than TM. | Verify the 6-byte CCSDS primary header and data-length field (`payload_len - 1`). |
| `consumer lagged; dropped frames` | Broadcast receiver fell behind the bounded lossy channel. | Increase `channel_capacity` or make the consumer faster; ingest intentionally preserves freshest data. |
| Doppler flag never appears in `cargo run` logs | Main path uses `RfMetadata::default()` until SDR metadata is wired. | Exercise Doppler with `apply_physics_validation` tests or future RF metadata integration. |
| Below-horizon frames flagged with default demo data | The default station/TLE is a public development fixture, not a current pass plan. | Use a current public TLE and appropriate station values for meaningful geometry. |

Keep all examples synthetic, public, and non-operational per `AGENTS.md`.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

These sources define or substantiate current behavior:

- **CCSDS 133.0-B-2, Space Packet Protocol** — basis for the primary-header fields parsed in
  `ccsds.rs`; only open, international-standard packet structure is used.
- **`spacepackets` crate documentation** — Rust parser used for CCSDS primary-header decoding
  behind the local `ccsds` module boundary.
- **Ephemerust documentation and tests** — source of `look_angles`, range-rate sign convention,
  and numerical tolerance style used by the propagator seam and co-validation tests.
- **`sgp4` crate** — SGP4/SDP4 numerical propagation used indirectly through Ephemerust.
- **Tokio documentation** — async UDP socket, broadcast channel, and shutdown primitives used by
  the ingestion loop.
- **Axum and NASA Open MCT documentation** — planned distribution target for Milestone 5.
- **Project governance docs** — `AGENTS.md`, `Methodology.md`, `BUILD_PLAN.md`, and
  `TEST_PLAN.md` are the local source of truth for compliance, decisions, milestones, and gates.

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
- **[Tokio](https://tokio.rs/)** and **[Axum](https://github.com/tokio-rs/axum)** — the
  asynchronous runtime and web framework that form the network core.
- **[CCSDS](https://public.ccsds.org/)** — the open international standards for space packet
  framing and protocols that define the gateway's wire formats.
- **[NASA Open MCT](https://nasa.github.io/openmct/)** — the open-source mission-control
  framework targeted by the distribution layer.
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
