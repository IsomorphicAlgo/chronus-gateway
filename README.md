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

The gateway is built around two principles: an asynchronous, lock-free network core and a clean
abstraction boundary between the pipeline and any astrodynamics backend.

```
Raw RF / SDR ──▶ Async UDP ingestion ──▶ CCSDS zero-copy parser ──▶ Physics-Telemetry
 (UDP today)       (Tokio)                  (validated frames)         Co-Validation engine
                                                                            │
                  OrbitalPropagator trait ◀── range-rate / look-angles ─────┤
                  (Ephemerust today, nyx-space later)                       ▼
                                               planned M5: Axum WebSocket ──▶ NASA Open MCT
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion today. The WebSocket
  distribution layer that fans out validated telemetry to operator screens is planned for M5.
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
│   │   └── main.rs         Entrypoint (runs ingest → parse → track → validate → log)
│   └── tests/
│       └── ingest.rs       Milestone 1 loopback UDP integration tests
├── AGENTS.md               Project constitution (compliance, attribution, security, testing)
├── Methodology.md          Decision log: the reasoning behind major choices
├── BUILD_PLAN.md           Iterative, stage-gated implementation roadmap
└── TEST_PLAN.md            Companion test plan and tolerance register
```

Unit tests live inline in each `src/*.rs` module; `tests/ingest.rs` covers the loopback UDP
integration path.

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
cargo run        # start UDP ingest on 127.0.0.1:7301
cargo test       # unit + integration + doctests
```

`cargo run` starts the M1-M4 demonstration pipeline: UDP ingest, CCSDS telemetry parsing, station
tracking, physics co-validation, and structured logs. Stop it with Ctrl-C. Defaults are hardcoded in
`IngestConfig::default()` and `StationConfig::default()`; there is no CLI, config file, or
environment-variable config surface yet. `RUST_LOG` controls tracing verbosity and falls back to
`info`.

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Operational runbook

### Prerequisites

- Rust 1.88 or newer (`rust-version` is declared in the workspace manifest).
- A sibling checkout of [Ephemerust](https://github.com/IsomorphicAlgo/ephemerust) at
  `../Ephemerust` relative to this repo.
- Synthetic/public CCSDS examples only. Do not use mission keys, controlled operational
  parameters, or export-controlled technical data.

### Send a synthetic telemetry packet

In one terminal:

```bash
RUST_LOG=info cargo run
```

In another terminal, send a valid CCSDS TM packet (APID `0x02a`, sequence `7`, payload `hello`) to
the default UDP socket:

```bash
python3 - <<'PY'
import socket

packet = bytes.fromhex("002a c007 0004 68656c6c6f")
socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(packet, ("127.0.0.1", 7301))
PY
```

Expected log shape:

```text
telemetry frame parsed apid=42 seq=7 payload=5 ... physics_flags=<0 or 2>
```

`physics_flags` may be `2` with the default ISS/station geometry because the elevation gate flags
below-horizon passes. Doppler is skipped in the current binary because `main` uses
`RfMetadata::default()` until measured carrier metadata is wired in a later milestone.

### Defaults reference

| Setting | Default | Notes |
|---------|---------|-------|
| UDP bind | `127.0.0.1:7301` | Loopback-only for development; deployments usually choose a NIC or `0.0.0.0`. |
| Max datagram | `65_542` bytes | CCSDS primary header plus max packet data field; fixes receive-buffer size. |
| Broadcast capacity | `1024` frames | Lossy: slow subscribers observe lag instead of blocking ingest. |
| Station | lat `35.0`, lon `-116.0`, alt `1000 m` | Synthetic/demo observer only. |
| TLE | Public ISS (ZARYA) inline TLE | Public reference data; replace only with non-controlled data. |
| Nominal carrier | `437_500_000 Hz` | Used by Doppler validation when RF metadata is present. |
| Doppler tolerance | `150 Hz` | See `TEST_PLAN.md` T-DOPPLER and `Methodology.md` D-012. |
| Minimum elevation | `0 deg` | Strict `<` threshold sets `FLAG_BELOW_HORIZON`. |
| Tracking throttle | `10 ms` | Reuses tracking state for frames within the throttle window. |

### Troubleshooting

- `failed to load source for dependency ephemerust`: clone Ephemerust as the documented sibling
  checkout (`../Ephemerust`) or update the path dependency intentionally.
- `no orbital propagator; running without physics state`: inspect the configured TLE and station
  values; the gateway will still ingest and parse frames.
- `dropping invalid/non-telemetry datagram`: the datagram was too short, malformed, truncated, or
  a TC packet on the telemetry path.
- `consumer lagged; dropped frames`: a downstream subscriber fell behind the lossy broadcast
  channel; newest telemetry is favored over stale delivery.
- Windows `LNK1104` / "Access is denied": check `.cargo/config.toml` and `Methodology.md` D-008
  before changing linker configuration.

---

## Public interface quick reference

The library crate exposes these module surfaces and re-exports the most-used extension points from
`lib.rs` for future adapters and tests:

| Module | Public surface |
|--------|----------------|
| `ingest` | `RawFrame`, `IngestStats`, `bind`, `run` |
| `ccsds` | `TelemetryFrame`, `CcsdsError`, `parse_telemetry`, `CCSDS_PRIMARY_HEADER_LEN` |
| `config` | `IngestConfig`, `StationConfig`, `TleSource`, `ConfigError`, `DEFAULT_ISS_TLE` |
| `propagator` | `OrbitalPropagator`, `EphemerustPropagator`, `TrackingProvider`, `TrackingState` |
| `validate` | `apply_physics_validation`, `expected_carrier_hz`, `RfMetadata`, `FLAG_*` constants |

Generate local API docs with:

```bash
cargo doc --open
```

### `physics_flags` contract

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Doppler anomaly: measured carrier differs from expected by more than tolerance. |
| 1 | `0x02` | Horizon/elevation: predicted elevation is below the configured minimum. |
| 2 | `0x04` | Reserved for RSSI/link-budget validation; not set today. |

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP, planned in-process WebSocket tests for M5, doctests, and
physics co-validation tests with explicitly documented tolerances — enforced at every milestone's
stage gate. The full strategy and per-milestone test matrix are defined in
[`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- [`BUILD_PLAN.md`](BUILD_PLAN.md) — milestone scope and stage gates.
- [`TEST_PLAN.md`](TEST_PLAN.md) — test matrix, counts, and physics tolerance register.
- [`Methodology.md`](Methodology.md) — decision log, trade-offs, and dependency attribution.
- [`AGENTS.md`](AGENTS.md) — compliance, attribution, security, and testing constitution.
- CCSDS Space Packet standard family (open international standards), implemented through the
  `spacepackets` crate boundary in `crates/gateway/src/ccsds.rs`.
- Ephemerust documentation/source for SGP4 look-angles and range-rate behavior consumed by the
  `OrbitalPropagator` seam.

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
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the CCSDS/ECSS packet crate used
  for Space Packet primary-header parsing.
- **[Tokio](https://tokio.rs/)** — the asynchronous runtime that drives UDP ingestion and future
  fan-out tasks.
- **[Axum](https://github.com/tokio-rs/axum)** — the planned M5 HTTP/WebSocket framework, selected
  to match the Rusty_Server-inspired architecture.
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
