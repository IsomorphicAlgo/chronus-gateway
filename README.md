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
Synthetic CCSDS / SDR UDP ──▶ Async UDP ingestion ──▶ CCSDS zero-copy parser
                                 (Tokio)                  (validated TM)
                                                              │
                  OrbitalPropagator trait ◀── range-rate / look-angles
                  (Ephemerust today, nyx-space later)         │
                                                              ▼
                                               Physics-Telemetry Co-Validation
                                                              │
                                                              ▼
                                                   tracing logs today
                                                   Axum WebSocket / Open MCT in M5
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion today. Axum
  WebSocket fan-out to operator screens is the next milestone, not yet part of the executable.
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
│   │   ├── config.rs       Ingestion, station, propagator, and validation defaults
│   │   ├── ingest.rs       Asynchronous UDP ingestion loop (RawFrame, stats, shutdown)
│   │   ├── ccsds.rs        CCSDS Space Packet parsing (TelemetryFrame, validation)
│   │   ├── validate.rs     Physics–Telemetry Co-Validation (Doppler, elevation, physics_flags)
│   │   ├── propagator.rs   OrbitalPropagator trait + Ephemerust-backed implementation
│   │   └── main.rs         M1–M4 demo pipeline (ingest → parse → track → validate → log)
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
cargo run        # run the UDP ingest → parse → track → validate demo
cargo test       # unit + integration + doctests
```

`cargo run` currently uses hard-coded development defaults from `IngestConfig::default()` and
`StationConfig::default()`; there is no CLI or config file yet. It binds UDP
`127.0.0.1:7301`, initializes the default ISS TLE-backed station tracker, consumes incoming
datagrams, accepts CCSDS telemetry packets, computes a tracking state at the frame's
`received_at` timestamp, applies physics validation, and logs the result with `tracing`
(`RUST_LOG` overrides the default `info` filter).

Minimal smoke test from another shell while `cargo run` is active:

```bash
python3 - <<'PY'
import socket

# CCSDS TM packet: APID 0x02a, sequence 7, unsegmented, 5-byte payload "hello".
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(packet, ("127.0.0.1", 7301))
PY
```

A valid packet logs `telemetry frame parsed` with APID, sequence count, payload length, tracking
state, and `physics_flags` when the propagator is available. Invalid, truncated, or TC packets are
logged and dropped without stopping the receive loop. Stop the demo with Ctrl-C.

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

## Operational notes and current contracts

- **Ingestion:** the default socket is loopback-only (`127.0.0.1:7301`) with a bounded, lossy
  broadcast channel (capacity `1024`). Slow consumers see `Lagged`; the socket loop keeps reading.
- **Datagram size:** the receive buffer is fixed at `65_542` bytes (CCSDS max packet size plus
  primary header). Oversized datagrams are dropped on Windows or truncated by the OS on Unix, then
  rejected by CCSDS length checks if incomplete.
- **Tracking:** `main` uses the frame capture time (`Utc::now()` at receive) for propagation. The
  default public ISS TLE is useful for deterministic tests near its 2020 epoch; runtime physics
  flags depend on how stale that TLE is at execution time.
- **Propagator fallback:** if station validation or TLE parsing fails, the gateway continues
  ingesting and parsing telemetry and logs `telemetry frame parsed (no physics state)`.
- **`physics_flags`:** bit `0x01` = Doppler anomaly, bit `0x02` = below configured minimum
  elevation, bit `0x04` = RSSI/link-budget reserved. The executable currently passes
  `RfMetadata::default()`, so Doppler is skipped until measured carrier metadata is wired in.

---

## References

- **CCSDS Space Packet Protocol (CCSDS 133.0-B-2):** public packet framing standard used by the
  parser boundary.
- **[`spacepackets` 0.17](https://crates.io/crates/spacepackets):** CCSDS/ECSS packet parsing
  crate wrapped by the local `ccsds` module.
- **[Ephemerust](https://github.com/IsomorphicAlgo/ephemerust)** and the
  **[`sgp4`](https://crates.io/crates/sgp4)** crate: SGP4 look-angle and range-rate source for
  the default propagator.
- **[Tokio](https://tokio.rs/)**, **[Tracing](https://docs.rs/tracing/)**, and
  **[Axum](https://github.com/tokio-rs/axum):** async runtime, observability, and planned
  WebSocket/HTTP framework.
- **[NASA Open MCT](https://nasa.github.io/openmct/):** target operator dashboard for Milestone 5
  distribution.

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
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the Apache-2.0/MIT CCSDS/ECSS
  packet library used for Space Packet header parsing.
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
