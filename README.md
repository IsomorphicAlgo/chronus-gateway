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
abstraction boundary between the pipeline and any astrodynamics backend. The implemented runtime
path today is UDP ingest → CCSDS parse → station tracking → physics validation → structured logs.
The Open MCT WebSocket adapter is the next stage, not yet part of the binary.

```
Raw RF / SDR ──▶ Async UDP ingestion ──▶ CCSDS zero-copy parser ──▶ Physics-Telemetry
 (UDP today)       (Tokio + broadcast)      (validated frames)       Co-Validation engine
                                                                          │
                OrbitalPropagator trait ◀── range-rate / look-angles ─────┤
                (Ephemerust today, nyx-space later)                       ▼
                                                       structured logs today
                                                       Open MCT WebSocket in M5
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion and a bounded, lossy
  broadcast channel. Slow consumers observe lag instead of blocking the socket loop.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
  `OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
  library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
  clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.
- **Recoverable validation failures.** Malformed, truncated, oversized, and non-telemetry packets
  are surfaced as structured errors or counters; the ingestion path continues running.

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
│   │   └── main.rs         Entrypoint (runs ingest → parse → validate logger)
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
cargo build        # compile the workspace
cargo run          # bind UDP 127.0.0.1:7301 and run ingest → parse → validate
cargo test         # unit + integration + doctests
RUST_LOG=debug cargo run
```

The binary currently uses `IngestConfig::default()` and `StationConfig::default()`:

- UDP bind address: `127.0.0.1:7301`
- Broadcast channel capacity: `1024` frames
- Maximum datagram size: `65_542` bytes
- Default station: synthetic development geometry at `35.0°N, 116.0°W, 1000 m`
- Default tracked object: public ISS (ZARYA) reference TLE embedded in `config.rs`
- Nominal carrier: `437_500_000 Hz`
- Physics gates: Doppler tolerance `±150 Hz`; minimum elevation `0°`

To smoke-test the running binary from another terminal, send one synthetic CCSDS telemetry packet:

```bash
python3 - <<'PY'
import socket

# TM packet, APID 0x02A, sequence 7, 5-byte packet data field "hello".
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

Expected result: the gateway logs `telemetry frame parsed` with APID `42`, sequence `7`, payload
length `5`, tracking values, and `physics_flags`. With the default ISS/station/time geometry, the
elevation bit may be set when the spacecraft is below the configured horizon. Press `Ctrl-C` to
shut down and print final ingest counters.

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Public interfaces and constraints

- `ingest::bind` / `ingest::run` bind a UDP socket and forward `RawFrame` values through a lossy
  Tokio broadcast channel. The socket loop is intentionally not backpressured by downstream work.
- `ccsds::parse_telemetry` accepts CCSDS Space Packets whose primary header declares telemetry
  (TM). Telecommand packets, short headers, and truncated payloads are rejected without panic.
- `StationConfig` supports inline and file-based TLE sources in the library. The current binary
  still constructs the default config directly; no CLI, environment, or TOML config loader exists
  yet. Local-only config files should use ignored names such as `*.local.toml`.
- `TrackingProvider` caches propagator output inside `min_recompute_interval_ms` so bursts of
  frames do not trigger redundant SGP4 calls.
- `validate::apply_physics_validation` writes a stable `physics_flags` bitfield:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Doppler anomaly: measured carrier differs from expected by more than tolerance. |
| 1 | `0x02` | Horizon/elevation anomaly: predicted elevation is below the configured minimum. |
| 2 | `0x04` | Reserved for RSSI/link-budget validation; not set yet. |

`RfMetadata::measured_carrier_hz` is not wired into the binary yet, so runtime Doppler validation is
skipped unless a caller supplies RF metadata through the library API. Elevation validation runs
whenever tracking state is available.

---

## Troubleshooting and common pitfalls

- **`ephemerust` path errors:** clone Ephemerust as a sibling checkout named `Ephemerust`; the
  workspace dependency is `../Ephemerust`.
- **Rust version errors:** use Rust 1.88 or newer, matching the workspace MSRV.
- **No packets received:** the default bind is loopback-only (`127.0.0.1:7301`). Production or SDR
  host testing will need a different `IngestConfig` once external configuration is added.
- **Packet rejected as truncated:** the CCSDS primary-header data-length field is encoded as
  `packet_data_field_len - 1`; the parser checks that declaration against the datagram size.
- **No Open MCT endpoint:** WebSocket distribution, the telemetry dictionary, and HTTP health route
  are Milestone 5 work. Axum is documented as the planned adapter, not an active dependency today.
- **No Doppler bit in binary logs:** `main.rs` passes `RfMetadata::default()`, so bit 0 is skipped
  until SDR/front-end carrier measurements are integrated.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- [`AGENTS.md`](AGENTS.md) — project constitution: ITAR/EAR posture, security rules, attribution,
  and testing standard.
- [`BUILD_PLAN.md`](BUILD_PLAN.md) and [`TEST_PLAN.md`](TEST_PLAN.md) — stage gates, current
  milestone status, test matrix, and physics tolerance register.
- [`Methodology.md`](Methodology.md) — decision log for Rust/Tokio, Ephemerust, CCSDS parsing,
  ingestion backpressure, station tracking, and validation thresholds.
- [CCSDS](https://public.ccsds.org/) Space Packet standards — open packet-framing basis for
  telemetry parsing.
- [`spacepackets`](https://crates.io/crates/spacepackets) — CCSDS Space Packet primary-header
  parser used behind the local `ccsds` module.
- [Ephemerust](https://github.com/IsomorphicAlgo/ephemerust) and the [`sgp4`](https://crates.io/crates/sgp4)
  crate — current astrodynamics backend and underlying SGP4/SDP4 numerics.
- [Tokio](https://tokio.rs/) and [Tracing](https://crates.io/crates/tracing) — async runtime and
  structured diagnostics used by the gateway.
- [NASA Open MCT](https://nasa.github.io/openmct/) — target operator dashboard for the planned
  Milestone 5 distribution layer.

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
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — the CCSDS Space Packet parsing
  crate used by the gateway's parser boundary.
- **[Tokio](https://tokio.rs/)** and **[Tracing](https://crates.io/crates/tracing)** — the
  asynchronous runtime and structured diagnostics that form the current network core.
- **[Axum](https://github.com/tokio-rs/axum)** — the planned WebSocket/HTTP framework for the
  Open MCT adapter, following the owner's prior async service patterns.
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
