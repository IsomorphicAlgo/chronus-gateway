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

> **Status:** Early development. The astrodynamics seam, the asynchronous UDP ingestion loop
> (Milestone 1), CCSDS Space Packet parsing (Milestone 2), and station-configured orbital tracking
> (Milestone 3) are implemented and tested; the co-validation engine and the Open MCT distribution
> layer are tracked as gated milestones in [`BUILD_PLAN.md`](BUILD_PLAN.md).

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
│   │   ├── propagator.rs   OrbitalPropagator trait + Ephemerust-backed implementation
│   │   └── main.rs         Entrypoint (runs the ingestion server)
│   └── tests/
│       └── ingest.rs       Milestone 1 integration tests
├── AGENTS.md               Project constitution (compliance, attribution, security, testing)
├── Methodology.md          Decision log: the reasoning behind major choices
├── RUNBOOK.md              Runtime guide: public interfaces, workflow, troubleshooting
├── BUILD_PLAN.md           Iterative, stage-gated implementation roadmap
└── TEST_PLAN.md            Companion test plan and tolerance register
```

---

## Setup

The project targets Rust 1.88 or newer and consumes the Ephemerust library as a sibling
checkout. The expected on-disk layout places both repositories next to each other:

```
…/Rust/
├── chronus-gateway/
└── Ephemerust/
```

If Cargo reports that `../Ephemerust` is missing, clone the public Ephemerust repository next to
this checkout (not inside it) and rerun the command. Keep any local credentials, station-specific
settings, or private mission data outside the repository; `.env`, `*.local.toml`, and
`credentials.txt` are ignored on purpose.

```bash
cargo build                 # compile the workspace
cargo test                  # unit + integration + doctests
cargo clippy --all-targets  # lint all targets
```

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Running the current gateway

The current binary runs the implemented M1-M3 path:

1. bind a UDP downlink socket from `IngestConfig` (default `127.0.0.1:7301`);
2. broadcast each datagram as a `RawFrame`;
3. parse CCSDS telemetry packets into `TelemetryFrame`;
4. compute a throttled `TrackingState` from `StationConfig` and the Ephemerust backend;
5. log accepted telemetry or recoverable parse/drop reasons.

```bash
RUST_LOG=info cargo run -p chronus-gateway
```

In another terminal, send a synthetic CCSDS telemetry packet over loopback:

```bash
python3 - <<'PY'
import socket

# TM packet, APID 0x02A, sequence 7, 5-byte packet data field "hello".
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

You should see a `telemetry frame parsed` log with APID, sequence count, payload length, and (when
the default public ISS TLE resolves) azimuth/elevation/range/range-rate. Stop the process with
Ctrl-C. The co-validation flags are reserved for Milestone 4, and the Open MCT WebSocket/API layer
is reserved for Milestone 5.

For operational details, public interfaces, and troubleshooting, see [`RUNBOOK.md`](RUNBOOK.md).

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- [`AGENTS.md`](AGENTS.md) — project constitution: compliance, attribution, security, and testing
  requirements.
- [`BUILD_PLAN.md`](BUILD_PLAN.md) and [`TEST_PLAN.md`](TEST_PLAN.md) — stage-gated roadmap,
  milestone status, test gates, and tolerance register.
- [`Methodology.md`](Methodology.md) — decision log for language, workspace layout, async runtime,
  CCSDS parser choice, Ephemerust integration, and station/tracking configuration.
- [`RUNBOOK.md`](RUNBOOK.md) — source-backed operational guide for the implemented UDP ingestion,
  CCSDS parser, and tracking provider path.
- [`crates/gateway/src/ingest.rs`](crates/gateway/src/ingest.rs),
  [`crates/gateway/src/ccsds.rs`](crates/gateway/src/ccsds.rs),
  [`crates/gateway/src/config.rs`](crates/gateway/src/config.rs), and
  [`crates/gateway/src/propagator.rs`](crates/gateway/src/propagator.rs) — current public
  interfaces and constraints.
- [CCSDS](https://public.ccsds.org/) — open space packet and TMTC standards used for the wire
  format boundary.
- [`spacepackets`](https://crates.io/crates/spacepackets) — Rust CCSDS/ECSS packet parsing crate
  used behind the local `ccsds` module.
- [Ephemerust](https://github.com/IsomorphicAlgo/ephemerust) — sibling SGP4/look-angle backend.
- [Tokio](https://tokio.rs/) — asynchronous runtime used for UDP ingestion and planned fan-out.

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
