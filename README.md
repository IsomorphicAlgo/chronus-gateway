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

The gateway is built around two principles: an asynchronous network core and a clean abstraction
boundary between the pipeline and any astrodynamics backend.

```
Raw RF / SDR ──▶ Async UDP ingestion ──▶ CCSDS zero-copy parser ──▶ Physics-Telemetry
 (UDP/TCP)        (Tokio)                  (validated frames)         Co-Validation engine
                                                                            │
                  OrbitalPropagator trait ◀── range-rate / look-angles ─────┤
                  (Ephemerust today, nyx-space later)                       ▼
                                                          Axum WebSocket ──▶ NASA Open MCT
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion today and will also
  drive the planned WebSocket fan-out, keeping slow consumers from blocking the receive loop.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
  `OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
  library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
  clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.
- **Validated packet boundary.** Raw datagrams become `TelemetryFrame`s only after CCSDS primary
  header decoding, declared-length checks, and TM/TC routing. The packet data field is exposed as
  a zero-copy borrow from the retained datagram.

The reasoning behind these and other choices is recorded in [`Methodology.md`](Methodology.md).

---

## Repository layout

```
chronus-gateway/
├── Cargo.toml              Workspace manifest (centralized dependency versions, MSRV 1.88)
├── crates/gateway/         The gateway binary + library
│   ├── src/
│   │   ├── lib.rs          Crate documentation and module wiring
│   │   ├── config.rs       Ingestion + station/propagator configuration
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
cargo run        # listen for CCSDS telemetry over UDP on 127.0.0.1:7301
cargo test       # unit + integration + doctests
```

`cargo run` starts the current Milestone 3 pipeline:

1. Bind the UDP downlink socket from `IngestConfig` (default `127.0.0.1:7301`).
2. Broadcast each datagram as a `RawFrame` with capture time and source address.
3. Parse telemetry datagrams as CCSDS Space Packets with `ccsds::parse_telemetry`.
4. Compute a station-relative `TrackingState` from `StationConfig` and the Ephemerust backend.
5. Log the APID, sequence count, payload length, look angles, range, and range rate.

Example local smoke input (synthetic CCSDS TM packet, APID `0x02a`, sequence `7`, payload
`hello`):

```bash
cargo run

# In another shell:
python - <<'PY'
import socket

packet = bytes.fromhex("002ac007000468656c6c6f")
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

The binary currently uses default in-code configuration. For custom deployments, instantiate the
library types directly:

| Type | Purpose | Default / constraints |
|------|---------|-----------------------|
| `IngestConfig` | UDP bind address, broadcast capacity, maximum datagram size | `127.0.0.1:7301`, capacity `1024`, max `65_542` bytes. The max fixes receive-buffer size so input cannot drive unbounded allocation. |
| `StationConfig` | Ground-station geodetic position, nominal carrier, TLE source, tracking throttle | `35.0°`, `-116.0°`, `1000 m`, `437.5 MHz`, public ISS TLE, `10 ms` throttle. Latitude, longitude, altitude, frequency, and inline TLE text are validated before propagation. |
| `TleSource` | Tracked-object element source | Inline text or local file. Network fetch from CelesTrak/Space-Track is intentionally deferred. |

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

## Public interfaces and current workflow

- `ingest::bind` and `ingest::run` provide the asynchronous UDP ingestion loop. The loop forwards
  `RawFrame`s over a bounded, lossy `tokio::sync::broadcast` channel; slow subscribers receive
  `Lagged` instead of stalling the socket.
- `ccsds::parse_telemetry` converts a `RawFrame` into a validated `TelemetryFrame`. Telecommands
  are rejected on this telemetry path, malformed/truncated packets return structured
  `CcsdsError`s, and bytes past the declared packet length are ignored.
- `EphemerustPropagator`, `OrbitalPropagator`, and `TrackingProvider` form the astrodynamics seam.
  `TrackingProvider` caches states inside the configured throttle window and computes outside its
  mutex so propagation work does not serialize unrelated callers.
- `TelemetryFrame::physics_flags` is reserved for the Milestone 4 co-validation bitfield. It is
  always `0` in the current implementation because anomaly checks have not landed yet.

---

## Troubleshooting and common pitfalls

- **`ephemerust` path dependency not found:** clone the owner's Ephemerust repository as a sibling
  directory named `Ephemerust` next to this repo, or update `Cargo.toml` deliberately and record the
  decision in `Methodology.md`.
- **No packets logged:** the default binary listens only on loopback (`127.0.0.1:7301`). Bind a
  different address in code for SDR/front-end hosts on another machine or NIC.
- **Packets logged as invalid/non-telemetry:** the current parser expects a complete CCSDS Space
  Packet with a telemetry packet type. Short datagrams, declared lengths larger than the datagram,
  and TC packets are dropped with structured warnings.
- **Slow consumers miss frames:** this is expected. The broadcast channel is intentionally lossy so
  ingestion favors freshest telemetry and never blocks on downstream work.
- **Unexpected tracking values:** confirm the station latitude/longitude/altitude and TLE source.
  The default TLE is a public ISS reference set used for deterministic tests, not an operational
  mission configuration.

---

## References

- CCSDS 133.0-B-2, *Space Packet Protocol* — primary-header fields, packet data-length semantics,
  APID, sequence count, and TM/TC packet type.
- [`spacepackets`](https://crates.io/crates/spacepackets) — Rust CCSDS/ECSS packet parsing used by
  `crates/gateway/src/ccsds.rs`.
- [`ephemerust`](https://github.com/IsomorphicAlgo/ephemerust) — owner's SGP4/look-angle library
  used through the `OrbitalPropagator` seam.
- [`sgp4`](https://crates.io/crates/sgp4) — SGP4/SDP4 orbit propagation used underneath
  Ephemerust.
- [`tokio`](https://tokio.rs/) — async runtime, UDP socket, signal handling, broadcast channel, and
  test runtime.
- [`tracing`](https://crates.io/crates/tracing) and
  [`tracing-subscriber`](https://crates.io/crates/tracing-subscriber) — structured runtime logging.
- [`chrono`](https://crates.io/crates/chrono), [`serde`](https://crates.io/crates/serde),
  [`anyhow`](https://crates.io/crates/anyhow), and
  [`thiserror`](https://crates.io/crates/thiserror) — time, serialization, and error-handling
  support.
- [`NASA Open MCT`](https://nasa.github.io/openmct/) — planned operator-dashboard integration.

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
