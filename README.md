# ChronusGateway-RS

ChronusGateway-RS (Chronus-GS) is an asynchronous, physics-validated Telemetry and Command
(TMTC) ground-station gateway written in Rust. It ingests raw spacecraft downlink frames,
parses them against open CCSDS standards, cross-checks each frame against the spacecraft's
computed orbital physics, and is being built to distribute validated telemetry to web-based
mission control dashboards such as NASA Open MCT — in a single, memory-safe,
garbage-collection-free executable.

Its distinguishing feature is a **Physics-Telemetry Co-Validation** engine: rather than checking
telemetry only against static limits, the gateway uses a live orbital propagator to derive the
expected Doppler shift, look-angles, and link geometry for the spacecraft, and flags frames whose
measured RF and signal parameters disagree with the physics when those measurements are available.

> **Status:** Early development. Ingestion (M1), CCSDS parsing (M2), station tracking (M3), and the
> Physics–Telemetry Co-Validation engine (M4) are implemented and tested. Open MCT WebSocket
> distribution is Milestone 5 (see [`BUILD_PLAN.md`](BUILD_PLAN.md)).

---

## Architecture

The gateway is built around two principles: a bounded asynchronous network core and a clean
abstraction boundary between the telemetry pipeline and any astrodynamics backend.

```
Raw CCSDS over UDP ──▶ Async UDP ingestion ──▶ CCSDS zero-copy parser ──▶ TelemetryFrame
 (SDR/front-end)          (Tokio)                 (spacepackets)              │
                                                                            ▼
StationConfig ──▶ TrackingProvider cache ──▶ OrbitalPropagator trait ──▶ Physics-Telemetry
                  (100 Hz default)          (Ephemerust today)          Co-Validation
                                                                            │
                                                                            ▼
                                                              Logs today; Axum WebSocket
                                                              + NASA Open MCT in M5
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion into a bounded, lossy
  broadcast channel. Slow consumers observe lag instead of blocking the receive loop.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
  `OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
  library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
  clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.
- **Stable validation flags.** `validate::apply_physics_validation` writes anomaly bits into
  `TelemetryFrame::physics_flags`: bit 0 = Doppler, bit 1 = below the configured elevation mask,
  bit 2 = reserved for RSSI/link-budget work. The production path currently has no SDR-measured
  carrier side channel, so Doppler is skipped when `RfMetadata::measured_carrier_hz` is `None`;
  elevation validation still runs when tracking state is available.

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
│   │   ├── validate.rs     Physics–Telemetry Co-Validation (Doppler, elevation, physics_flags)
│   │   ├── propagator.rs   OrbitalPropagator trait + Ephemerust-backed implementation
│   │   └── main.rs         Entrypoint (wires ingest → parse → track → validate → log)
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
cargo run        # bind 127.0.0.1:7301 and run the M1-M4 pipeline until Ctrl-C
cargo test       # unit + integration + doctests
```

The default development configuration is deliberately synthetic/public:

- UDP bind address: `127.0.0.1:7301`.
- Ingestion channel: 1024-frame lossy broadcast; max datagram size 65,542 bytes.
- Station: 35.0°N, 116.0°W, 1000 m altitude.
- Tracked object: public ISS (ZARYA) reference TLE from `config::DEFAULT_ISS_TLE`.
- Nominal carrier / validation: 437.5 MHz, ±150 Hz Doppler tolerance, 0° minimum elevation.

To exercise the current binary after `cargo run`, send a small synthetic CCSDS telemetry packet:

```bash
python3 - <<'PY'
import socket

# TM packet, APID 0x02A, sequence 7, 5-byte data field "hello".
packet = bytes.fromhex("002ac0070004") + b"hello"
socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(packet, ("127.0.0.1", 7301))
PY
```

The gateway validates the CCSDS primary header, computes tracking state when the propagator can
serve the frame timestamp, applies the available physics checks, and logs either a parsed telemetry
frame or a structured recoverable error for malformed/non-telemetry datagrams.

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and, when M5 lands, in-process WebSockets; doctests; and physics
co-validation tests with explicitly documented tolerances. Current coverage is 24 unit tests, 4
ingestion integration tests, and 1 doctest. The full strategy and per-milestone test matrix are
defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- **CCSDS 133.0-B-2, Space Packet Protocol.** Defines the Space Packet primary header parsed by
  `ccsds.rs`; only open, public CCSDS framing is used in examples and tests.
- **Project design document / milestone PDF.** Source for the staged roadmap, the co-validation
  model, and provisional Doppler/RSSI tolerances; details are captured in `BUILD_PLAN.md`,
  `TEST_PLAN.md`, and `Methodology.md`.
- **Ephemerust documentation and tests.** Source for the SGP4 look-angle/range-rate API and the
  tolerance style reused by this gateway.
- **Rust crate documentation.** `tokio`, `spacepackets`, `tracing`, `serde`, `chrono`, `anyhow`,
  and `thiserror` API docs guide the current implementation.

## Acknowledgements

ChronusGateway-RS builds directly on prior work. Thanks to the maintainers and communities behind:

- **[Ephemerust](https://github.com/IsomorphicAlgo/ephemerust)** — the SGP4-based orbital
  mechanics and satellite-tracking library that provides the look-angle and range-rate
  computations underpinning the co-validation engine. Authored by the same maintainer.
- **Rusty_Server** — an earlier asynchronous networking and REST service by the same maintainer,
  whose Tokio/Axum architecture and integration patterns informed this gateway's design.
- **[`sgp4`](https://crates.io/crates/sgp4)** — the validated SGP4/SDP4 propagator that
  Ephemerust delegates to for numerical orbit propagation.
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the CCSDS/ECSS packet library used
  for Space Packet primary-header parsing.
- **[Tokio](https://tokio.rs/)**, **[`tracing`](https://crates.io/crates/tracing)**,
  **[`anyhow`](https://crates.io/crates/anyhow)**, **[`thiserror`](https://crates.io/crates/thiserror)**,
  **[`chrono`](https://crates.io/crates/chrono)**, and **[`serde`](https://serde.rs/)** — the Rust
  runtime, observability, error, time, and serialization crates used by the gateway.
- **[Axum](https://github.com/tokio-rs/axum)** — the planned M5 WebSocket/HTTP framework, following
  the Rusty_Server architecture pattern.
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
