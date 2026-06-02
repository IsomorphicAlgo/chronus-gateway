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
Raw RF / SDR front-end ──▶ Async UDP ingestion ──▶ CCSDS zero-copy parser ──▶
      (UDP today)              (Tokio)                 (validated TM)          │
                                                                                ▼
                  OrbitalPropagator trait ◀── range-rate / look-angles ── Physics-
                  (Ephemerust today, nyx-space later)                    Telemetry
                                                                        Co-Validation
                                                                                │
                                                                                ▼
                                                        Axum WebSocket ──▶ NASA Open MCT
                                                        (planned M5)
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion today. Milestone 5 adds
  the Axum WebSocket distribution layer for concurrent operator screens.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
  `OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
  library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
  clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.
- **Validation bitfield.** Parsed telemetry carries `physics_flags` so downstream dashboards can
  color or alarm on physics-derived anomalies without reparsing RF metadata.

The reasoning behind these and other choices is recorded in [`Methodology.md`](Methodology.md).

---

## Repository layout

```
chronus-gateway/
├── Cargo.toml              Workspace manifest (centralized dependency versions, MSRV 1.88)
├── crates/gateway/         The gateway binary + library
│   ├── src/
│   │   ├── lib.rs          Crate documentation and module wiring
│   │   ├── config.rs       Ingestion + station/TLE/validation configuration
│   │   ├── ingest.rs       Asynchronous UDP ingestion loop (RawFrame, stats, shutdown)
│   │   ├── ccsds.rs        CCSDS Space Packet parsing (TelemetryFrame, validation)
│   │   ├── validate.rs     Physics–Telemetry Co-Validation (Doppler, elevation, physics_flags)
│   │   ├── propagator.rs   OrbitalPropagator trait + Ephemerust-backed implementation
│   │   └── main.rs         Entrypoint (UDP ingest → parse → validate logging demo)
│   └── tests/
│       └── ingest.rs       Loopback UDP integration tests
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
cargo run        # listen on UDP 127.0.0.1:7301 and log parsed/validated telemetry
cargo test       # unit + integration + doctests
```

The current binary is a local pipeline demonstrator, not yet a configurable production service:
it binds the default UDP socket, parses incoming CCSDS telemetry packets, computes tracking state
from the default station/TLE, applies physics validation, and logs the result. If the propagator
cannot be constructed, ingestion and CCSDS parsing continue without physics state.

To send one synthetic CCSDS telemetry packet to a running gateway:

```bash
python3 - <<'PY'
import socket

payload = b"hello"
word1 = 0x002A          # version 0, TM, no secondary header, APID 0x02A
word2 = 0xC007          # unsegmented sequence, sequence count 7
data_len = len(payload) - 1
packet = word1.to_bytes(2, "big") + word2.to_bytes(2, "big") + data_len.to_bytes(2, "big") + payload

socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(packet, ("127.0.0.1", 7301))
PY
```

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Operational defaults and constraints

These defaults are hard-coded via `Default` implementations while file/env configuration is still
pending:

| Area | Default | Constraint / behavior |
|------|---------|-----------------------|
| UDP bind | `127.0.0.1:7301` | Loopback for development; production deployments should bind a specific NIC or `0.0.0.0` once configuration is added. |
| Broadcast channel | `1024` frames | Lossy by design: slow subscribers observe lag and never block socket receive. |
| Datagram ceiling | `65_542` bytes | Fixed receive buffer; oversized datagrams are dropped on Windows or truncated by Unix kernels and then rejected by length checks if incomplete. |
| Station | lat `35.0`, lon `-116.0`, altitude `1000 m` | Synthetic development geometry only. |
| TLE | public ISS (ZARYA) reference TLE | Public reference data; no mission-specific operational data belongs in the repo. |
| Nominal carrier | `437_500_000 Hz` | Used only by the Doppler check. |
| Tracking throttle | `10 ms` | Reuses the last `TrackingState` for bursts inside the throttle window. |
| Doppler tolerance | `150 Hz` | `|measured - expected|` above this sets bit 0 when measured carrier metadata exists. |
| Minimum elevation | `0 deg` | Predicted elevation strictly below this sets bit 1. |

`physics_flags` is the stable anomaly contract exported on `TelemetryFrame`:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Doppler anomaly: measured carrier differs from expected beyond tolerance. |
| 1 | `0x02` | Horizon/elevation anomaly: spacecraft is below the configured minimum elevation. |
| 2 | `0x04` | Reserved for future RSSI / link-budget validation; not set today. |

The default binary currently passes `RfMetadata::default()`, so Doppler validation is library-ready
but skipped at runtime until SDR/front-end carrier metadata is wired. The elevation check still
runs whenever a tracking state is available.

---

## Public library surface

The `chronus_gateway` crate exposes the primary interfaces used by downstream binaries and future
services:

- `IngestConfig`, `ingest::bind`, `ingest::run`, `RawFrame`, and `IngestStats` for UDP capture.
- `parse_telemetry`, `TelemetryFrame`, `CcsdsError`, and `CCSDS_PRIMARY_HEADER_LEN` for CCSDS
  Space Packet primary-header parsing and zero-copy payload access.
- `StationConfig`, `TleSource`, `EphemerustPropagator`, `OrbitalPropagator`, `TrackingProvider`,
  and `TrackingState` for station-resolved look angles and range rate.
- `apply_physics_validation`, `expected_carrier_hz`, `RfMetadata`, and `FLAG_*` constants for
  physics anomaly checks.

CCSDS parsing currently validates the primary header and exposes whether a secondary header is
declared; it does not decode PUS or mission-specific secondary-header fields.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and planned in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

Current coverage is strongest at the module boundary: UDP ingestion has loopback integration tests,
while CCSDS parsing, station tracking, and validation are covered by focused unit tests. The first
full ingest → parse → validate → WebSocket test arrives with Milestone 5.

---

## References

- [CCSDS](https://public.ccsds.org/) and the CCSDS Space Packet protocol family, including
  CCSDS 133.0-B-2 as the source for the primary-header model used by `ccsds.rs`.
- [`spacepackets`](https://crates.io/crates/spacepackets) — Rust implementation used for CCSDS
  Space Packet primary-header decoding.
- [`sgp4`](https://crates.io/crates/sgp4) — Rust SGP4/SDP4 propagator consumed through
  Ephemerust.
- [NASA Open MCT](https://nasa.github.io/openmct/) — target dashboard framework for the planned
  distribution layer.
- [`nyx-space`](https://github.com/nyx-space/nyx) and [`sat-rs`](https://github.com/us-irs/sat-rs)
  — Rust aerospace ecosystem projects considered during design.
- [`space-packet`](https://crates.io/crates/space-packet) — evaluated CCSDS parser alternative;
  documented in `Methodology.md` D-010.

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
  asynchronous runtime in use today and planned web framework for the network core.
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the CCSDS packet crate used for
  primary-header parsing behind the local `ccsds` module boundary.
- **[CCSDS](https://public.ccsds.org/)** — the open international standards for space packet
  framing and protocols that define the gateway's wire formats.
- **[NASA Open MCT](https://nasa.github.io/openmct/)** — the open-source mission-control
  framework targeted by the distribution layer.
- **[NeXosim](https://github.com/asynchronics/nexosim)** — the discrete-event simulation
  framework planned for hardware-in-the-loop validation.

The broader Rust aerospace ecosystem — including `sat-rs`, `space-packet`, and `nyx-space` —
informed the design analysis.

---

## License and compliance

Licensed under the MIT License.

This project is designed strictly around open international standards (CCSDS) and is published
openly to comply with the Public Domain and Fundamental Research exclusions of ITAR/EAR. See
[`AGENTS.md`](AGENTS.md) for the project's compliance, attribution, and security policies.
