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
 (UDP today)       (Tokio)                 (TelemetryFrame)          (Ephemerust SGP4)
                                                                         │
                                                                         ▼
                  OrbitalPropagator trait ◀── range-rate / look-angles ──┤
                  (Ephemerust today, nyx-space later)                    ▼
                                                  Physics co-validation sets physics_flags
                                                                         │
                                                                         ▼
                                              Axum WebSocket (M5) ──▶ NASA Open MCT
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
├── crates/gateway/         The gateway binary + library
│   ├── src/
│   │   ├── lib.rs          Crate documentation and module wiring
│   │   ├── config.rs       Ingestion configuration
│   │   ├── ingest.rs       Asynchronous UDP ingestion loop (RawFrame, stats, shutdown)
│   │   ├── ccsds.rs        CCSDS Space Packet parsing (TelemetryFrame, validation)
│   │   ├── validate.rs     Physics–Telemetry Co-Validation (Doppler, elevation, physics_flags)
│   │   ├── propagator.rs   OrbitalPropagator trait + Ephemerust-backed implementation
│   │   └── main.rs         Entrypoint (ingest → parse → track → validate)
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
cargo run        # listen on loopback UDP and log parsed/validated telemetry
cargo test       # unit + integration + doctests
```

### Current runtime workflow

The binary currently uses default in-code configuration:

- UDP bind address: `127.0.0.1:7301`.
- Downlink packets: CCSDS Space Packets carrying telemetry (TM); telecommand packets are rejected
  on this path.
- Tracking: a public ISS (ZARYA) reference TLE and a generic synthetic ground station.
- Co-validation: Doppler bit 0 is skipped until RF metadata is wired; the elevation gate can still
  set `physics_flags` bit 1 when the propagated state is below the configured horizon threshold.

To exercise the loopback path with a synthetic CCSDS TM packet:

```bash
# terminal 1
RUST_LOG=info cargo run

# terminal 2
python3 - <<'PY'
import socket

payload = b"hello"
apid = 0x2A
seq_count = 1
word1 = apid                 # version=0, type=TM, no secondary header
word2 = (0b11 << 14) | seq_count
data_len = len(payload) - 1  # CCSDS stores (data field octets - 1)
packet = word1.to_bytes(2, "big") + word2.to_bytes(2, "big") + data_len.to_bytes(2, "big") + payload
socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(packet, ("127.0.0.1", 7301))
PY
```

Expected log fields include `apid=42`, `seq=1`, `payload=5`, propagated `az_deg`/`el_deg`/`range_km`,
`range_rate_km_s`, and `physics_flags`.

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

## Public interface contracts

- `chronus_gateway::ingest` exposes `RawFrame`, `IngestConfig`, `IngestStats`, `bind`, and `run`.
  The broadcast channel is intentionally lossy so slow consumers do not stall UDP receive.
- `chronus_gateway::ccsds::parse_telemetry` validates primary-header length, header decoding,
  declared packet length, and TM/TC routing before returning a zero-copy `TelemetryFrame`.
- `chronus_gateway::propagator` exposes the `OrbitalPropagator` seam, the Ephemerust-backed
  implementation, and the throttled `TrackingProvider` cache used by the runtime pipeline.
- `chronus_gateway::validate::apply_physics_validation` clears then sets `TelemetryFrame::physics_flags`.
  Stable bits today are:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Doppler anomaly: measured carrier differs from expected beyond tolerance. |
| 1 | `0x02` | Horizon/elevation anomaly: propagated elevation is below the configured threshold. |
| 2 | `0x04` | Reserved for RSSI/link-budget validation; not set yet. |

If the Ephemerust propagator cannot be constructed (for example, bad TLE input), `main` logs a
warning and continues ingesting/parsing without physics state rather than stopping the UDP path.

---

## References

- [`AGENTS.md`](AGENTS.md) — project compliance, attribution, security, and testing rules.
- [`BUILD_PLAN.md`](BUILD_PLAN.md) — stage-gated milestone roadmap and current implementation state.
- [`TEST_PLAN.md`](TEST_PLAN.md) — deterministic test matrix, physics tolerances, and status counts.
- [`Methodology.md`](Methodology.md) — decision log, trade-offs, and dependency attribution.
- [CCSDS public standards](https://public.ccsds.org/) — open space packet and TMTC standards.
- [NASA Open MCT](https://nasa.github.io/openmct/) — target web mission-control framework for M5.
- [Tokio](https://tokio.rs/) and [Axum](https://github.com/tokio-rs/axum) — async runtime and
  planned WebSocket/HTTP framework.

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
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the Rust CCSDS/ECSS packet crate
  used behind the gateway's parser boundary.
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
