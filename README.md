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

## Current runtime pipeline (M1-M4)

The binary currently runs the ingest -> parse -> track -> validate path and logs accepted or
rejected datagrams. WebSocket / Open MCT distribution is intentionally deferred to Milestone 5.

1. **UDP ingest** (`ingest::run`) binds `IngestConfig::bind_addr` and forwards each datagram as a
   `RawFrame { bytes, received_at, source }` on a lossy `tokio::sync::broadcast` channel.
2. **CCSDS parse** (`ccsds::parse_telemetry`) accepts only CCSDS Space Packet telemetry (TM)
   packets. It validates the 6-byte primary header, declared data-field length, and packet type,
   then exposes the packet data field through `TelemetryFrame::payload()` without copying.
3. **Tracking** (`TrackingProvider`) resolves the station TLE through `EphemerustPropagator`,
   caches look-angle results for the configured throttle window, and returns `TrackingState`
   (`azimuth_deg`, `elevation_deg`, `range_km`, `range_rate_km_s`).
4. **Physics validation** (`validate::apply_physics_validation`) clears and sets
   `TelemetryFrame::physics_flags`:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Doppler anomaly: measured carrier differs from expected by more than the tolerance. |
| 1 | `0x02` | Below configured minimum elevation (default: below the mathematical horizon). |
| 2 | `0x04` | Reserved for RSSI / link-budget validation; not set yet. |

The current binary passes `RfMetadata::default()`, so Doppler is skipped until SDR carrier
metadata is wired in. Elevation validation still runs whenever the propagator is available.

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
cargo run -p chronus-gateway
cargo test       # unit + integration + doctests
```

`cargo run -p chronus-gateway` starts the UDP gateway on the development default
`127.0.0.1:7301` and runs until Ctrl-C. In another terminal, send a synthetic CCSDS TM packet:

```bash
python3 - <<'PY'
import socket

# CCSDS primary header:
# - TM packet, APID 0x02A
# - unsegmented sequence count 7
# - data length 4, meaning a 5-byte packet data field ("hello")
packet = bytes.fromhex("00 2A C0 07 00 04") + b"hello"

sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

Expected behavior: the gateway logs `telemetry frame parsed` with APID `42`, sequence `7`,
payload length `5`, tracking values, and `physics_flags`. If the packet is malformed or a
telecommand (TC), the gateway logs a recoverable warning and continues.

### Configuration constraints

Runtime CLI/env configuration has not landed yet; defaults live in `crates/gateway/src/config.rs`
and library users/tests can construct configs directly.

| Setting | Default | Constraint / note |
|---------|---------|-------------------|
| `IngestConfig::bind_addr` | `127.0.0.1:7301` | Loopback for development; production can bind a NIC-specific address. |
| `IngestConfig::channel_capacity` | `1024` frames | Lossy by design; slow subscribers observe `Lagged`. |
| `IngestConfig::max_datagram_size` | `65_542` bytes | Fixed receive buffer; oversized datagrams are dropped or truncated by the OS. |
| `StationConfig::latitude_deg` / `longitude_deg` | `35.0`, `-116.0` | Must be finite and in `[-90, 90]` / `[-180, 180]`. |
| `StationConfig::nominal_carrier_hz` | `437_500_000.0` | Must be finite and positive; used for Doppler math. |
| `StationConfig::tle` | Inline public ISS TLE | Inline text or readable file via `TleSource::File`. |
| `StationConfig::doppler_tolerance_hz` | `150.0` Hz | Must be finite and positive; see `TEST_PLAN.md` T-DOPPLER. |
| `StationConfig::minimum_elevation_deg` | `0.0` deg | Must be finite and in `[-90, 90]`; strict lower-than comparison. |

### Common troubleshooting

- **Build cannot find Ephemerust:** check that `../Ephemerust` exists relative to this repository;
  the dependency is currently a sibling path dependency.
- **Rust version error:** use Rust 1.88 or newer (`cargo +stable ...` if the default toolchain is
  older).
- **Address already in use:** another process is bound to `127.0.0.1:7301`; stop it or use the
  library API with a different `IngestConfig::bind_addr` until CLI config is added.
- **Packets are dropped as invalid:** confirm the datagram is at least 6 bytes, the CCSDS
  data-length field equals `payload_len - 1`, and the packet type is TM (not TC).
- **Unexpected `physics_flags`:** bit 1 can be set for below-horizon geometry using the default
  ISS/station example. Bit 0 is not set by the current binary because no measured carrier is
  supplied yet.
- **Slow consumers miss frames:** this is intentional backpressure behavior; the ingest loop favors
  fresh telemetry over blocking on stale subscribers.

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- [CCSDS Space Packet Protocol](https://public.ccsds.org/) — public packet framing standard used
  for the telemetry primary header and packet data field.
- [`spacepackets`](https://crates.io/crates/spacepackets) — Rust CCSDS/ECSS packet parsing crate
  wrapped by `crates/gateway/src/ccsds.rs`.
- [Ephemerust](https://github.com/IsomorphicAlgo/ephemerust) — sibling SGP4/look-angle crate used
  by `EphemerustPropagator`.
- [`sgp4`](https://crates.io/crates/sgp4) — SGP4/SDP4 propagation crate used through Ephemerust.
- [Tokio](https://tokio.rs/) — async UDP, tasks, signals, and broadcast channels.
- [NASA Open MCT](https://nasa.github.io/openmct/) — target dashboard integration for the planned
  Milestone 5 distribution layer.

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
