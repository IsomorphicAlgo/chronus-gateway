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

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion and a pool of
  WebSocket connections, scaling to many concurrent telemetry channels and operator screens.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
  `OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
  library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
  clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.

The reasoning behind these and other choices is recorded in [`Methodology.md`](Methodology.md).

---

## Current implemented workflow

Milestones 1-3 implement a deterministic ingestion and tracking skeleton:

1. `main` binds a UDP socket from `IngestConfig::default()`.
2. `ingest::run` receives each datagram into a fixed-size buffer and publishes a `RawFrame` on a
   lossy `tokio::sync::broadcast` channel.
3. A demonstration consumer parses each datagram with `ccsds::parse_telemetry`.
4. Valid TM packets are paired with a throttled `TrackingProvider` state at the frame timestamp.
5. The binary logs the APID, sequence count, payload length, and current tracking state.

The co-validation engine does not yet set anomaly flags, and the Open MCT WebSocket interface is
still planned work. Library callers can use the M1-M3 modules directly today.

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

## Public interfaces and constraints

| Module | Public surface | Important constraints |
|--------|----------------|-----------------------|
| `config` | `IngestConfig`, `StationConfig`, `TleSource`, `ConfigError` | The binary currently uses defaults. `StationConfig::validate()` range-checks station fields; file TLEs are read only by `resolve_tle_text()`. |
| `ingest` | `bind`, `run`, `RawFrame`, `IngestStats` | UDP only; receive memory is bounded by `max_datagram_size`; broadcast is intentionally lossy under backpressure. |
| `ccsds` | `parse_telemetry`, `TelemetryFrame`, `CcsdsError` | Accepts CCSDS TM packets only. Header decode, length checks, and TC rejection are recoverable errors. `payload()` borrows zero-copy from the original datagram. |
| `propagator` | `OrbitalPropagator`, `EphemerustPropagator`, `TrackingProvider`, `TrackingState` | The default backend is Ephemerust/SGP4. `TrackingProvider` caches states within `min_recompute_interval_ms`; `0` disables caching. |

Default runtime values are intentionally development-safe:

- UDP bind address: `127.0.0.1:7301`
- Broadcast capacity: `1024` frames
- Maximum datagram size: `65_542` bytes (CCSDS primary header plus maximum packet data field)
- Station: synthetic/public ISS TLE, `35.0` deg lat, `-116.0` deg lon, `1000 m` altitude
- Nominal carrier: `437_500_000 Hz`
- Tracking recompute throttle: `10 ms` (about 100 Hz)

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
cargo run        # listen for UDP telemetry on 127.0.0.1:7301
cargo test       # unit + integration + doctests
```

Use `RUST_LOG` to adjust runtime logs:

```bash
RUST_LOG=info,chronus_gateway=debug cargo run
```

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Local smoke test

With the gateway running in one terminal, send a synthetic CCSDS TM packet from another terminal:

```bash
python - <<'PY'
import socket

# TM, APID 0x02A, unsegmented sequence count 7, 5-byte payload "hello".
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"

sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

Expected result: the gateway logs a parsed telemetry frame with `apid=42`, `seq=7`,
`payload=5`, and the current Ephemerust tracking state. Malformed packets, truncated payloads,
and TC packets are logged and dropped without stopping the ingestion loop.

---

## Troubleshooting and operational notes

- **`failed to bind UDP socket`**: another process may already be using `127.0.0.1:7301`, or the
  desired production interface may not exist. Library callers can set `IngestConfig.bind_addr`;
  the binary does not expose CLI/env configuration yet.
- **No parsed frames appear**: confirm the sender is using UDP, the destination is the bind
  address above, and the packet is a CCSDS telemetry packet with a valid primary header and
  declared data length.
- **`dropping invalid/non-telemetry datagram`**: the parser rejected a recoverable CCSDS error.
  Common causes are fewer than 6 header bytes, a payload shorter than the header declares, or a
  TC packet on the TM ingestion path.
- **Slow consumers miss frames**: this is expected. The internal broadcast channel drops old
  frames under pressure so the socket loop remains live.
- **Oversized datagrams differ by OS**: Windows may report and drop `WSAEMSGSIZE`; Unix may
  truncate to `max_datagram_size`. The parser treats resulting partial packets as invalid.
- **No physics state**: invalid station coordinates, invalid carrier frequency, or unreadable TLE
  sources prevent `EphemerustPropagator::from_station` from starting. The current binary logs a
  warning and continues ingesting/parsing without tracking state.
- **External data guardrail**: keep examples synthetic or public-reference only. Do not commit
  mission keys, controlled RF parameters, or operational data.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- **CCSDS Space Packet Protocol (CCSDS 133.0-B-2)** — public standard for the primary header and
  packet data length semantics used by `ccsds::parse_telemetry`.
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — CCSDS/ECSS packet crate used to
  decode the primary header behind the gateway's `ccsds` module boundary.
- **[Ephemerust](https://github.com/IsomorphicAlgo/ephemerust)** — sibling SGP4/look-angle crate
  used by `EphemerustPropagator`.
- **[`sgp4`](https://crates.io/crates/sgp4)** — SGP4/SDP4 propagation crate used by Ephemerust.
- **[Tokio](https://tokio.rs/)** — async runtime used for UDP I/O, broadcast channels, signals,
  and tests.
- **[`tracing`](https://crates.io/crates/tracing)** and
  **[`tracing-subscriber`](https://crates.io/crates/tracing-subscriber)** — structured runtime
  logging.
- **[NASA Open MCT](https://nasa.github.io/openmct/)** — target dashboard framework for the
  planned distribution milestone.
- **[NeXosim](https://github.com/asynchronics/nexosim)** — planned simulation framework for the
  stretch hardware-in-the-loop validation milestone.

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
- **[Tokio](https://tokio.rs/)** — the asynchronous runtime that forms the current network core.
- **[Axum](https://github.com/tokio-rs/axum)** — the web framework intended for the planned
  Open MCT distribution layer, following Rusty_Server patterns.
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
