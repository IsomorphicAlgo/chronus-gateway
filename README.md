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
                                                         M5 WebSocket ──▶ NASA Open MCT
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion and a pool of
  future WebSocket connections, scaling to many concurrent telemetry channels and operator
  screens without letting slow consumers block the socket loop.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
  `OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
  library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
  clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.
- **Stable anomaly contract.** The co-validation engine writes a `physics_flags` bitfield onto
  each parsed `TelemetryFrame`: bit 0 = Doppler anomaly, bit 1 = below the configured elevation
  threshold, bit 2 = reserved for future RSSI/link-budget checks.

The reasoning behind these and other choices is recorded in [`Methodology.md`](Methodology.md).

### Current runtime workflow (M1-M4)

The executable currently wires a demonstration pipeline that exercises the implemented core:

1. `IngestConfig::default()` binds UDP loopback `127.0.0.1:7301`, caps datagrams at 65,542 bytes,
   and fans frames out through a bounded, lossy `tokio::sync::broadcast` channel.
2. `StationConfig::default()` validates a public ISS TLE, observer location (`35°N`, `116°W`,
   `1000 m`), nominal carrier (`437.5 MHz`), `150 Hz` Doppler tolerance, `0°` minimum elevation,
   and a `10 ms` tracking recompute throttle.
3. A consumer task parses each `RawFrame` with `ccsds::parse_telemetry`, computes a throttled
   `TrackingState` through `TrackingProvider`, applies `validate::apply_physics_validation`, and
   logs APID/sequence/payload/tracking values plus `physics_flags`.
4. Until SDR RF metadata is wired, the runtime passes `RfMetadata::default()`, so Doppler bit 0 is
   skipped in the executable; the elevation bit still runs whenever tracking state is available.
5. Slow consumers see `RecvError::Lagged` and the newest telemetry keeps flowing.

Only public/synthetic examples and public reference TLEs belong in this repository; do not commit
real mission keys, frequencies, controlled performance data, or operational parameters.

### Public interfaces and common pitfalls

- Use `IngestConfig`, `ingest::bind`, and `ingest::run` to embed the UDP receive loop in tests or
  future services. Subscribers must handle `tokio::sync::broadcast::error::RecvError::Lagged`.
- Use `ccsds::parse_telemetry(&RawFrame)` for CCSDS Space Packet validation. It accepts telemetry
  packets only, ignores bytes beyond the declared packet length, and returns structured
  `CcsdsError` values for malformed/truncated/non-TM input.
- Use `StationConfig::validate()` and `StationConfig::resolve_tle_text()` before constructing an
  `EphemerustPropagator`; inline and file TLE sources are supported, while live catalog fetches are
  intentionally deferred.
- Use `TrackingProvider` when many frames share nearby timestamps; it recomputes outside its cache
  lock and reuses states inside the configured throttle window.
- If the executable logs parsed frames with `physics_flags = 0` while you expected Doppler checks,
  remember that `main.rs` currently passes no measured carrier metadata. Unit tests cover Doppler
  by providing `RfMetadata { measured_carrier_hz: Some(...) }`.

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
│   │   └── main.rs         Entrypoint (UDP → CCSDS → tracking → validation)
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
cargo run        # run the UDP ingestion + parse + validation pipeline on 127.0.0.1:7301
cargo test       # unit + integration + doctests
```

To smoke-test the running binary with a synthetic CCSDS telemetry packet:

```bash
python3 - <<'PY'
import socket

# CCSDS TM primary header: APID 0x02A, sequence 7, unsegmented, 5-byte payload "hello".
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

Run with `RUST_LOG=info` (the default) to see parsed telemetry logs, or `RUST_LOG=debug` when
tracing future distribution/consumer work.

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

- **Implementation codepaths:** `crates/gateway/src/ingest.rs`, `ccsds.rs`, `config.rs`,
  `propagator.rs`, `validate.rs`, and `main.rs` are the source of truth for the M1-M4 runtime.
- **CCSDS Space Packet Protocol:** CCSDS 133.0-B-2 defines the primary-header fields parsed by the
  gateway. Secondary-header/PUS interpretation is planned but not decoded by the current pipeline.
- **`spacepackets` 0.17:** Rust crate used behind the `ccsds` module for CCSDS primary-header
  decoding; the rest of the gateway depends on `TelemetryFrame`.
- **Ephemerust and `sgp4`:** Ephemerust provides look angles and range rate through SGP4; the
  gateway consumes those values through `OrbitalPropagator`.
- **Tokio, Tracing, Serde, Anyhow, Thiserror:** core Rust infrastructure crates for async I/O,
  observability, serialization, and recoverable error reporting.
- **NASA Open MCT and Axum:** target dashboard and planned WebSocket/HTTP stack for Milestone 5.
- **NeXosim:** planned hardware-in-the-loop simulation reference for the stretch validation
  milestone.

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
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the CCSDS packet parsing crate used
  by the gateway's M2 parser boundary.
- **[Tokio](https://tokio.rs/)** and **[Axum](https://github.com/tokio-rs/axum)** — the
  asynchronous runtime and planned web framework for the network core and distribution layer.
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
