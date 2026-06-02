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
> Physics–Telemetry Co-Validation engine (M4) are implemented and tested. The current binary runs
> a local UDP ingest → parse → tracking → validation logging pipeline. Open MCT WebSocket
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
  subscribers. The ingestion channel is bounded and intentionally lossy: slow consumers see lag,
  but never stall the UDP receive loop.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
  `OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
  library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
  clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.

The reasoning behind these and other choices is recorded in [`Methodology.md`](Methodology.md).

---

## Current pipeline contracts

The implemented M1–M4 path is intentionally narrow and source-backed:

1. **UDP ingestion (`ingest`).** `IngestConfig::default()` binds `127.0.0.1:7301`, uses a
   1,024-frame broadcast channel, and caps datagrams at 65,542 bytes. Each datagram becomes a
   `RawFrame { bytes: Arc<[u8]>, received_at, source }`. Oversized input is bounded by the fixed
   receive buffer; the loop continues after recoverable socket errors.
2. **CCSDS parsing (`ccsds`).** `parse_telemetry(&RawFrame)` validates the 6-byte CCSDS Space
   Packet primary header, declared payload length, and packet type. Telemetry (TM) packets become
   `TelemetryFrame`; telecommands (TC), short headers, truncated payloads, and malformed headers
   return structured `CcsdsError`s. The payload is a zero-copy borrow into the retained datagram.
3. **Tracking (`propagator`).** `StationConfig` resolves an inline or file-based TLE, validates
   station fields, and builds `EphemerustPropagator`. `TrackingProvider` wraps any
   `OrbitalPropagator` and caches states inside `min_recompute_interval_ms` (default 10 ms) to
   avoid redundant SGP4 work during bursts.
4. **Physics co-validation (`validate`).** `apply_physics_validation` clears and sets
   `TelemetryFrame::physics_flags` from Doppler and elevation checks. Doppler is skipped when no
   measured carrier is present.

`physics_flags` is the stable anomaly bitfield exposed to downstream consumers:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Doppler anomaly: measured carrier differs from expected beyond tolerance. |
| 1 | `0x02` | Horizon/elevation anomaly: predicted elevation is below the configured minimum. |
| 2 | `0x04` | Reserved for RSSI / link-budget validation; not set yet. |

Current limitations are deliberate milestone boundaries: no CLI/env configuration loader yet, no
SDR side-channel for measured RF metadata yet, and no HTTP/WebSocket distribution until M5.

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

## Setup and running

The project targets Rust 1.88 or newer and consumes the Ephemerust library as a sibling
checkout. The expected on-disk layout places both repositories next to each other:

```
…/Rust/
├── chronus-gateway/
└── Ephemerust/
```

```bash
cargo build      # compile the workspace
cargo run        # run the UDP ingest/parse/validation logger on 127.0.0.1:7301
cargo test       # unit + integration + doctests
```

To exercise the current binary with a synthetic, public CCSDS telemetry packet:

```bash
# terminal 1
RUST_LOG=info cargo run

# terminal 2: TM packet, APID 0x02a, sequence 7, payload "hello"
python - <<'PY'
import socket
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

Expected result: the gateway logs `telemetry frame parsed` with APID `42`, sequence `7`, payload
length `5`, and either physics fields/flags or `telemetry frame parsed (no physics state)` if the
propagator cannot produce a state for that receive timestamp. Invalid or non-telemetry datagrams
are logged and dropped; the receive loop keeps running until Ctrl-C.

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Developer runbook and common pitfalls

- **Missing Ephemerust checkout.** If builds fail with a missing `../Ephemerust` path dependency,
  clone or place Ephemerust as a sibling directory to this repository. This is intentional for
  local co-development and recorded in `Methodology.md` D-005.
- **Port already in use.** The binary currently uses `IngestConfig::default()` and binds
  `127.0.0.1:7301`. Library tests bind loopback port `0`; changing the runtime bind address is a
  code/configuration integration task planned for later milestones.
- **No parsed-frame log after sending UDP.** Confirm the packet is CCSDS TM, includes at least the
  6-byte primary header, and that the `data length` field equals `payload_len - 1`. TC packets and
  truncated packets are rejected by design.
- **Slow consumers.** The broadcast channel is lossy by design. Lagging subscribers must handle
  `RecvError::Lagged`; this protects the socket loop and favors fresh telemetry over stale frames.
- **Physics flags.** Bit 0 requires `RfMetadata::measured_carrier_hz = Some(f64)`; the current
  binary passes no measured carrier, so only elevation can be flagged by the demo path.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

Sources used by the implementation and documentation:

- **CCSDS Space Packet Protocol, CCSDS 133.0-B-2** — public packet framing standard used by the
  `ccsds` module's primary-header parsing and TM/TC distinction.
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — Rust CCSDS/ECSS packet library used
  to decode Space Packet primary headers. License: Apache-2.0/MIT.
- **[Ephemerust](https://github.com/IsomorphicAlgo/ephemerust)** — sibling Rust astrodynamics
  crate providing SGP4 look angles and range rate for station-relative tracking.
- **[`sgp4`](https://crates.io/crates/sgp4)** — SGP4/SDP4 propagation crate used by Ephemerust.
- **[Tokio](https://tokio.rs/)** — async runtime and UDP/broadcast primitives used by ingestion.
- **[Tracing](https://crates.io/crates/tracing)** and
  **[`tracing-subscriber`](https://crates.io/crates/tracing-subscriber)** — structured runtime
  logging used by the binary and ingest loop.
- **[Chrono](https://crates.io/crates/chrono)** — UTC timestamps on raw and parsed frames.
- **[NASA Open MCT](https://nasa.github.io/openmct/)** — target dashboard for the planned M5
  WebSocket distribution layer.
- **Project governance:** [`AGENTS.md`](AGENTS.md), [`Methodology.md`](Methodology.md),
  [`BUILD_PLAN.md`](BUILD_PLAN.md), and [`TEST_PLAN.md`](TEST_PLAN.md).

---

## Acknowledgements

ChronusGateway-RS builds directly on prior work, and credit is given accordingly:

- **[Ephemerust](https://github.com/IsomorphicAlgo/ephemerust)** — the SGP4-based orbital
  mechanics and satellite-tracking library that provides the look-angle and range-rate
  computations underpinning the co-validation engine. Authored by the same maintainer.
- **Rusty_Server** — an earlier asynchronous networking and REST service by the same maintainer,
  whose Tokio/Axum architecture and integration patterns informed this gateway's design.
- **CCSDS and the public space-systems standards community** — the open international packet and
  telemetry standards that keep this project in an open, non-proprietary scope.
- **The `spacepackets` maintainers** — for the CCSDS/ECSS parsing crate used at the packet
  boundary.
- **[`sgp4`](https://crates.io/crates/sgp4)** — the validated propagator that Ephemerust delegates
  to for numerical orbit propagation.
- **[Tokio](https://tokio.rs/)**, **[Axum](https://github.com/tokio-rs/axum)**, and the Rust async
  ecosystem — the runtime and web-service foundation for the current ingestion loop and planned
  distribution layer.
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
