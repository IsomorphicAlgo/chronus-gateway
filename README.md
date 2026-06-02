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
│   │   └── main.rs         Entrypoint (runs the ingestion server)
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
cargo run        # run the UDP ingestion gateway on the default loopback address
cargo test       # unit + integration + doctests
```

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Current gateway runbook

The implemented runtime pipeline is:

1. Bind UDP on `IngestConfig::default().bind_addr` (`127.0.0.1:7301`).
2. Broadcast each bounded datagram as `RawFrame`.
3. Parse telemetry packets with `ccsds::parse_telemetry`.
4. Compute a throttled `TrackingState` from the default `StationConfig`.
5. Apply physics co-validation and log the decoded frame plus `physics_flags`.

Run the gateway:

```bash
RUST_LOG=info cargo run -p chronus-gateway
```

Send one synthetic CCSDS telemetry packet from another shell:

```bash
python3 - <<'PY'
import socket

# TM packet, APID 0x02A, sequence 7, unsegmented, 5-byte payload "hello".
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

Expected result: the gateway logs a parsed frame with `apid=42`, `seq=7`, `payload=5`, tracking
state fields, and a `physics_flags` value. The default binary currently uses code defaults rather
than CLI or environment configuration; TLE file resolution exists on `StationConfig` but is not yet
wired into the executable. WebSocket/Open MCT distribution starts in Milestone 5.

### Operational constraints and pitfalls

- Only synthetic/public data belongs in examples. Do not commit real mission keys, controlled
  frequencies, or operational parameters.
- UDP datagrams larger than `max_datagram_size` are bounded by the fixed receive buffer. Windows
  reports oversized datagrams as dropped; Unix may truncate them, after which CCSDS length checks
  reject partial packets.
- The broadcast channel is intentionally lossy. Slow consumers receive `Lagged`; the receive loop
  keeps the freshest telemetry flowing.
- CCSDS parsing accepts TM packets only. TC packets, short headers, malformed headers, and
  truncated packet data are structured errors and are dropped by `main`.
- Doppler validation requires `RfMetadata::measured_carrier_hz`. The current binary passes no SDR
  RF metadata, so bit 0 is skipped at runtime; unit tests exercise measured-carrier behavior.
- The default ISS TLE and station coordinates are public development fixtures. Replace them through
  `StationConfig` in library integrations before interpreting physics flags operationally.

---

## Public interfaces and contracts

| Interface | Purpose | Current contract |
|-----------|---------|------------------|
| `IngestConfig` | UDP ingress limits | Defaults to `127.0.0.1:7301`, channel capacity `1024`, max datagram size `65_542` bytes. |
| `ingest::run` | Receive loop | Forwards `RawFrame { bytes, received_at, source }` on a lossy `broadcast` channel until the shutdown future resolves. |
| `ccsds::parse_telemetry` | Space Packet parser | Parses the CCSDS primary header with `spacepackets`, rejects TC/non-TM packets, and exposes payload bytes zero-copy through `TelemetryFrame::payload()`. |
| `StationConfig` | Ground-station and tracked-object settings | Validates lat/lon/altitude, nominal carrier, TLE source, Doppler tolerance, elevation threshold, and look-angle throttle. |
| `OrbitalPropagator` | Astrodynamics seam | Returns station-relative azimuth, elevation, slant range, and line-of-sight range rate for a UTC instant. |
| `validate::apply_physics_validation` | Co-validation engine | Clears then sets `TelemetryFrame::physics_flags` from Doppler and elevation checks. |

`physics_flags` is a stable bitfield for downstream distribution:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Doppler anomaly: measured carrier differs from expected by more than `doppler_tolerance_hz`. |
| 1 | `0x02` | Horizon/elevation anomaly: predicted elevation is strictly below `minimum_elevation_deg`. |
| 2 | `0x04` | Reserved for future RSSI/link-budget validation; not set today. |

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- [`AGENTS.md`](AGENTS.md) — project constitution covering compliance, attribution, security, and
  testing requirements.
- [`BUILD_PLAN.md`](BUILD_PLAN.md) — stage-gated roadmap and implemented milestone status.
- [`TEST_PLAN.md`](TEST_PLAN.md) — test matrix, status counts, and tolerance register.
- [`Methodology.md`](Methodology.md) — decision log and attribution ledger.
- [CCSDS Space Packet Protocol](https://public.ccsds.org/) — open packet standard used for TM/TC
  framing.
- [`spacepackets`](https://crates.io/crates/spacepackets) — Rust CCSDS/ECSS parsing crate used by
  the gateway parser module.
- [Ephemerust](https://github.com/IsomorphicAlgo/ephemerust) — sibling astrodynamics crate used for
  SGP4 look-angles and range-rate.
- [NASA Open MCT](https://nasa.github.io/openmct/) — target web mission-control framework for the
  planned distribution layer.

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
