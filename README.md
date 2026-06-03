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

> **Status:** Roadmap through **Milestone 7** is implemented: M1–M6 as before, plus the
> **`chronus-hil-sim`** NeXosim driver (synthetic CCSDS TM over UDP) with ingest/soak tests and
> profiling notes in [`docs/HIL.md`](docs/HIL.md). See [`BUILD_PLAN.md`](BUILD_PLAN.md).

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
├── deny.toml               cargo-deny policy (CI supply-chain gate)
├── .github/workflows/ci.yml Tests, clippy, audit, deny (checks out Ephemerust sibling)
├── crates/gateway/         The gateway binary + library
│   ├── benches/
│   │   └── parse_validate.rs   Criterion: parse + validate hot paths (M6)
│   ├── src/
│   │   ├── lib.rs          Crate documentation and module wiring
│   │   ├── config.rs       Ingestion + HTTP bind (`IngestConfig`, `StationConfig`)
│   │   ├── ingest.rs       Asynchronous UDP ingestion loop (RawFrame, stats, shutdown)
│   │   ├── ccsds.rs        CCSDS Space Packet parsing (TelemetryFrame, validation)
│   │   ├── validate.rs     Physics–Telemetry Co-Validation (Doppler, elevation, physics_flags)
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
│   └── HIL.md              Manual profiling recipe (gateway metrics)
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
cargo run        # UDP ingest (default 127.0.0.1:7301) + Axum HTTP/WebSocket (default 127.0.0.1:8080)
cargo test       # unit + integration + doctests
cargo bench -p chronus-gateway   # Criterion benchmarks (M6)
cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 2000   # NeXosim HIL (M7); run gateway first
```

See [`docs/HIL.md`](docs/HIL.md) for pairing with `GET /api/v1/chronus/metrics`.

Default bind addresses are loopback-only (`IngestConfig` / `StationConfig` in `config.rs`). Set
`RUST_LOG=debug` for verbose tracing.

### Developer setup notes

- Keep `Ephemerust` checked out beside this repository, with that exact capitalisation, because
  `Cargo.toml` uses `ephemerust = { path = "../Ephemerust" }`.
- Use the stable toolchain at Rust **1.88+**. If the default toolchain is older, run commands as
  `cargo +stable ...` after installing stable with `rustup toolchain install stable`.
- The gateway binary currently uses in-code defaults; there is no environment/config-file loader
  yet. Change defaults through `IngestConfig` and `StationConfig` when embedding the library.
- All examples use loopback and synthetic/public data only. Do not place mission keys, controlled
  frequencies, or operational parameters in the repo.

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Runtime interfaces

The default binary starts two local services:

| Interface | Default | Codepath | Notes |
|-----------|---------|----------|-------|
| UDP telemetry ingest | `127.0.0.1:7301` | `crates/gateway/src/ingest.rs` | Receives one CCSDS TM Space Packet per datagram. The receive buffer is bounded by `IngestConfig::max_datagram_size` (`65_542` bytes). Slow consumers do not block ingestion; the internal broadcast channel is intentionally lossy. |
| HTTP / WebSocket | `127.0.0.1:8080` | `crates/gateway/src/http.rs` | Serves health, metrics, Open MCT stubs, and the real-time WebSocket. |

HTTP routes:

| Route | Response |
|-------|----------|
| `GET /health` | `{"status":"ok"}` |
| `GET /api/v1/chronus/metrics` | Combined ingest counters, gateway counters, and `avg_processing_latency_ms`. |
| `GET /api/v1/chronus/openmct/dictionary` | Stub dictionary point list for future Open MCT wiring. |
| `GET /api/v1/chronus/history` | Stub empty packet list; persistence/history is not implemented yet. |
| `GET /telemetry/openmct` | WebSocket upgrade; one JSON text message per parsed TM frame. |

WebSocket messages use `chronus_schema: "openmct.realtime.v1"` and include:

```json
{
  "chronus_schema": "openmct.realtime.v1",
  "apid": 42,
  "seq_count": 7,
  "received_at": "2020-07-12T12:00:00Z",
  "physics_flags": 0,
  "source": "127.0.0.1:5000",
  "elevation_deg": null,
  "azimuth_deg": null,
  "range_km": null,
  "range_rate_km_s": null,
  "payload_base64": "aGVsbG8="
}
```

The physics fields are populated when the default `EphemerustPropagator` initializes successfully.
Malformed, truncated, or telecommand packets are counted as parse errors and are not sent on the
WebSocket.

`physics_flags` is a stable bitfield from `validate.rs`:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Measured carrier is outside the configured Doppler tolerance. The current binary passes no SDR carrier metadata, so this check is skipped in live UDP runs. |
| 1 | `0x02` | Predicted elevation is below `StationConfig::minimum_elevation_deg` (default `0` degrees). |
| 2 | `0x04` | Reserved for RSSI/link-budget validation; not set yet. |

### Quick manual smoke test

Start the gateway, send synthetic frames, then inspect metrics:

```bash
cargo run -p chronus-gateway
# in another shell
cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 100
curl http://127.0.0.1:8080/api/v1/chronus/metrics
```

Expected result after the simulator run: `ingest.frames_received` reaches the requested frame
count, `ingest.recv_errors` remains `0`, and WebSocket/gateway counters increase only if a
WebSocket client was connected while frames arrived.

### Common pitfalls

- `failed to bind UDP socket` or `failed to bind HTTP`: another process is using `7301` or `8080`;
  change `IngestConfig` for embedded runs or stop the conflicting local service.
- Metrics show ingest frames but no WebSocket messages: connect a client to `/telemetry/openmct`
  before sending frames. The gateway does not replay old frames.
- `telemetry_parse_errors` increases: the UDP payload is not a CCSDS telemetry Space Packet, is
  truncated, or is a telecommand packet on the telemetry path.
- `anomaly_frames` increases during HIL: the default ISS TLE and synthetic ground station may put
  the spacecraft below the configured horizon; this exercises bit 1 of `physics_flags`.
- Cargo cannot find `../Ephemerust`: clone the sibling dependency at the documented path or adjust
  the path dependency locally without committing that change.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, NeXosim HIL tests in
`chronus-hil-sim`, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

The implementation and documentation are grounded in source code and open references:

- [`crates/gateway/src/ingest.rs`](crates/gateway/src/ingest.rs) — UDP ingestion behavior,
  bounded buffer, lossy broadcast, and ingest counters.
- [`crates/gateway/src/ccsds.rs`](crates/gateway/src/ccsds.rs) — CCSDS Space Packet parsing and
  synthetic TM encoding helper.
- [`crates/gateway/src/validate.rs`](crates/gateway/src/validate.rs) — Doppler/elevation
  co-validation model and `physics_flags` contract.
- [`crates/gateway/src/http.rs`](crates/gateway/src/http.rs) — HTTP routes, metrics response, and
  Open MCT WebSocket JSON envelope.
- [`crates/chronus-hil-sim/src/lib.rs`](crates/chronus-hil-sim/src/lib.rs) — NeXosim synthetic
  spacecraft model and UDP bridge.
- [CCSDS Space Packet Protocol](https://public.ccsds.org/) — open standard for the primary header
  parsed by the gateway.
- [NASA Open MCT](https://nasa.github.io/openmct/) — target open-source mission-control dashboard
  shape for the distribution layer.

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
  framework used by `chronus-hil-sim` for hardware-in-the-loop validation.

The broader Rust aerospace ecosystem — including `sat-rs`, `spacepackets`, and `nyx-space` —
informed the design analysis.

---

## License and compliance

Licensed under the MIT License.

This project is designed strictly around open international standards (CCSDS) and is published
openly to comply with the Public Domain and Fundamental Research exclusions of ITAR/EAR. See
[`AGENTS.md`](AGENTS.md) for the project's compliance, attribution, and security policies.
