# ChronusGateway-RS

ChronusGateway-RS (Chronus-GS) is an asynchronous, physics-validated Telemetry and Command
(TMTC) ground-station gateway written in Rust. It ingests raw spacecraft downlink frames,
parses them against open CCSDS standards, and cross-checks each frame against the spacecraft's
computed orbital physics. The project is being built toward web-based mission control
distribution such as NASA Open MCT, all in a single, memory-safe, garbage-collection-free
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
   (UDP)          (Tokio)                  (validated frames)         Co-Validation engine
                                                                            │
                  OrbitalPropagator trait ◀── range-rate / look-angles ─────┤
                  (Ephemerust today, nyx-space later)                       ▼
                                                       planned M5: WebSocket/Open MCT
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion and a bounded,
  intentionally lossy broadcast channel so slow consumers cannot stall the receive loop.
  WebSocket fan-out is planned for Milestone 5.
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
│   │   └── main.rs         Entrypoint (runs ingest -> parse -> track -> validate logging)
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
cargo run        # run the local UDP ingest -> parse -> validate pipeline
cargo test       # unit + integration + doctests
```

`cargo run` starts a development listener on `127.0.0.1:7301` using the code defaults in
`IngestConfig` and `StationConfig`. Configuration is code-only today: there is no config file or
environment loader yet. Logs use `tracing_subscriber`, so `RUST_LOG=debug cargo run` enables more
detail. Stop the process with Ctrl-C; the ingestion loop records final frame/error counters.

### Local smoke workflow

In one terminal, start the gateway:

```bash
cargo run
```

In another terminal, send a synthetic CCSDS telemetry packet over loopback:

```bash
python3 - <<'PY'
import socket

# TM packet, APID 0x02A, unsegmented seq 7, five-byte data field b"hello".
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

The gateway should log `telemetry frame parsed` with `apid=42`, `seq=7`, `payload=5`, tracking
fields from the default public ISS TLE, and a `physics_flags` value. Because the current binary
uses `RfMetadata::default()`, Doppler validation is skipped until SDR/front-end carrier metadata is
wired in; the elevation gate still runs when tracking state is available.

### Current configuration defaults

| Surface | Default | Notes |
|---------|---------|-------|
| UDP bind | `127.0.0.1:7301` | Loopback-only for development; production can bind a selected NIC later. |
| Broadcast capacity | `1024` frames | Lossy by design; oldest frames drop for lagging consumers. |
| Max datagram size | `65_542` bytes | Fixed receive buffer; oversized/truncated packets fail safely. |
| Station | lat `35.0`, lon `-116.0`, alt `1000 m` | Synthetic/default development geometry only. |
| TLE | Public ISS (ZARYA) reference TLE | Keep examples public and generic per `AGENTS.md`. |
| Nominal carrier | `437_500_000 Hz` | Used by Doppler math when measured carrier metadata exists. |
| Doppler tolerance | `150 Hz` | See `TEST_PLAN.md` tolerance `T-DOPPLER`. |
| Minimum elevation | `0 deg` | Frames below the mathematical horizon set bit 1. |

### `physics_flags` contract

`TelemetryFrame::physics_flags` is a stable bitfield for downstream alarm coloring:

| Mask | Meaning | Current binary behavior |
|------|---------|-------------------------|
| `0x01` | Doppler anomaly: measured carrier differs from expected by more than tolerance. | Skipped unless `RfMetadata::measured_carrier_hz` is `Some`. |
| `0x02` | Horizon/elevation anomaly: predicted elevation is below `minimum_elevation_deg`. | Active when tracking state is available. |
| `0x04` | Reserved for RSSI/link-budget validation. | Reserved; not set in M4. |

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP, planned in-process WebSocket tests, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- **Local source of truth:** [`ingest.rs`](crates/gateway/src/ingest.rs),
  [`ccsds.rs`](crates/gateway/src/ccsds.rs), [`config.rs`](crates/gateway/src/config.rs),
  [`propagator.rs`](crates/gateway/src/propagator.rs), and
  [`validate.rs`](crates/gateway/src/validate.rs) define the current M1–M4 behavior.
- **[CCSDS public standards catalog](https://public.ccsds.org/Publications/BlueBooks.aspx):**
  CCSDS 133.0-B-2 Space Packet Protocol is the packet-framing basis for `ccsds.rs`.
- **[Ephemerust](https://github.com/IsomorphicAlgo/ephemerust):** SGP4 look-angle and range-rate
  backend used by `EphemerustPropagator`.
- **[`spacepackets` documentation](https://docs.rs/spacepackets/latest/spacepackets/):**
  CCSDS/ECSS packet parsing API wrapped by this gateway.
- **[Tokio](https://tokio.rs/) and [Tracing](https://tracing.rs/):** async runtime, UDP socket,
  broadcast channel, and logging infrastructure.
- **[NASA Open MCT documentation](https://nasa.github.io/openmct/):** target dashboard/distribution
  interface for Milestone 5.

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
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the CCSDS/ECSS packet parsing
  crate used behind the `ccsds` module boundary.
- **[Tokio](https://tokio.rs/)** and **[Tracing](https://tracing.rs/)** — the asynchronous runtime,
  broadcast channel, UDP socket, and observability foundation used by the current network core.
- **[Axum](https://github.com/tokio-rs/axum)** — the planned web framework for Milestone 5
  WebSocket/Open MCT distribution, following the Rusty_Server-inspired direction.
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
