# ChronusGateway-RS

ChronusGateway-RS (Chronus-GS) is an asynchronous Telemetry and Command (TMTC)
ground-station gateway written in Rust. It ingests raw spacecraft downlink frames, parses them
against open CCSDS standards, and is being built toward physics co-validation plus web-based
mission-control distribution in a single, memory-safe, garbage-collection-free executable.

Its planned distinguishing feature is a **Physics-Telemetry Co-Validation** engine: rather than
checking telemetry only against static limits, the gateway will use a live orbital propagator to
derive the expected Doppler shift, look-angles, and link geometry for the spacecraft, then flag
frames whose measured RF and signal parameters disagree with the physics. The propagator seam and
tracking-state provider that feed this engine are implemented today.

> **Status:** Early development. The astrodynamics seam, the asynchronous UDP ingestion loop
> (Milestone 1), CCSDS Space Packet parsing (Milestone 2), and station-configured orbital tracking
> (Milestone 3) are implemented and tested; the co-validation engine and the Open MCT distribution
> layer are tracked as gated milestones in [`BUILD_PLAN.md`](BUILD_PLAN.md).

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

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion today and will host the
  WebSocket distribution layer in Milestone 5.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
  `OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
  library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
  clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.

The reasoning behind these and other choices is recorded in [`Methodology.md`](Methodology.md).

### Current pipeline (Milestone 3)

What runs today is intentionally smaller than the full architecture diagram:

1. `main.rs` binds the UDP ingest socket using `IngestConfig::default()`
   (`127.0.0.1:7301`, channel capacity `1024`, max datagram `65_542` bytes).
2. `ingest::run` receives datagrams into a fixed-size buffer and broadcasts `RawFrame` values.
   Broadcast backpressure is lossy by design: slow consumers observe lag instead of blocking the
   socket loop.
3. A demo consumer parses each datagram with `ccsds::parse_telemetry`. Valid CCSDS telemetry
   packets become zero-copy `TelemetryFrame` values; telecommand, truncated, or malformed packets
   are logged and dropped.
4. If the default `StationConfig` and public ISS TLE build successfully, `TrackingProvider`
   computes a cached/throttled `TrackingState` for each parsed frame. If propagation is unavailable
   or fails for a frame timestamp, the gateway continues in parse-only mode.

`TelemetryFrame::physics_flags` is reserved for Milestone 4 and is currently always `0`.

### Public interfaces at a glance

| Module | Purpose | Main public types/functions |
|--------|---------|-----------------------------|
| `config` | Ingest and station settings | `IngestConfig`, `StationConfig`, `TleSource`, `ConfigError` |
| `ingest` | UDP receive loop | `RawFrame`, `IngestStats`, `bind`, `run` |
| `ccsds` | CCSDS Space Packet parsing | `TelemetryFrame`, `CcsdsError`, `parse_telemetry` |
| `propagator` | Astrodynamics seam and throttled provider | `OrbitalPropagator`, `EphemerustPropagator`, `TrackingProvider`, `TrackingState` |

---

## Repository layout

```
chronus-gateway/
├── Cargo.toml              Workspace manifest (centralized dependency versions, MSRV 1.88)
├── crates/gateway/         The gateway binary + library
│   ├── src/
│   │   ├── lib.rs          Crate documentation and module wiring
│   │   ├── config.rs       Ingestion and station/TLE configuration
│   │   ├── ingest.rs       Asynchronous UDP ingestion loop (RawFrame, stats, shutdown)
│   │   ├── ccsds.rs        CCSDS Space Packet parsing (TelemetryFrame, validation)
│   │   ├── propagator.rs   OrbitalPropagator trait + Ephemerust-backed implementation
│   │   └── main.rs         Entrypoint (ingest → parse → optional tracking logs)
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
cargo run        # listen on 127.0.0.1:7301 and run the M3 demo pipeline
cargo test       # unit + integration + doctests
```

To exercise the running gateway with a synthetic CCSDS telemetry packet, start `cargo run` in one
terminal and send the known-good packet below from another:

```bash
python3 - <<'PY'
import socket

# CCSDS TM, APID 0x02A, sequence 7, unsegmented, 5-byte payload "hello".
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

The gateway logs the decoded APID, sequence count, payload length, and tracking state when
propagation succeeds for the frame timestamp.

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

### Common setup and troubleshooting

| Symptom | Likely cause | Fix |
|---------|--------------|-----|
| `ephemerust` path dependency is missing | The sibling checkout is not next to this repo | Clone or place `Ephemerust/` beside `chronus-gateway/` as shown above. |
| `cargo run` cannot bind `127.0.0.1:7301` | Another process owns the default UDP port | Change `IngestConfig::bind_addr` in code while config-file support is pending. |
| Invalid datagrams are logged and dropped | Packet is not CCSDS telemetry, is too short, or declares more bytes than sent | Use the synthetic packet above or ensure the primary-header data-length field matches the payload. |
| Parsed frame logs show no physics state | TLE/station setup failed, or propagation failed for the received timestamp | Check the warning log and keep examples on public/synthetic TLEs only. |

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP, doctests, and future physics co-validation/WebSocket tests
with explicitly documented tolerances — enforced at every milestone's stage gate.

Current coverage is 15 unit tests (`ccsds`, `config`, `propagator`), 4 loopback UDP integration
tests (`tests/ingest.rs`), and 1 doctest. The full strategy, current counts, and pending M4/M5
gates are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- [`BUILD_PLAN.md`](BUILD_PLAN.md) — stage-gated implementation roadmap and milestone status.
- [`TEST_PLAN.md`](TEST_PLAN.md) — test matrix, fixture policy, and tolerance register.
- [`Methodology.md`](Methodology.md) — decision log, trade-offs, and dependency attribution.
- [`AGENTS.md`](AGENTS.md) — compliance, security, attribution, and testing constitution.
- [CCSDS Space Packet Protocol](https://public.ccsds.org/) — open packet standards that define the
  telemetry framing model used by the `ccsds` module.
- [`spacepackets`](https://crates.io/crates/spacepackets) — Rust CCSDS/ECSS packet library used for
  primary-header parsing.
- [Ephemerust](https://github.com/IsomorphicAlgo/ephemerust) and [`sgp4`](https://crates.io/crates/sgp4)
  — SGP4 look-angle/range-rate sources behind the default propagator.
- [Tokio](https://tokio.rs/), [Axum](https://github.com/tokio-rs/axum), and
  [NASA Open MCT](https://nasa.github.io/openmct/) — runtime and dashboard ecosystem sources for
  the current async core and planned distribution layer.

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
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the CCSDS/ECSS packet library used
  for Space Packet primary-header parsing behind the gateway's `ccsds` module boundary.
- **[Tokio](https://tokio.rs/)** and **[Axum](https://github.com/tokio-rs/axum)** — the
  asynchronous runtime and planned web framework that form the network core.
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

Licensed under the MIT License.

This project is designed strictly around open international standards (CCSDS) and is published
openly to comply with the Public Domain and Fundamental Research exclusions of ITAR/EAR. See
[`AGENTS.md`](AGENTS.md) for the project's compliance, attribution, and security policies.
