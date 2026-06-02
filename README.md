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
Raw RF / SDR ──▶ Async UDP ingestion ──▶ CCSDS zero-copy parser ──▶ TrackingProvider
 (UDP now)         (Tokio + broadcast)      (TelemetryFrame)           (throttled cache)
                                                                            │
                  OrbitalPropagator trait ◀── range-rate / look-angles ─────┤
                  (Ephemerust today, nyx-space later)                       ▼
                                                             Physics-Telemetry Co-Validation
                                                             (physics_flags bitfield)
                                                                            │
                                                             M5: Axum WebSocket ──▶ NASA Open MCT
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion and a lossy broadcast
  channel, so slow consumers observe lag instead of stalling the socket loop.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
  `OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
  library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
  clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.
- **Current distribution boundary.** The binary currently runs the M1-M4 local pipeline
  (ingest -> parse -> track -> validate) and logs validated frames. Axum WebSocket / Open MCT
  fan-out is the next milestone, not yet part of the runtime.

The reasoning behind these and other choices is recorded in [`Methodology.md`](Methodology.md).

---

## Repository layout

```
chronus-gateway/
├── Cargo.toml              Workspace manifest (centralized dependency versions, MSRV 1.88)
├── crates/gateway/         The gateway binary + library
│   ├── src/
│   │   ├── lib.rs          Crate documentation and module wiring
│   │   ├── config.rs       Ingestion + station/TLE configuration
│   │   ├── ingest.rs       Asynchronous UDP ingestion loop (RawFrame, stats, shutdown)
│   │   ├── ccsds.rs        CCSDS Space Packet parsing (TelemetryFrame, validation)
│   │   ├── validate.rs     Physics–Telemetry Co-Validation (Doppler, elevation, physics_flags)
│   │   ├── propagator.rs   OrbitalPropagator trait, Ephemerust backend, TrackingProvider cache
│   │   └── main.rs         Entrypoint (runs the M1-M4 local UDP pipeline)
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
cargo run        # listen on 127.0.0.1:7301 and log parsed/validated telemetry
cargo test       # unit + integration + doctests
```

`cargo run` uses the built-in development defaults:

- UDP bind address: `127.0.0.1:7301`
- station/TLE: synthetic development station plus a public ISS TLE from `config.rs`
- validation: elevation gating runs when tracking state is available; Doppler is skipped until
  SDR/front-end RF metadata provides `measured_carrier_hz`
- shutdown: press `Ctrl-C`; final ingest counters are logged

To exercise the parser without live hardware, send the same synthetic CCSDS telemetry packet used
by the unit tests (APID `0x02a`, sequence `7`, payload `hello`) from another terminal:

```bash
printf '\x00\x2A\xC0\x07\x00\x04hello' | nc -u -w1 127.0.0.1 7301
```

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
loopback UDP integration tests, doctests, physics co-validation tests with documented tolerances,
and future in-process WebSocket tests for M5 distribution — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- [`BUILD_PLAN.md`](BUILD_PLAN.md) — milestone scope, gates, and current implementation status.
- [`TEST_PLAN.md`](TEST_PLAN.md) — deterministic test matrix, tolerance register, and
  `physics_flags` contract.
- [`Methodology.md`](Methodology.md) — decision log, trade-offs, and attribution register.
- [CCSDS public standards](https://public.ccsds.org/) — open packet/protocol standards that define
  the gateway's TMTC framing scope.
- [`spacepackets`](https://crates.io/crates/spacepackets) — Rust CCSDS/ECSS packet parsing crate
  wrapped by `crates/gateway/src/ccsds.rs`.
- [Ephemerust](https://github.com/IsomorphicAlgo/ephemerust) — local sibling astrodynamics crate
  used by the default `OrbitalPropagator` backend.

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
- **[`spacepackets`](https://crates.io/crates/spacepackets)** (us-irs) — the CCSDS/ECSS packet
  library used behind the gateway's CCSDS module boundary.
- **[Tokio](https://tokio.rs/)** — the asynchronous runtime that forms the ingestion and task
  execution core.
- **[Axum](https://github.com/tokio-rs/axum)** — the planned WebSocket/HTTP framework for the M5
  Open MCT distribution layer, following patterns from Rusty_Server.
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
