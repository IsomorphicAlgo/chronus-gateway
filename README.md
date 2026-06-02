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
│   │   └── main.rs         Entrypoint (ingest → parse → tracking → validation logging)
│   └── tests/
│       └── ingest.rs       Loopback UDP ingestion integration tests
├── AGENTS.md               Project constitution (compliance, attribution, security, testing)
├── Methodology.md          Decision log: the reasoning behind major choices
├── BUILD_PLAN.md           Iterative, stage-gated implementation roadmap
└── TEST_PLAN.md            Companion test plan and tolerance register
```

---

## Current data path (implemented through M4)

1. **Bind UDP** on [`IngestConfig::bind_addr`](crates/gateway/src/config.rs) (default
   `127.0.0.1:7301`) and receive datagrams into a fixed-size buffer.
2. **Broadcast raw frames** as cheap-clone `RawFrame { bytes: Arc<[u8]>, received_at, source }`.
   The channel is bounded and intentionally lossy so slow consumers cannot stall the socket.
3. **Parse CCSDS telemetry** with [`ccsds::parse_telemetry`](crates/gateway/src/ccsds.rs):
   short, malformed, truncated, or telecommand packets return structured `CcsdsError`s and are
   dropped by the demo consumer.
4. **Compute tracking state** with `TrackingProvider`, which wraps an `OrbitalPropagator` and
   caches look-angle/range-rate results for the configured recompute interval.
5. **Apply physics validation** with
   [`apply_physics_validation`](crates/gateway/src/validate.rs), setting `TelemetryFrame::physics_flags`:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Doppler anomaly: measured carrier is outside `doppler_tolerance_hz` from expected. |
| 1 | `0x02` | Elevation anomaly: predicted elevation is below `minimum_elevation_deg`. |
| 2 | `0x04` | Reserved for future RSSI / link-budget validation; not set today. |

The current binary logs the parsed/validated frames. WebSocket/Open MCT distribution is still the
next milestone, so there is no stable JSON or HTTP API yet.

---

## Public interfaces

The `chronus_gateway` library re-exports the main interfaces from `lib.rs`:

| Area | Key APIs | Notes |
|------|----------|-------|
| Ingestion | `IngestConfig`, `RawFrame`, `IngestStats`, `ingest::bind`, `ingest::run` | Async UDP receive loop with explicit shutdown future and counters. |
| CCSDS parsing | `TelemetryFrame`, `CcsdsError`, `ccsds::parse_telemetry` | Validates the CCSDS primary header and exposes zero-copy payload borrows. |
| Station / TLE config | `StationConfig`, `TleSource`, `ConfigError` | Validates numeric station fields, inline TLE text, and file-based TLE loading. |
| Propagation | `OrbitalPropagator`, `EphemerustPropagator`, `TrackingProvider`, `TrackingState` | Trait seam isolates the gateway from the astrodynamics backend. |
| Co-validation | `RfMetadata`, `expected_carrier_hz`, `apply_physics_validation`, `FLAG_*` constants | Doppler check is skipped when no measured carrier is provided. Elevation still runs. |

---

## Building, running, and local smoke testing

The project targets Rust 1.88 or newer and consumes the Ephemerust library as a sibling
checkout. The expected on-disk layout places both repositories next to each other:

```
…/Rust/
├── chronus-gateway/
└── Ephemerust/
```

```bash
cargo build      # compile the workspace
cargo run        # run the local UDP ingestion/validation demo
cargo test       # unit + integration + doctests
```

To exercise the binary manually, run `cargo run`, then send a synthetic CCSDS telemetry packet from
another terminal:

```bash
python3 - <<'PY'
import socket

# CCSDS TM primary header: APID 0x02a, sequence 7, unsegmented, 5-byte payload "hello".
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

The demo consumer should log a parsed telemetry frame. Doppler bit 0 will not be set by this demo
because SDR carrier metadata is not wired yet (`RfMetadata::measured_carrier_hz` is `None`).

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Troubleshooting and common pitfalls

- **`../Ephemerust` is missing:** clone the owner's Ephemerust repository next to this checkout.
  In this cloud workspace that usually means `/Ephemerust` as the sibling of `/workspace`.
- **Rust version is too old:** install or select a Rust toolchain that satisfies MSRV 1.88, then
  rerun the command with that toolchain.
- **Port already in use:** change `IngestConfig::bind_addr` in code/tests or stop the other local
  listener on `127.0.0.1:7301`.
- **No parsed frames:** the ingestion loop accepts UDP bytes, but `ccsds::parse_telemetry` drops
  datagrams that are shorter than 6 bytes, declare more payload than arrived, or are TC packets.
- **Unexpected frame loss under load:** this is intentional for now. The broadcast channel is
  bounded and lossy; consumers must treat `RecvError::Lagged` as a signal that stale frames were
  skipped.
- **No Doppler anomaly in the demo:** measured carrier frequency is optional and not supplied by
  the current binary. Use `RfMetadata { measured_carrier_hz: Some(...) }` in tests or future SDR
  wiring to exercise bit 0.
- **Old TLEs:** the default ISS TLE is public reference data for deterministic development. For
  operational-style experiments, provide a current public TLE via `TleSource::Inline` or
  `TleSource::File`.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

The implementation and documentation are grounded in these public sources:

- [CCSDS Space Packet Protocol](https://public.ccsds.org/) — open standard used for the telemetry
  packet primary-header model.
- [`spacepackets` crate](https://crates.io/crates/spacepackets) — Rust/ECSS packet parsing crate
  wrapped by the local `ccsds` module.
- [Ephemerust](https://github.com/IsomorphicAlgo/ephemerust) — sibling SGP4/look-angle library
  used by `EphemerustPropagator`.
- [`sgp4` crate](https://crates.io/crates/sgp4) — SGP4/SDP4 numerical propagation used by
  Ephemerust.
- [Tokio](https://tokio.rs/) — async runtime used for UDP ingestion, shutdown, and broadcast
  fan-out.
- [Axum](https://github.com/tokio-rs/axum) and
  [NASA Open MCT](https://nasa.github.io/openmct/) — planned Milestone 5 distribution stack and
  dashboard target.

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
