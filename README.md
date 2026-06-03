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

### Runtime interfaces (MVP defaults)

The current binary wires `IngestConfig::default()` and `StationConfig::default()` directly in
`main.rs`; there is not yet a CLI, environment-variable, or config-file layer for changing these
values at runtime. To use different bind addresses, TLEs, carrier frequencies, or validation
thresholds today, update the config in code or embed `chronus-gateway` as a library and construct
`SharedGateway` yourself.

| Interface | Default | Notes |
|-----------|---------|-------|
| UDP ingest | `127.0.0.1:7301` | CCSDS TM Space Packets; channel capacity `1024`; max datagram `65_542` bytes. |
| HTTP / WebSocket | `127.0.0.1:8080` | Axum server; Ctrl-C triggers graceful shutdown of HTTP + ingest. |
| Station model | public ISS TLE, lat `35.0`, lon `-116.0`, alt `1000 m` | Nominal carrier `437.5 MHz`, Doppler tolerance `150 Hz`, minimum elevation `0°`. |

### HTTP / WebSocket API

| Route | Behavior |
|-------|----------|
| `GET /health` | Returns `{"status":"ok"}` for liveness checks. |
| `GET /telemetry/openmct` | WebSocket upgrade; each valid TM datagram becomes one JSON text message. |
| `GET /api/v1/chronus/metrics` | Combined ingest + gateway counters and average processing latency. |
| `GET /api/v1/chronus/history` | Stub: returns an empty packet list until persistence lands. |
| `GET /api/v1/chronus/openmct/dictionary` | Stub dictionary of Open MCT point identifiers. |

WebSocket messages use the stable schema identifier `openmct.realtime.v1`:

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

Invalid or non-telemetry datagrams are counted as parse errors and skipped rather than sent to
clients. The broadcast fan-out is intentionally lossy: slow WebSocket consumers may miss frames,
but they do not block UDP ingestion. Physics fields are `null` when no tracking state is available.
With the default binary, `physics_flags` currently has live elevation semantics but no measured
carrier input, so Doppler bit 0 is available to callers that provide `RfMetadata`.

| `physics_flags` bit | Mask | Meaning |
|---------------------|------|---------|
| 0 | `0x01` | Doppler anomaly; requires measured carrier metadata (`RfMetadata::measured_carrier_hz`). |
| 1 | `0x02` | Predicted elevation is strictly below `minimum_elevation_deg`. |
| 2 | `0x04` | Reserved for RSSI / link-budget validation; not set yet. |

### Metrics quick reference

`GET /api/v1/chronus/metrics` returns:

```json
{
  "ingest": {
    "frames_received": 0,
    "bytes_received": 0,
    "oversized_dropped": 0,
    "recv_errors": 0
  },
  "gateway": {
    "telemetry_frames_emitted": 0,
    "telemetry_parse_errors": 0,
    "anomaly_frames": 0,
    "ws_messages_sent": 0,
    "processing_latency_ms_sum": 0,
    "processing_latency_ms_count": 0,
    "ws_clients_connected": 0
  },
  "avg_processing_latency_ms": null
}
```

The counters are atomic snapshots and should be treated as operational indicators under
concurrency. `avg_processing_latency_ms` is receive-to-JSON-ready latency and is `null` until a
WebSocket client processes at least one frame.

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, NeXosim HIL tests in
`chronus-hil-sim`, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

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

## References

- CCSDS Space Packet Protocol, CCSDS 133.0-B series — open packet framing used by
  `ccsds::parse_telemetry`.
- [`spacepackets`](https://crates.io/crates/spacepackets) — CCSDS/ECSS packet parsing crate used
  behind the gateway's parser boundary.
- [`Ephemerust`](https://github.com/IsomorphicAlgo/ephemerust) and the underlying
  [`sgp4`](https://crates.io/crates/sgp4) crate — SGP4 propagation, look-angles, and range-rate.
- [NASA Open MCT](https://nasa.github.io/openmct/) — target dashboard shape for the WebSocket
  realtime stream and dictionary identifiers.
- [NeXosim](https://github.com/asynchronics/nexosim) — discrete-event simulation framework used by
  `chronus-hil-sim`.
- [Tokio](https://tokio.rs/) and [Axum](https://github.com/tokio-rs/axum) — async runtime and
  HTTP/WebSocket framework for the gateway core.

---

## License and compliance

Licensed under the MIT License.

This project is designed strictly around open international standards (CCSDS) and is published
openly to comply with the Public Domain and Fundamental Research exclusions of ITAR/EAR. See
[`AGENTS.md`](AGENTS.md) for the project's compliance, attribution, and security policies.
