# ChronusGateway-RS

ChronusGateway-RS (Chronus-GS) is an asynchronous, physics-validated Telemetry and Command
(TMTC) ground-station gateway written in Rust. It ingests raw spacecraft downlink frames,
parses them against open CCSDS standards, cross-checks each frame against the spacecraft's
computed orbital physics, and is designed to distribute validated telemetry to web-based mission
control dashboards such as NASA Open MCT — in a single, memory-safe, garbage-collection-free
executable.

Its distinguishing feature is a **Physics-Telemetry Co-Validation** engine: rather than checking
telemetry only against static limits, the gateway uses a live orbital propagator to derive the
expected Doppler shift, look-angles, and link geometry for the spacecraft, and flags frames whose
measured RF and signal parameters disagree with the physics.

> **Status:** Early development. Ingestion (M1), CCSDS parsing (M2), station tracking (M3), and the
> Physics–Telemetry Co-Validation engine (M4) are implemented and tested. Open MCT WebSocket
> distribution is Milestone 5 (see [`BUILD_PLAN.md`](BUILD_PLAN.md)).

---

## Architecture

The gateway is built around two principles: an asynchronous, non-blocking network core and a
clean abstraction boundary between the pipeline and any astrodynamics backend.

```
Raw RF / SDR ──▶ Async UDP ingestion ──▶ CCSDS zero-copy parser ──▶ Physics-Telemetry
  (UDP now)        (Tokio broadcast)        (TelemetryFrame)          Co-Validation engine
                                                                            │
                  OrbitalPropagator trait ◀── range-rate / look-angles ─────┤
                  (Ephemerust today, nyx-space later)                       ▼
                                                              validated logs
                                                              M5: WebSocket
                                                              + NASA Open MCT
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion and a pool of
  downstream subscribers. The current binary logs parsed and physics-validated frames; WebSocket
  fan-out is the next milestone.
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
│   │   ├── config.rs       Ingestion + station/validation configuration
│   │   ├── ingest.rs       Asynchronous UDP ingestion loop (RawFrame, stats, shutdown)
│   │   ├── ccsds.rs        CCSDS Space Packet parsing (TelemetryFrame, validation)
│   │   ├── validate.rs     Physics–Telemetry Co-Validation (Doppler, elevation, physics_flags)
│   │   ├── propagator.rs   OrbitalPropagator trait + Ephemerust-backed implementation
│   │   └── main.rs         Entrypoint (ingest → parse → track → validate → log)
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
cargo run        # run the UDP gateway with CCSDS parsing + physics validation
cargo test       # unit + integration + doctests
```

By default, `cargo run` binds UDP on `127.0.0.1:7301`, builds the default ISS-backed station
tracker, parses inbound CCSDS telemetry packets, applies physics validation, and logs the result.
Stop it with Ctrl-C.

Minimal local smoke frame (valid CCSDS TM primary header, APID `0x02A`, payload `hello`):

```bash
python3 - <<'PY'
import socket
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(packet, ("127.0.0.1", 7301))
PY
```

Current runtime constraints:

- The UDP receive buffer is fixed at `max_datagram_size` (default `65_542` bytes); malformed,
  truncated, oversized, or non-telemetry datagrams are rejected without panicking the loop.
- The internal Tokio `broadcast` channel is bounded and lossy (default capacity `1024` frames), so
  slow consumers observe lag instead of blocking ingestion.
- If the default propagator cannot be constructed, the binary still ingests and parses frames but
  skips physics validation for that run.
- `main.rs` currently passes `RfMetadata::default()`, so the Doppler check is skipped until SDR
  carrier metadata is wired in; the elevation gate still runs when tracking state is available.

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Runtime pipeline (Milestones 1-4)

1. **Ingest (`ingest`).** `IngestConfig` binds a UDP socket and `ingest::run` forwards each
   datagram as a cheap-clone `RawFrame` (`Arc<[u8]>`, receive timestamp, source address) plus
   cumulative `IngestStats`.
2. **Parse (`ccsds`).** `parse_telemetry` validates the CCSDS Space Packet primary header in this
   order: minimum header length, header decode, declared length vs available bytes, then TM/TC
   routing. Accepted packets become `TelemetryFrame`; `payload()` borrows the packet data field
   zero-copy from the retained datagram.
3. **Track (`propagator`).** `StationConfig` resolves the public/default ISS TLE or a local TLE
   file, then `TrackingProvider` serves throttled `TrackingState` values from the
   `OrbitalPropagator` trait. The Ephemerust SGP4 backend is the default implementation.
4. **Co-validate (`validate`).** `apply_physics_validation` clears `TelemetryFrame::physics_flags`
   and sets stable bit flags: bit 0 Doppler anomaly, bit 1 below configured elevation threshold,
   bit 2 reserved for future RSSI/link-budget checks.

Default operational values:

| Setting | Default | Source |
|---------|---------|--------|
| UDP bind address | `127.0.0.1:7301` | `IngestConfig::default()` |
| Channel capacity | `1024` frames | `IngestConfig::default()` |
| Max datagram size | `65_542` bytes | `IngestConfig::default()` |
| Station location | `35.0°`, `-116.0°`, `1000 m` | `StationConfig::default()` |
| Nominal carrier | `437_500_000 Hz` | `StationConfig::default()` |
| TLE source | Inline public ISS (ZARYA) TLE | `DEFAULT_ISS_TLE` |
| Tracking throttle | `10 ms` | `StationConfig::default()` |
| Doppler tolerance | `150 Hz` | `StationConfig::default()`, `TEST_PLAN.md` T-DOPPLER |
| Minimum elevation | `0°` strict threshold | `StationConfig::default()`, `TEST_PLAN.md` T-ELEVATION |

Public library surfaces are available from the `chronus_gateway::` modules; selected items are
also re-exported at the crate root:

- Ingestion: `ingest::bind`, `ingest::run`, `IngestConfig`, `RawFrame`, `IngestStats`.
- Parsing: `ccsds::parse_telemetry`, `TelemetryFrame`, `CcsdsError`.
- Tracking: `StationConfig`, `TleSource`, `ConfigError`, `EphemerustPropagator`,
  `OrbitalPropagator`, `TrackingProvider`, `TrackingState`.
- Validation: `validate::apply_physics_validation`, `RfMetadata`,
  `validate::expected_carrier_hz`, and the `FLAG_*` bit masks.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

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

## References

- [`BUILD_PLAN.md`](BUILD_PLAN.md) — milestone scope, stage gates, and pending M5-M7 work.
- [`TEST_PLAN.md`](TEST_PLAN.md) — layered test gates, current test counts, and tolerance register.
- [`Methodology.md`](Methodology.md) — decision log and attribution table for dependencies and
  design choices.
- [`crates/gateway/src`](crates/gateway/src) — source of truth for the runtime pipeline documented
  above.
- [CCSDS public standards](https://public.ccsds.org/) — open TMTC packet standards used by the
  parser boundary.
- [NASA Open MCT](https://nasa.github.io/openmct/) — planned web mission-control target for M5.

---

## License and compliance

Licensed under the MIT License.

This project is designed strictly around open international standards (CCSDS) and is published
openly to comply with the Public Domain and Fundamental Research exclusions of ITAR/EAR. See
[`AGENTS.md`](AGENTS.md) for the project's compliance, attribution, and security policies.
