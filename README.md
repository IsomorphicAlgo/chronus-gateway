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

> **Status:** Roadmap through **Milestone 8** is implemented: M1–M7 as before, plus optional **TOML**
> configuration (`--config` / `CHRONUS_GATEWAY_CONFIG`, `[gateway.example.toml](gateway.example.toml)`).
> NeXosim HIL notes: `[docs/HIL.md](docs/HIL.md)`.
> **User guide (intro + first run + alarms):** `[docs/USER_GUIDE.md](docs/USER_GUIDE.md)` — grows with the plan files below.
> Post-M8 **extended co-validation** (`[docs/EXTENDED_COVALIDATION_PLAN.md](docs/EXTENDED_COVALIDATION_PLAN.md)`): **CV-1…CV-5** implemented; **Gate CV-5** pending owner sign-off. See `[docs/BUILD_PLAN.md](docs/BUILD_PLAN.md)`.

---

## Architecture

The gateway is built around two principles: an asynchronous, lock-free network core and a clean
abstraction boundary between the pipeline and any astrodynamics backend.

```
Raw RF / SDR ──▶ Async UDP ingestion ──▶ CCSDS zero-copy parser ──▶ Physics-Telemetry
 (UDP/TCP)        (Tokio)                  (validated frames)         Co-Validation engine
                                                                             │
                  OrbitalPropagator trait ◀── range-rate / look-angles ──=──┤
                  (Ephemerust today, nyx-space later)                       ▼
                                                          Axum WebSocket ──▶ NASA Open MCT
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion and a pool of
WebSocket connections, scaling to many concurrent telemetry channels and operator screens.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
`OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.

The reasoning behind these and other choices is recorded in `[Methodology.md](Methodology.md)`.

---

## Repository layout

```
chronus-gateway/
├── Cargo.toml              Workspace manifest (centralized dependency versions, MSRV 1.89)
├── deny.toml               cargo-deny policy (CI supply-chain gate)
├── gateway.example.toml    Example TOML for `chronus-gateway --config` (M8)
├── .github/workflows/ci.yml Tests, clippy, audit, deny (checks out Ephemerust sibling)
├── crates/gateway/         The gateway binary + library
│   ├── benches/
│   │   └── parse_validate.rs   Criterion: parse + validate hot paths (M6)
│   ├── src/
│   │   ├── lib.rs          Crate documentation and module wiring
│   │   ├── config/         Ingest + station types; TOML file loader (`file.rs`, M8)
│   │   ├── ingest.rs       Asynchronous UDP ingestion loop (RawFrame, stats, shutdown)
│   │   ├── ccsds.rs        CCSDS Space Packet parsing (TelemetryFrame, validation)
│   │   ├── validate.rs     Physics–Telemetry Co-Validation (Doppler, elevation, link budget, `physics_flags`)
│   │   ├── propagator.rs   OrbitalPropagator trait + Ephemerust-backed implementation
│   │   ├── http.rs         Axum router: `/health`, metrics, Open MCT WebSocket
│   │   ├── metrics.rs      Gateway / WebSocket counters (M6)
│   │   ├── state.rs        Shared Axum + ingest state (`SharedGateway`)
│   │   └── main.rs         Entrypoint: UDP ingest + Axum HTTP/WebSocket (Ctrl-C shutdown)
│   └── tests/
│       ├── ingest.rs       Milestone 1 integration tests (UDP loop)
│       └── distribution.rs Milestone 5 (HTTP health + WebSocket JSON)
├── crates/chronus-hil-sim/ NeXosim HIL: synthetic spacecraft → UDP (`chronus-hil-sim` binary)
│   ├── src/lib.rs          `SpacecraftDemo` + UDP bridge + `run_nexosim_udp_hil`
│   ├── src/main.rs        CLI: `[DEST] [FRAMES]` (default `127.0.0.1:7301`, `100`)
│   └── tests/hil_ingest.rs Milestone 7 smoke + soak vs real `ingest::run`
├── docs/
│   ├── USER_GUIDE.md       Operator guide (intro, first run, `physics_flags`; grows with plans)
│   ├── BUILD_PLAN.md       Iterative, stage-gated implementation roadmap
│   ├── SHOWCASE_PLAN.md    Owner-gated demo/showcase stages (S0–S4; Docker, dashboard, replay)
│   ├── Demo_Test.md        Manual acceptance for showcase gates (companion to SHOWCASE_PLAN)
│   ├── EXTENDED_COVALIDATION_PLAN.md  Post-M8 co-validation milestones (CV-0…CV-5)
│   └── HIL.md              Manual profiling recipe (gateway metrics)
├── Methodology.md          Decision log: the reasoning behind major choices
└── TEST_PLAN.md            Companion test plan and tolerance register
```

---

## Building and running

The project targets Rust 1.89 or newer and consumes the Ephemerust library as a sibling
checkout. The expected on-disk layout places both repositories next to each other:

```
…/Rust/
├── chronus-gateway/
└── Ephemerust/
```

```bash
cargo build      # compile the workspace
cargo run -p chronus-gateway    # UDP ingest (127.0.0.1:7301) + HTTP/WebSocket (127.0.0.1:8080)
cargo run -p chronus-gateway -- --config gateway.example.toml   # optional TOML (M8)
cargo test       # unit + integration + doctests
cargo bench -p chronus-gateway   # Criterion benchmarks (M6)
cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 2000   # NeXosim HIL (M7); run gateway first
```

See `[docs/HIL.md](docs/HIL.md)` for pairing with `GET /api/v1/chronus/metrics`.

Default bind addresses are loopback-only (`IngestConfig` / `StationConfig` in `config`). Set
`RUST_LOG=debug` for verbose tracing. Settings can be overridden with TOML (`--config` / `-c`, or
`CHRONUS_GATEWAY_CONFIG`; CLI wins when both are set) — see `[gateway.example.toml](gateway.example.toml)`.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, NeXosim HIL tests in
`chronus-hil-sim`, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The roadmap lives in `[docs/BUILD_PLAN.md](docs/BUILD_PLAN.md)`; the full strategy and
per-milestone test matrix are in `[TEST_PLAN.md](TEST_PLAN.md)`.

---

## Acknowledgements

ChronusGateway-RS builds directly on prior work, and credit is given accordingly:

- **[Ephemerust](https://github.com/IsomorphicAlgo/ephemerust)** — the SGP4-based orbital
mechanics and satellite-tracking library that provides the look-angle and range-rate
computations underpinning the co-validation engine. Authored by the same maintainer.
- **Rusty_Server** — an earlier asynchronous networking and REST service by the same maintainer,
whose Tokio/Axum architecture and integration patterns informed this gateway's design.
- `**[sgp4](https://crates.io/crates/sgp4)`** — the validated SGP4/SDP4 propagator that
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
[`Methodology.md`](Methodology.md) and this README for compliance, attribution, and security expectations.