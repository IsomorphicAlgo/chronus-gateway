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

The gateway is built around two principles: an asynchronous, lock-free network core and a clean
abstraction boundary between the pipeline and any astrodynamics backend.

```
Raw RF / SDR ──▶ Async UDP ingestion ──▶ CCSDS zero-copy parser ──▶ Physics-Telemetry
 (UDP today)      (Tokio)                  (validated frames)         Co-Validation engine
                                                                            │
                  OrbitalPropagator trait ◀── range-rate / look-angles ─────┤
                  (Ephemerust today, nyx-space later)                       │
                                                                            ▼
                                                M5 planned: Axum WebSocket ──▶ NASA Open MCT
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion and downstream
  consumers through a bounded, lossy broadcast channel. Slow consumers observe lag
  instead of blocking the receive loop.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
  `OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
  library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
  clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.
- **Current binary pipeline.** `cargo run` binds UDP, parses each valid telemetry datagram as a
  CCSDS Space Packet, computes tracking state when the default station/TLE validates, applies the
  `physics_flags` checks, and emits structured logs. WebSocket/Open MCT distribution is not
  implemented yet.

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
│   │   └── main.rs         Entrypoint (ingest → parse → track → validate logs)
│   └── tests/
│       └── ingest.rs       Loopback UDP integration tests for ingestion
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
cargo build                 # compile the workspace
RUST_LOG=info cargo run     # run the live UDP gateway until Ctrl-C
cargo test                  # unit + integration + doctests
```

`cargo run` currently uses in-code defaults rather than CLI flags, environment configuration, or
a config file. Change defaults through `IngestConfig` and `StationConfig` while those surfaces are
still stabilizing.

### Current runtime defaults

| Setting | Default | Source |
|---------|---------|--------|
| UDP bind address | `127.0.0.1:7301` | `IngestConfig::default()` |
| Broadcast capacity | `1024` frames, lossy on lag | `IngestConfig::default()` / `ingest::run` |
| Max datagram size | `65_542` bytes | `IngestConfig::default()` |
| Station location | `35.0°N`, `116.0°W`, `1000 m` altitude | `StationConfig::default()` |
| Tracked object | Public ISS (ZARYA) reference TLE | `DEFAULT_ISS_TLE` |
| Nominal carrier | `437_500_000 Hz` | `StationConfig::default()` |
| Look-angle throttle | `10 ms` recompute interval | `StationConfig::default()` |
| Validation thresholds | Doppler `±150 Hz`; minimum elevation `0°` | `StationConfig::default()` |

### Local loopback smoke test

Use only synthetic/public data when exercising the gateway. The bytes below are the CCSDS unit-test
golden packet: TM, APID `0x02A`, sequence `7`, payload `hello`.

```bash
# Terminal 1
RUST_LOG=info cargo run

# Terminal 2
python3 - <<'PY'
import socket

packet = bytes([
    0x00, 0x2A,  # version/type/sec-hdr/APID: TM, APID 0x02A
    0xC0, 0x07,  # unsegmented sequence, count 7
    0x00, 0x04,  # payload length minus one
    *b"hello",
])
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

Expected logs include `ChronusGateway-RS listening for telemetry`, `orbital tracking provider
ready`, and then `telemetry frame parsed` with APID, sequence count, payload length, tracking
state, and `physics_flags`. Because `main.rs` currently passes `RfMetadata::default()`, Doppler is
skipped in the binary until RF metadata is wired; the elevation check still runs when tracking
state is available. Invalid, truncated, or telecommand packets are logged and dropped without
stopping ingestion.

### Library workflow

Library users can wire the same stages independently:

1. Build an `IngestConfig`, call `ingest::bind`, then run `ingest::run` with a
   `tokio::sync::broadcast::Sender<RawFrame>`.
2. Subscribe to raw frames and call `ccsds::parse_telemetry(&raw_frame)`.
3. Build an `EphemerustPropagator` from `StationConfig`, wrap it in `TrackingProvider`, and request
   `tracking_state(frame.received_at)`.
4. Call `apply_physics_validation` with optional `RfMetadata`.

The stable `physics_flags` contract is:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Doppler anomaly; measured carrier exceeds tolerance from expected carrier. |
| 1 | `0x02` | Horizon/elevation anomaly; predicted elevation is below the configured minimum. |
| 2 | `0x04` | Reserved for future RSSI/link-budget validation. |

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Testing

Testing is a first-class deliverable. The current suite includes inline unit tests, loopback UDP
integration tests for ingestion, doctests, and physics co-validation tests with explicitly
documented tolerances. In-process WebSocket/Open MCT tests are planned with Milestone 5. The full
strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- [`BUILD_PLAN.md`](BUILD_PLAN.md) — stage-gated implementation roadmap and current milestone
  status.
- [`TEST_PLAN.md`](TEST_PLAN.md) — layered test matrix, tolerance register, and current counts.
- [`Methodology.md`](Methodology.md) — decision log, trade-offs, and attribution register.
- [`AGENTS.md`](AGENTS.md) — compliance, security, attribution, and testing constitution.
- [CCSDS public standards](https://public.ccsds.org/) — open packet and TMTC standards that define
  the protocol family used here.
- [`spacepackets`](https://crates.io/crates/spacepackets) — CCSDS Space Packet parsing used by the
  `ccsds` module.
- [NASA Open MCT](https://nasa.github.io/openmct/) — planned web mission-control target.

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
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the CCSDS Space Packet parser used
  for primary-header decoding and packet validation.
- **[Tokio](https://tokio.rs/)** — the asynchronous runtime that drives UDP ingestion and internal
  broadcast fan-out. **[Axum](https://github.com/tokio-rs/axum)** is planned for the Milestone 5
  WebSocket/HTTP distribution layer.
- **[CCSDS](https://public.ccsds.org/)** — the open international standards for space packet
  framing and protocols that define the gateway's wire formats.
- **[NASA Open MCT](https://nasa.github.io/openmct/)** — the open-source mission-control
  framework targeted by the planned distribution layer.
- **[NeXosim](https://github.com/asynchronics/nexosim)** — the discrete-event simulation
  framework planned for hardware-in-the-loop validation.

The broader Rust aerospace ecosystem — including `sat-rs` and `nyx-space` — informed the design
analysis.

---

## License and compliance

Licensed under the MIT License.

This project is designed strictly around open international standards (CCSDS) and is published
openly to comply with the Public Domain and Fundamental Research exclusions of ITAR/EAR. See
[`AGENTS.md`](AGENTS.md) for the project's compliance, attribution, and security policies.
