# ChronusGateway-RS

ChronusGateway-RS (Chronus-GS) is an asynchronous, physics-validated Telemetry and Command
(TMTC) ground-station gateway written in Rust. It ingests raw spacecraft downlink frames,
parses them against open CCSDS standards, cross-checks each frame against the spacecraft's
computed orbital physics, and is designed to distribute validated telemetry to web-based mission
control dashboards such as NASA Open MCT — in a single, memory-safe, garbage-collection-free
executable.

Its distinguishing feature is a **Physics-Telemetry Co-Validation** engine: rather than checking
telemetry only against static limits, the gateway uses a live orbital propagator to derive the
expected Doppler shift, look-angles, and link geometry for the spacecraft, and flags frames whose
measured RF and signal parameters disagree with the physics.

> **Status:** Early development. Ingestion (M1), CCSDS parsing (M2), station tracking (M3), and the
> Physics–Telemetry Co-Validation engine (M4) are implemented and tested. Open MCT WebSocket
> distribution is Milestone 5 (see [`BUILD_PLAN.md`](BUILD_PLAN.md)).

---

## Architecture

The gateway is built around two principles: asynchronous, bounded-memory ingestion and a clean
abstraction boundary between the pipeline and any astrodynamics backend.

```
Raw RF / SDR ──▶ Async UDP ingestion ──▶ CCSDS zero-copy parser ──▶ Physics-Telemetry
  (UDP)           (Tokio)                  (validated frames)         Co-Validation engine
                                                                            │
                  OrbitalPropagator trait ◀── range-rate / look-angles ─────┤
                  (Ephemerust today, nyx-space later)                       ▼
                                                       M5 Axum WebSocket ──▶ NASA Open MCT
                                                       (planned)
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion. Frames are broadcast
  on a bounded, lossy channel so slow consumers cannot stall the socket loop. The Axum WebSocket
  distribution layer is planned for Milestone 5, not shipped yet.
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
│   │   ├── config.rs       Ingestion, station, and co-validation configuration
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
cargo run        # run the UDP parse -> track -> validate demo gateway
cargo test       # unit + integration + doctests
```

### Operating the current demo gateway

`cargo run` starts the implemented M1-M4 pipeline with default, developer-safe settings:

- UDP bind address: `127.0.0.1:7301` (`IngestConfig::default()`).
- Broadcast channel capacity: `1024` frames; the channel is intentionally lossy under
  backpressure.
- Maximum datagram size: `65_542` bytes, enough for one maximum-size CCSDS Space Packet plus the
  6-byte primary header while keeping the receive buffer bounded.
- Station defaults: public ISS TLE, latitude `35.0`, longitude `-116.0`, altitude `1000 m`,
  nominal carrier `437.5 MHz`, tracking recompute throttle `10 ms`, Doppler tolerance `150 Hz`,
  and minimum elevation `0 deg`.
- Logging: set `RUST_LOG=debug` (or another `tracing-subscriber` env-filter value) for more detail;
  the default filter is `info`.

To exercise the live path, send a synthetically generated CCSDS telemetry (TM) UDP datagram to
`127.0.0.1:7301`. The binary parses the Space Packet, computes a tracking state when the default
TLE loads, applies physics co-validation, and logs APID, sequence count, payload length,
look-angles, range/range-rate, and `physics_flags`. Invalid or non-telemetry datagrams are logged
and dropped without stopping the loop. Stop with Ctrl-C.

Current constraint: the binary passes `RfMetadata::default()`, so measured-carrier data is absent
and the Doppler check is skipped in the live demo. Elevation validation still runs whenever physics
state is available. Production SDR metadata wiring and WebSocket/Open MCT fan-out are Milestone 5+
work.

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Public library workflow

The crate exposes the pieces needed to build the pipeline in-process:

1. Bind and run UDP ingestion with `ingest::bind`, `ingest::run`, `IngestConfig`, `RawFrame`, and
   `IngestStats`.
2. Parse each `RawFrame` with `ccsds::parse_telemetry`; accepted telemetry becomes a
   zero-copy `TelemetryFrame`.
3. Resolve station/TLE settings with `StationConfig` and compute topocentric state through
   `TrackingProvider` over an `OrbitalPropagator` implementation.
4. Call `apply_physics_validation` with optional `RfMetadata`; downstream consumers read the
   resulting `TelemetryFrame::physics_flags`.

`physics_flags` is a stable bitfield:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Doppler anomaly: measured carrier differs from expected beyond tolerance. |
| 1 | `0x02` | Horizon/elevation anomaly: predicted elevation is below the configured minimum. |
| 2 | `0x04` | Reserved for RSSI/link-budget validation; not set by the current code. |

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP, planned in-process WebSocket tests for the distribution
milestone, doctests, and physics co-validation tests with explicitly documented tolerances —
enforced at every milestone's stage gate. The full strategy and per-milestone test matrix are
defined in [`TEST_PLAN.md`](TEST_PLAN.md).

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
- **[Tokio](https://tokio.rs/)** — the asynchronous runtime that powers the current UDP ingestion
  loop and task orchestration.
- **[Axum](https://github.com/tokio-rs/axum)** — the planned Milestone 5 web framework for the
  Open MCT WebSocket/HTTP distribution layer.
- **[CCSDS](https://public.ccsds.org/)** — the open international standards for space packet
  framing and protocols that define the gateway's wire formats.
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the Rust CCSDS/ECSS packet parsing
  crate wrapped by the gateway's `ccsds` module.
- **[NASA Open MCT](https://nasa.github.io/openmct/)** — the open-source mission-control
  framework targeted by the distribution layer.
- **[NeXosim](https://github.com/asynchronics/nexosim)** — the discrete-event simulation
  framework planned for hardware-in-the-loop validation.

The broader Rust aerospace ecosystem — including `sat-rs` and `nyx-space` — informed the design
analysis.

---

## References

- [`BUILD_PLAN.md`](BUILD_PLAN.md) — stage-gated implementation roadmap and milestone status.
- [`TEST_PLAN.md`](TEST_PLAN.md) — layered test matrix, current test counts, and tolerance register.
- [`Methodology.md`](Methodology.md) — decision log and attribution register.
- [`crates/gateway/src/ingest.rs`](crates/gateway/src/ingest.rs) — UDP ingestion, backpressure, and
  shutdown behavior.
- [`crates/gateway/src/ccsds.rs`](crates/gateway/src/ccsds.rs) — CCSDS Space Packet parsing and
  `TelemetryFrame`.
- [`crates/gateway/src/propagator.rs`](crates/gateway/src/propagator.rs) — `OrbitalPropagator`,
  Ephemerust integration, and tracking-state cache.
- [`crates/gateway/src/validate.rs`](crates/gateway/src/validate.rs) — Doppler/elevation
  co-validation and `physics_flags`.
- CCSDS 133.0-B-2 Space Packet Protocol, via the public CCSDS standards program.
- Ephemerust and its underlying `sgp4` dependency for SGP4 look-angles and range-rate.

---

## License and compliance

Licensed under the MIT License.

This project is designed strictly around open international standards (CCSDS) and is published
openly to comply with the Public Domain and Fundamental Research exclusions of ITAR/EAR. See
[`AGENTS.md`](AGENTS.md) for the project's compliance, attribution, and security policies.
