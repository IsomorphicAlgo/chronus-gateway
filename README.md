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

The gateway is built around two principles: an asynchronous network pipeline with bounded memory
and a clean abstraction boundary between the pipeline and any astrodynamics backend.

```
Raw RF / SDR ──▶ Async UDP ingestion ──▶ CCSDS parser ──▶ Physics-Telemetry
 (UDP today)       (Tokio)              (TelemetryFrame)    Co-Validation
                                                               │
                  OrbitalPropagator trait ◀── range-rate / look-angles
                  (Ephemerust today, nyx-space later)
                                                               │
                                                               ▼
                                    M4 binary logs validated frames today
                                    M5 plans Axum WebSocket → NASA Open MCT
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion and a lossy broadcast
  channel so a slow consumer cannot stall the socket loop. WebSocket fan-out is the next milestone,
  not part of the current binary.
- **Bounded, cheap-clone frames.** Incoming datagrams are capped by `max_datagram_size` and stored
  as `Arc<[u8]>`; downstream subscribers clone a reference count, not the payload.
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
│   │   ├── config.rs       Ingestion and station/tracking configuration
│   │   ├── ingest.rs       Asynchronous UDP ingestion loop (RawFrame, stats, shutdown)
│   │   ├── ccsds.rs        CCSDS Space Packet parsing (TelemetryFrame, validation)
│   │   ├── validate.rs     Physics–Telemetry Co-Validation (Doppler, elevation, physics_flags)
│   │   ├── propagator.rs   OrbitalPropagator trait + Ephemerust-backed implementation
│   │   └── main.rs         Entrypoint (runs the M1-M4 pipeline)
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
cargo build             # compile the workspace
RUST_LOG=debug cargo run # listen for UDP telemetry on 127.0.0.1:7301 until Ctrl-C
cargo test              # unit + integration + doctests
```

`cargo run` starts the implemented M1-M4 pipeline with default, hard-coded development settings:

| Setting | Default | Source |
|---------|---------|--------|
| UDP bind address | `127.0.0.1:7301` | `IngestConfig::default()` |
| Broadcast capacity | `1024` frames | `IngestConfig::default()` |
| Max datagram size | `65_542` bytes | `IngestConfig::default()` |
| Station location | 35 deg N, 116 deg W, 1000 m | `StationConfig::default()` |
| Nominal carrier | `437_500_000` Hz | `StationConfig::default()` |
| Tracking object | public ISS (ZARYA) TLE | `DEFAULT_ISS_TLE` |
| Tracking throttle | 10 ms | `StationConfig::default()` |
| Doppler tolerance | `150` Hz | `StationConfig::default()` / `TEST_PLAN.md` |
| Minimum elevation | `0` deg | `StationConfig::default()` |

Send the same synthetic telemetry packet used by the parser tests:

```bash
python3 - <<'PY'
import socket

packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(packet, ("127.0.0.1", 7301))
PY
```

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details. Other
> Windows developers may need to update the absolute linker path or remove the target override.

### Runtime pipeline (M4)

The current binary is intentionally small and explicit:

1. Bind a UDP socket with `IngestConfig::default()`.
2. Publish each datagram as a `RawFrame { bytes, received_at, source }` on a bounded broadcast
   channel and update `IngestStats`.
3. Parse telemetry with `ccsds::parse_telemetry`, which decodes the CCSDS primary header via
   `spacepackets`, rejects telecommands on this path, and exposes the packet data field zero-copy.
4. Build a `TrackingProvider` from `StationConfig::default()`. If the propagator cannot be built
   (for example, an invalid TLE), ingestion and parsing continue without physics state.
5. Apply `validate::apply_physics_validation` when tracking state is available, then log the frame.

`physics_flags` is the stable bitfield carried by `TelemetryFrame`:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Doppler anomaly: measured carrier differs from expected beyond tolerance. |
| 1 | `0x02` | Horizon/elevation anomaly: predicted elevation is below the configured minimum. |
| 2 | `0x04` | Reserved for RSSI/link-budget validation; not set yet. |

The running binary currently passes `RfMetadata::default()`, so no measured carrier is available
and the Doppler check is skipped. Tests cover Doppler behavior by passing synthetic RF metadata;
production SDR metadata wiring is deferred to the distribution or side-channel work.

### Troubleshooting

- **`ephemerust` not found:** clone or place Ephemerust as `../Ephemerust` relative to this
  workspace, or adjust the path dependency in `Cargo.toml` for local experiments.
- **Older Rust toolchain:** install or select Rust 1.88 or newer, for example with
  `rustup update stable` and `cargo +stable test`.
- **No parsed frames in logs:** send valid CCSDS telemetry packets. Short datagrams, malformed
  headers, truncated packets, and telecommands are logged and dropped as recoverable input errors.
- **Oversized datagrams:** Windows reports `WSAEMSGSIZE` and increments `oversized_dropped`; Unix
  may truncate to the receive buffer and let the CCSDS length check reject the partial packet.
- **No Doppler anomaly bit from `cargo run`:** expected until measured carrier data is supplied.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP, planned in-process WebSocket tests for M5, doctests, and
physics co-validation tests with explicitly documented tolerances — enforced at every milestone's
stage gate. The full strategy and per-milestone test matrix are defined in
[`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- [`Methodology.md`](Methodology.md) — decision log, trade-offs, dependency attribution, and open
  decisions.
- [`BUILD_PLAN.md`](BUILD_PLAN.md) and [`TEST_PLAN.md`](TEST_PLAN.md) — stage gates, implemented
  milestone scope, test counts, and physics tolerance register.
- [CCSDS Space Packet Protocol, CCSDS 133.0-B-2](https://public.ccsds.org/) — open standard behind
  the telemetry primary header parsed by `ccsds.rs`.
- [`spacepackets`](https://crates.io/crates/spacepackets) — CCSDS/ECSS packet parsing crate used
  behind the gateway's `ccsds` module boundary.
- [Ephemerust](https://github.com/IsomorphicAlgo/ephemerust) and the [`sgp4` crate](https://crates.io/crates/sgp4)
  — SGP4-based look-angle and range-rate source for tracking and Doppler validation.
- [Tokio](https://tokio.rs/) — asynchronous runtime used for UDP ingestion, broadcast fan-out, and
  shutdown handling.
- [NASA Open MCT](https://nasa.github.io/openmct/) — target operator dashboard for the planned M5
  distribution layer.

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
- **[Tokio](https://tokio.rs/)** — the asynchronous runtime that drives the ingestion loop and
  broadcast channel.
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the maintained CCSDS/ECSS parser
  used for Space Packet primary-header decoding.
- **[Axum](https://github.com/tokio-rs/axum)** — the web framework planned for the M5 WebSocket
  and health endpoints, following patterns from Rusty_Server.
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

Licensed under the MIT License; see [`LICENSE`](LICENSE).

This project is designed strictly around open international standards (CCSDS) and is published
openly to comply with the Public Domain and Fundamental Research exclusions of ITAR/EAR. See
[`AGENTS.md`](AGENTS.md) for the project's compliance, attribution, and security policies.
