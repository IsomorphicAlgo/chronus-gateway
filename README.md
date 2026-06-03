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

The gateway is built around two principles: an asynchronous, bounded-memory network core and a
clean abstraction boundary between the pipeline and any astrodynamics backend.

```
Synthetic HIL / SDR UDP ──▶ Async UDP ingest ──▶ RawFrame broadcast ──▶ Axum WebSocket
      (loopback by default)       (Tokio)              (lossy)               │
                                                                              ▼
                                      CCSDS parser ──▶ Physics-Telemetry ──▶ JSON text frames
                                                        Co-Validation              │
                                                             ▲                     ▼
                  OrbitalPropagator trait ◀── range-rate / look-angles ─── NASA Open MCT
                  (Ephemerust today, nyx-space later)
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion and a pool of
  WebSocket connections. UDP datagrams are forwarded as `RawFrame` values over a bounded,
  intentionally lossy broadcast channel so slow consumers cannot stall the socket.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
  `OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
  library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
  clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.
- **Distribution-time validation.** The ingest loop fans out raw datagrams. CCSDS parsing,
  physics validation, metrics updates, and Open MCT JSON serialization happen on the WebSocket
  subscriber path in `crates/gateway/src/http.rs`.

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

### Runtime interfaces

The default binary has no CLI or config file yet; it starts with `IngestConfig::default()` and
`StationConfig::default()`.

| Surface | Default / path | Purpose and constraints |
|---------|----------------|-------------------------|
| UDP ingest | `127.0.0.1:7301` | Raw CCSDS TM datagrams only. Datagrams are bounded by `max_datagram_size = 65542` bytes and forwarded over a lossy broadcast channel. |
| HTTP health | `GET /health` | Liveness JSON: `{"status":"ok"}`. |
| Real-time telemetry | `GET /telemetry/openmct` | WebSocket upgrade. Each valid TM frame becomes one text JSON message with `chronus_schema: "openmct.realtime.v1"`. |
| Metrics | `GET /api/v1/chronus/metrics` | Combined ingest counters, WebSocket/telemetry counters, and average processing latency. |
| History | `GET /api/v1/chronus/history` | Stub: returns an empty packet list; persistence is not implemented. |
| Dictionary | `GET /api/v1/chronus/openmct/dictionary` | Stub point identifiers for Open MCT adapter work. |

Example WebSocket payload:

```json
{
  "chronus_schema": "openmct.realtime.v1",
  "apid": 42,
  "seq_count": 7,
  "received_at": "2026-06-03T00:00:00Z",
  "physics_flags": 0,
  "source": "127.0.0.1:50000",
  "elevation_deg": null,
  "azimuth_deg": null,
  "range_km": null,
  "range_rate_km_s": null,
  "payload_base64": "aGVsbG8="
}
```

`payload_base64` is the CCSDS packet data field (secondary header, if present, plus user data).
Physics geometry fields are `null` if tracking is unavailable. On the live WebSocket path,
`RfMetadata::measured_carrier_hz` is not wired yet, so Doppler bit 0 is skipped; elevation bit 1
can still be set when tracking is available and predicted elevation is below the configured
minimum.

`physics_flags` is a stable bitfield:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Doppler anomaly (`measured` vs expected carrier exceeds tolerance); requires RF metadata. |
| 1 | `0x02` | Predicted elevation is strictly below `minimum_elevation_deg`. |
| 2 | `0x04` | Reserved for RSSI / link-budget validation; not set today. |

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

## References

- `crates/gateway/src/http.rs` — Axum routes, Open MCT WebSocket JSON contract, and metrics JSON.
- `crates/gateway/src/ingest.rs` — UDP ingest loop, bounded datagram handling, and ingest counters.
- `crates/gateway/src/ccsds.rs` — CCSDS Space Packet parsing and synthetic TM encoder.
- `crates/gateway/src/validate.rs` — `physics_flags`, Doppler formula, and validation constraints.
- `crates/chronus-hil-sim/src/lib.rs` — NeXosim HIL model and UDP bridge.
- [`Methodology.md`](Methodology.md) — decision log and attribution register.
- [CCSDS public standards](https://public.ccsds.org/) and [NASA Open MCT](https://nasa.github.io/openmct/)
  for the open packet and dashboard integration targets.

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
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the CCSDS Space Packet parser used
  behind the gateway's `ccsds` module boundary.
- **[Tokio](https://tokio.rs/)** and **[Axum](https://github.com/tokio-rs/axum)** — the
  asynchronous runtime and web framework that form the network core.
- **[`base64`](https://crates.io/crates/base64)** — encodes CCSDS packet data fields for the
  Open MCT JSON stream.
- **[CCSDS](https://public.ccsds.org/)** — the open international standards for space packet
  framing and protocols that define the gateway's wire formats.
- **[NASA Open MCT](https://nasa.github.io/openmct/)** — the open-source mission-control
  framework targeted by the distribution layer.
- **[NeXosim](https://github.com/asynchronics/nexosim)** — the discrete-event simulation
  framework used by `chronus-hil-sim` for synthetic hardware-in-the-loop validation.

The broader Rust aerospace ecosystem — including `sat-rs` and `nyx-space` — informed the design
analysis.

---

## License and compliance

Licensed under the MIT License.

This project is designed strictly around open international standards (CCSDS) and is published
openly to comply with the Public Domain and Fundamental Research exclusions of ITAR/EAR. See
[`AGENTS.md`](AGENTS.md) for the project's compliance, attribution, and security policies.
