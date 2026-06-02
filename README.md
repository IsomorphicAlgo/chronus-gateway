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
 (UDP today)       (Tokio)                  (validated frames)         Co-Validation engine
                                                                            │
                  OrbitalPropagator trait ◀── range-rate / look-angles ─────┤
                  (Ephemerust today, nyx-space later)                       ▼
                                                  Axum WebSocket ──▶ NASA Open MCT
                                                  (planned Milestone 5)
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion today; the planned
  Axum WebSocket layer will reuse the same broadcast channel for multiple operator screens.
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
│   │   ├── config.rs       Ingestion + station/validation defaults
│   │   ├── ingest.rs       Asynchronous UDP ingestion loop (RawFrame, stats, shutdown)
│   │   ├── ccsds.rs        CCSDS Space Packet parsing (TelemetryFrame, validation)
│   │   ├── validate.rs     Physics–Telemetry Co-Validation (Doppler, elevation, physics_flags)
│   │   ├── propagator.rs   OrbitalPropagator trait + Ephemerust-backed implementation
│   │   └── main.rs         Entrypoint (ingest → parse → track → validate loop)
│   └── tests/
│       └── ingest.rs       Milestone 1 integration tests (M2-M4 use inline unit tests)
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
cargo run        # run the default local gateway
cargo test       # unit + integration + doctests
```

`cargo run` starts the current M1-M4 pipeline:

1. bind the UDP downlink socket;
2. broadcast each raw datagram as an `ingest::RawFrame`;
3. parse valid telemetry packets with `ccsds::parse_telemetry`;
4. compute a throttled `TrackingState` from the default station/TLE configuration;
5. apply Physics-Telemetry Co-Validation flags; and
6. log parsed frames, invalid datagrams, lag, and shutdown statistics.

There is no CLI or external config file yet. The binary uses the defaults in
`IngestConfig::default()` and `StationConfig::default()`; library consumers can construct their
own configs directly.

### Default local settings

| Setting | Default | Notes |
|---------|---------|-------|
| UDP bind address | `127.0.0.1:7301` | Loopback only for development; production can bind a specific NIC or `0.0.0.0` in code/config. |
| Channel capacity | `1024` frames | Lossy broadcast; slow consumers observe lag instead of blocking the socket. |
| Max datagram size | `65_542` bytes | Fixed receive buffer; oversized/truncated packets are dropped by later validation. |
| Station | `35.0°`, `-116.0°`, `1000 m` | Synthetic generic ground site for development. |
| TLE | Public ISS (ZARYA) reference TLE | Public data only; see compliance notes below. |
| Nominal carrier | `437_500_000 Hz` | Used by Doppler validation when RF metadata is available. |
| Doppler tolerance | `±150 Hz` | See `TEST_PLAN.md` (`T-DOPPLER`) and `Methodology.md` (D-012). |
| Minimum elevation | `0°` | Frames below the mathematical horizon set `physics_flags` bit 1. |
| Tracking throttle | `10 ms` | Reuses the last tracking state for frames in the same window. |

Use `RUST_LOG` to control `tracing_subscriber` output, for example:

```bash
RUST_LOG=info cargo run
```

### Send a synthetic CCSDS telemetry packet

With the gateway running, send a small CCSDS telemetry packet over loopback:

```bash
python3 - <<'PY'
import socket

# TM packet, APID 0x02A, sequence 7, unsegmented, 5-byte payload "hello".
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

Expected result: the gateway logs a `telemetry frame parsed` event with `apid=42`, `seq=7`,
payload length `5`, tracking fields, and `physics_flags`. Doppler is skipped in the binary today
because no SDR side-channel supplies `RfMetadata::measured_carrier_hz`; elevation validation still
runs when tracking state is available.

### Operational notes and common pitfalls

- `Ctrl-C` triggers graceful shutdown and logs cumulative ingest counters.
- `dropping invalid/non-telemetry datagram` usually means the bytes were too short, malformed,
  declared a payload longer than the datagram, or were telecommand (TC) rather than telemetry (TM).
- `physics_flags` is a stable bitfield for downstream consumers:
  - bit 0 (`0x01`) = Doppler anomaly;
  - bit 1 (`0x02`) = below configured elevation threshold;
  - bit 2 (`0x04`) = reserved for future RSSI/link-budget validation.
- The default TLE is intentionally public and generic. Do not commit real mission keys,
  controlled frequencies, or controlled operational parameters.
- The Open MCT/Axum distribution layer is planned for Milestone 5; today validated frames are
  demonstrated through logs and the library API.

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

Current coverage is split between `tests/ingest.rs` for the loopback UDP ingestion integration
suite and inline unit tests for CCSDS parsing, station configuration/tracking, and co-validation.
The full ingest → parse → validate → WebSocket integration test is intentionally deferred to
Milestone 5 with the distribution layer.

---

## Public library surface

The `chronus_gateway` crate re-exports the main building blocks for tests, demos, and future
distribution code:

- ingestion: `IngestConfig`, `RawFrame`, `IngestStats`, `ingest::bind`, `ingest::run`;
- CCSDS parsing: `parse_telemetry`, `TelemetryFrame`, `CcsdsError`;
- station/tracking: `StationConfig`, `TleSource`, `ConfigError`, `OrbitalPropagator`,
  `EphemerustPropagator`, `TrackingProvider`, `TrackingState`;
- co-validation: `apply_physics_validation`, `expected_carrier_hz`, `RfMetadata`,
  `FLAG_DOPPLER_ANOMALY`, `FLAG_BELOW_HORIZON`, `FLAG_RSSI_RESERVED`.

See the module-level rustdoc in `crates/gateway/src/*.rs` for API constraints and examples.

---

## References

- [`BUILD_PLAN.md`](BUILD_PLAN.md) — stage-gated implementation roadmap and current milestone
  scope.
- [`TEST_PLAN.md`](TEST_PLAN.md) — test matrix, status counts, and physics tolerance register.
- [`Methodology.md`](Methodology.md) — decision log, trade-offs, and attribution details.
- [`AGENTS.md`](AGENTS.md) — compliance, security, attribution, and testing rules.
- CCSDS Space Packet protocol documentation from the Consultative Committee for Space Data
  Systems, used through the `spacepackets` crate for open-standard packet parsing.
- Ephemerust and its `sgp4` dependency, used for SGP4-based look-angle and range-rate
  computation.

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
