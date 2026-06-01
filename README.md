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
│   │   ├── config.rs       Ingestion and station/tracking configuration
│   │   ├── ingest.rs       Asynchronous UDP ingestion loop (RawFrame, stats, shutdown)
│   │   ├── ccsds.rs        CCSDS Space Packet parsing (TelemetryFrame, validation)
│   │   ├── validate.rs     Physics–Telemetry Co-Validation (Doppler, elevation, physics_flags)
│   │   ├── propagator.rs   OrbitalPropagator trait + Ephemerust-backed implementation
│   │   └── main.rs         Entrypoint (ingest → parse → track → validate logging pipeline)
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
cargo build       # compile the workspace
RUST_LOG=info cargo run  # run the local UDP telemetry pipeline
cargo test        # unit + integration + doctests
```

`cargo run` currently starts a development pipeline:

1. bind the UDP downlink socket;
2. broadcast raw datagrams to consumers without letting slow consumers block ingestion;
3. parse valid CCSDS telemetry Space Packets;
4. compute the current tracking state from the default station/TLE; and
5. apply Physics-Telemetry Co-Validation flags before logging the parsed frame.

Open MCT/WebSocket distribution is still Milestone 5, so the binary logs validated telemetry rather
than serving it to clients.

### Current defaults

These defaults come from `IngestConfig::default()` and `StationConfig::default()`:

| Setting | Default | Notes |
|---------|---------|-------|
| UDP bind address | `127.0.0.1:7301` | Loopback-only for local development. Bind a real interface in deployment code/config later. |
| Broadcast capacity | `1024` frames | Lossy by design; lagging subscribers see dropped-frame warnings. |
| Max datagram size | `65_542` bytes | CCSDS primary header plus the maximum packet data field; bounds receive-buffer memory. |
| Station | `35.0°N, 116.0°W, 1000 m` | Synthetic development observer, not mission-specific operational data. |
| TLE | Public ISS (ZARYA) reference TLE | Inline, public reference data only. File-backed TLEs are supported by `TleSource::File`. |
| Nominal carrier | `437_500_000 Hz` | Used by the Doppler calculation when RF metadata is present. |
| Tracking throttle | `10 ms` | Reuses tracking state for frames inside the throttle window. |
| Doppler tolerance | `±150 Hz` | See `TEST_PLAN.md` T-DOPPLER and `Methodology.md` D-012. |
| Minimum elevation | `0°` | Frames predicted strictly below this threshold set bit 1. |

### Local UDP smoke workflow

Terminal A:

```bash
RUST_LOG=info cargo run
```

Terminal B can send a synthetic, valid CCSDS telemetry packet to the loopback socket:

```bash
python3 - <<'PY'
import socket

# TM, APID 0x02A, unsegmented sequence 7, 5-byte payload "hello".
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

The gateway should log `telemetry frame parsed` with APID `42`, sequence `7`, payload length `5`,
tracking values, and `physics_flags`. Today the binary passes `RfMetadata::default()`, so Doppler
validation is skipped unless a caller supplies measured carrier metadata; the elevation gate still
runs when tracking is available.

### `physics_flags` contract

`TelemetryFrame::physics_flags` is the stable bitfield consumed by downstream distribution code:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Doppler anomaly: measured carrier differs from expected by more than the configured tolerance. |
| 1 | `0x02` | Horizon/elevation anomaly: predicted elevation is below `minimum_elevation_deg`. |
| 2 | `0x04` | Reserved for future RSSI/link-budget validation; not set today. |

When `RfMetadata::measured_carrier_hz` is `None` or non-finite, Doppler validation does not set bit
0. Non-finite tracking values skip the checks that depend on them; invalid CCSDS datagrams are
warned and dropped without stopping ingestion.

### Troubleshooting

- **`../Ephemerust` build errors:** clone/check out Ephemerust as a sibling of this repository, or
  update `Cargo.toml` deliberately if the dependency source changes.
- **Bad or missing TLE:** `EphemerustPropagator::from_station` errors are logged as warnings; the
  binary continues ingesting and parsing without physics state.
- **`consumer lagged; dropped frames`:** a broadcast subscriber fell behind the bounded channel.
  This is expected backpressure behavior; newest telemetry is preferred over blocking the socket.
- **CCSDS parse warnings:** malformed, truncated, telecommand, or otherwise non-telemetry datagrams
  are dropped and the loop continues.
- **Windows `LNK1104` / access denied while linking:** keep `.cargo/config.toml`'s `rust-lld`
  workaround in place; see `Methodology.md` D-008.

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

- [`AGENTS.md`](AGENTS.md) — project constitution covering compliance, attribution, security, and
  the required testing standard.
- [`Methodology.md`](Methodology.md) — decision log for architecture, dependencies, tolerances, and
  attribution.
- [`BUILD_PLAN.md`](BUILD_PLAN.md) — milestone roadmap and current implementation status.
- [`TEST_PLAN.md`](TEST_PLAN.md) — test matrix, stage gates, and tolerance register.
- `crates/gateway/src/{ingest,ccsds,config,propagator,validate}.rs` — source-of-truth module docs
  for current behavior.
- CCSDS Space Packet standards (open international standards) and the `spacepackets` crate are the
  parsing references for Milestone 2.
- Ephemerust and the `sgp4` crate are the current astrodynamics references for tracking and
  range-rate-derived Doppler validation.

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
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the Rust CCSDS Space Packet parser
  wrapped by the gateway's `ccsds` module.
- **[NASA Open MCT](https://nasa.github.io/openmct/)** — the open-source mission-control
  framework targeted by the distribution layer.
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
