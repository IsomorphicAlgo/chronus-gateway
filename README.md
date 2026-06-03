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
│   └── HIL.md              NeXosim HIL runbook + manual profiling recipe
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

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Runtime interface and operations

The default binary is intentionally local-only: UDP telemetry on `127.0.0.1:7301`, HTTP/WebSocket
on `127.0.0.1:8080`, and a public ISS reference TLE from `StationConfig::default()`. Alternate
addresses or station parameters currently require constructing `IngestConfig` / `StationConfig` in
library code (the binary has no CLI config parser yet).

### HTTP and WebSocket endpoints

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/health` | `GET` | Liveness check; returns `{"status":"ok"}`. |
| `/telemetry/openmct` | WebSocket | Real-time stream; each valid CCSDS TM datagram becomes one JSON text message. |
| `/api/v1/chronus/metrics` | `GET` | Combined UDP ingest counters, gateway counters, and average processing latency. |
| `/api/v1/chronus/openmct/dictionary` | `GET` | Stub Open MCT point dictionary; documents current point identifiers. |
| `/api/v1/chronus/history` | `GET` | Stub history endpoint; returns an empty packet list until persistence exists. |

`/telemetry/openmct` emits `OpenMctRealtimeMessageV1`:

```json
{
  "chronus_schema": "openmct.realtime.v1",
  "apid": 1968,
  "seq_count": 7,
  "received_at": "2026-06-03T06:00:00Z",
  "physics_flags": 0,
  "source": "127.0.0.1:54000",
  "elevation_deg": 12.3,
  "azimuth_deg": 250.1,
  "range_km": 900.0,
  "range_rate_km_s": -2.1,
  "payload_base64": "AAAA..."
}
```

Physics fields are `null` when no tracking provider is configured or the propagator could not be
initialized. `payload_base64` is the CCSDS packet data field (secondary header plus user data, if
present), not the full primary header. `physics_flags == 0` means no anomaly detected; bit `0` is
Doppler, bit `1` is below-horizon, and bit `2` is reserved for future RSSI/link-budget validation.

### Metrics and troubleshooting

Useful local checks once the gateway is running:

```bash
curl http://127.0.0.1:8080/health
curl http://127.0.0.1:8080/api/v1/chronus/metrics
curl http://127.0.0.1:8080/api/v1/chronus/openmct/dictionary
```

- `ingest.frames_received` increases for every datagram accepted by the UDP loop, even if no
  WebSocket client is connected.
- `gateway.telemetry_frames_emitted` and `gateway.ws_messages_sent` increase only when a WebSocket
  subscriber is connected and a datagram parses as telemetry.
- `gateway.telemetry_parse_errors` means the distribution path observed a datagram that failed
  CCSDS TM parsing (for example, short, truncated, malformed, or TC-routed packets).
- `ingest.oversized_dropped` is expected only on platforms that report oversized UDP datagrams as
  receive errors; Unix-like systems may truncate first, after which CCSDS length validation rejects
  the partial packet.
- HIL packets from `chronus-hil-sim` use synthetic APID `0x7B0` by default and a 16-byte payload:
  `u32 seq`, `f32 eps_bus_voltage_v`, `f32 thermal_panel_c`, `f32 body_rate_deg_s`, all big-endian.

Common local pitfalls:

- Keep the sibling `../Ephemerust` checkout present; the workspace depends on it by path.
- Start the gateway before running `chronus-hil-sim` if you want metrics or WebSocket emission.
- Connect WebSocket clients before sending test datagrams; the broadcast channel is lossy and does
  not replay historical telemetry.
- The history and dictionary routes are intentionally stubs in the current roadmap; use the
  WebSocket stream for real-time telemetry.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, NeXosim HIL tests in
`chronus-hil-sim`, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- **CCSDS Space Packet Protocol (CCSDS 133.0-B series)** — packet framing model used by
  `ccsds::parse_telemetry` and `encode_synthetic_tm`.
- **Ephemerust** — sibling astrodynamics crate used by `EphemerustPropagator` for SGP4
  propagation, look-angles, range, and range-rate.
- **`spacepackets` crate** — CCSDS primary-header parsing dependency wrapped by the gateway's
  `ccsds` module.
- **Tokio / Axum / Tower-HTTP** — async runtime and HTTP/WebSocket stack used by ingestion and
  distribution.
- **NASA Open MCT** — target dashboard integration style for the real-time WebSocket JSON
  contract.
- **NeXosim** — discrete-event simulation framework used by `chronus-hil-sim` for synthetic HIL
  telemetry.

Implementation rationale, dependency roles, and source/license attribution are maintained in
[`Methodology.md`](Methodology.md).

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
  framework used for the synthetic hardware-in-the-loop driver.

The broader Rust aerospace ecosystem — including `sat-rs`, `spacepackets`, and `nyx-space` —
informed the design analysis.

---

## License and compliance

Licensed under the MIT License.

This project is designed strictly around open international standards (CCSDS) and is published
openly to comply with the Public Domain and Fundamental Research exclusions of ITAR/EAR. See
[`AGENTS.md`](AGENTS.md) for the project's compliance, attribution, and security policies.
