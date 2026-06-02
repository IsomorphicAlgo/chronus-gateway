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

## Implemented workflow (Milestones 1-4)

The checked-in gateway currently runs the first half of the telemetry pipeline:

1. **UDP ingest** (`ingest::run`) binds `127.0.0.1:7301` by default, receives datagrams into a fixed
   buffer, and broadcasts `RawFrame` values on a lossy `tokio::sync::broadcast` channel.
2. **CCSDS parse** (`ccsds::parse_telemetry`) validates the Space Packet primary header, rejects TC
   packets on the telemetry path, and exposes the packet data field through a zero-copy
   `TelemetryFrame::payload()` borrow.
3. **Tracking state** (`TrackingProvider`) computes or reuses an Ephemerust-backed
   `TrackingState` for the frame timestamp, throttled by `StationConfig::min_recompute_interval_ms`.
4. **Physics validation** (`validate::apply_physics_validation`) clears and sets
   `TelemetryFrame::physics_flags`:

   | Bit | Mask | Meaning |
   |-----|------|---------|
   | 0 | `0x01` | Doppler anomaly: measured carrier differs from expected by more than the configured tolerance. |
   | 1 | `0x02` | Below configured minimum elevation. |
   | 2 | `0x04` | Reserved for future RSSI / link-budget validation; not set today. |

The binary is still a development runner: it logs parsed frames and validation flags, but does not
yet expose WebSocket/Open MCT distribution (Milestone 5).

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

## Public Rust interfaces

The crate re-exports the current public API from `chronus_gateway`:

- `IngestConfig`, `RawFrame`, `IngestStats`, and `ingest::bind` / `ingest::run` for UDP capture.
- `TelemetryFrame`, `CcsdsError`, and `ccsds::parse_telemetry` for CCSDS telemetry parsing.
- `StationConfig`, `TleSource`, `EphemerustPropagator`, `OrbitalPropagator`, `TrackingProvider`,
  and `TrackingState` for station-aware orbit geometry.
- `RfMetadata`, `expected_carrier_hz`, `apply_physics_validation`, and the `FLAG_*` constants for
  Physics-Telemetry Co-Validation.

All network-facing inputs are bounded and recoverable by design: malformed/truncated CCSDS packets
return structured errors, oversized UDP datagrams do not drive unbounded allocation, and slow
consumers lag instead of blocking the socket loop.

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
cargo run        # run the development ingestion/validation gateway
cargo test       # unit + integration + doctests
```

To exercise the implemented UDP path locally, run the gateway in one terminal:

```bash
cargo run -p chronus-gateway
```

Then send a synthetic CCSDS telemetry packet from another terminal:

```bash
python3 - <<'PY'
import socket

# TM packet, APID 0x02a, sequence 1, 5-byte packet data field "hello".
packet = bytes([0x00, 0x2A, 0xC0, 0x01, 0x00, 0x04]) + b"hello"
socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(packet, ("127.0.0.1", 7301))
PY
```

Expected result: the gateway logs `telemetry frame parsed` with `apid=42`, `seq=1`, and a
`physics_flags` value when an Ephemerust tracking state is available for the frame timestamp.
Without SDR metadata, Doppler bit 0 is skipped; the elevation gate can still set bit 1.

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Developer runbook and pitfalls

- **Sibling dependency:** keep `Ephemerust/` next to this repo (`../Ephemerust` from the workspace
  root). A missing checkout fails dependency resolution before compilation starts.
- **Toolchain:** use Rust 1.88 or newer. If the local default is older, run commands with a newer
  toolchain, for example `cargo +stable test`.
- **Bind address:** development defaults to loopback (`127.0.0.1:7301`). Change `IngestConfig` in
  code for a different interface until external configuration lands.
- **TLE freshness:** the default ISS TLE is public reference data for deterministic development.
  If propagation fails at a far-future timestamp, ingestion and parsing still continue and the
  binary logs the frame without physics state.
- **Backpressure:** the broadcast channel is intentionally lossy. Treat `Lagged` as a signal to
  increase capacity, reduce consumer work, or prefer a different delivery contract for archival use.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- Consultative Committee for Space Data Systems (CCSDS), **Space Packet Protocol**, public
  recommended standards: <https://public.ccsds.org/>.
- `spacepackets` crate documentation and source for CCSDS/ECSS packet handling:
  <https://crates.io/crates/spacepackets>.
- Ephemerust, the project's sibling astrodynamics crate:
  <https://github.com/IsomorphicAlgo/ephemerust>.
- `sgp4` crate documentation for the SGP4/SDP4 propagation backend used by Ephemerust:
  <https://crates.io/crates/sgp4>.
- Tokio asynchronous runtime documentation: <https://tokio.rs/>.
- Axum web framework documentation, planned for Milestone 5 distribution:
  <https://github.com/tokio-rs/axum>.
- NASA Open MCT documentation for the planned dashboard integration:
  <https://nasa.github.io/openmct/>.

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
