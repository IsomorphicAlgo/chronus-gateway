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
 (UDP today)       (Tokio)                  (validated frames)         Co-Validation engine
                                                                            │
                  OrbitalPropagator trait ◀── range-rate / look-angles ─────┤
                  (Ephemerust today, nyx-space later)                       ▼
                                                        M5: Axum WebSocket ──▶ NASA Open MCT
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion and a pool of
  broadcast subscribers. WebSocket fan-out to operator screens is the next milestone.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
  `OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
  library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
  clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.

The reasoning behind these and other choices is recorded in [`Methodology.md`](Methodology.md).

### Current implemented pipeline (M1-M4)

`cargo run` wires the implemented stages together:

| Stage | Codepath | Runtime behavior |
|-------|----------|------------------|
| Ingest | `crates/gateway/src/ingest.rs` | Binds UDP `127.0.0.1:7301` by default, receives into a fixed-size buffer, and broadcasts `RawFrame` values on a bounded lossy channel. |
| Parse | `crates/gateway/src/ccsds.rs` | Parses CCSDS Space Packet primary headers with `spacepackets`, accepts telemetry (TM), rejects telecommand (TC), and exposes the packet data field zero-copy. |
| Track | `crates/gateway/src/propagator.rs` | Builds an `EphemerustPropagator` from `StationConfig`, then a throttled `TrackingProvider` supplies azimuth, elevation, range, and range-rate. If the propagator cannot be built, ingestion and parsing continue without physics state. |
| Validate | `crates/gateway/src/validate.rs` | Resets and sets `physics_flags`: bit 0 Doppler anomaly, bit 1 below configured elevation, bit 2 reserved for future RSSI/link-budget validation. The current binary passes `RfMetadata::default()`, so Doppler is skipped until SDR metadata is wired. |

Malformed, truncated, oversized, or non-telemetry datagrams are logged and dropped; the receive
loop continues.

---

## Repository layout

```
chronus-gateway/
├── Cargo.toml              Workspace manifest (centralized dependency versions, MSRV 1.88)
├── crates/gateway/         The gateway binary + library
│   ├── src/
│   │   ├── lib.rs          Crate documentation and module wiring
│   │   ├── config.rs       Ingest + station/TLE configuration and M4 thresholds
│   │   ├── ingest.rs       Asynchronous UDP ingestion loop (RawFrame, stats, shutdown)
│   │   ├── ccsds.rs        CCSDS Space Packet parsing (TelemetryFrame, validation)
│   │   ├── validate.rs     Physics–Telemetry Co-Validation (Doppler, elevation, physics_flags)
│   │   ├── propagator.rs   OrbitalPropagator trait + Ephemerust-backed implementation
│   │   └── main.rs         Entrypoint (runs the M1-M4 demo pipeline)
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
cargo build                    # compile the workspace
RUST_LOG=info cargo run         # bind UDP and run ingest -> parse -> track -> validate
cargo test                     # unit + integration + doctests
```

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

### Send a synthetic telemetry packet

With the gateway running in one terminal, this sends a minimal CCSDS telemetry packet over loopback
UDP. The packet is synthetic/public test data only.

```bash
python3 - <<'PY'
import socket

# CCSDS TM, APID 0x02A, unsegmented seq 7, 5-byte packet data field "hello".
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"

sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

Expected result: the gateway logs `telemetry frame parsed` with APID, sequence count, payload
length, and (when tracking succeeds) look-angle/range values plus `physics_flags`. Invalid CCSDS
bytes should produce a warning and should not stop the loop.

### Configuration surface

Configuration is code-first today; there is no CLI, environment, or config-file loader yet. The
defaults are intentionally local and synthetic:

| Setting | Default | Notes |
|---------|---------|-------|
| `IngestConfig::bind_addr` | `127.0.0.1:7301` | Loopback development default; production can bind a NIC-specific address. |
| `IngestConfig::channel_capacity` | `1024` frames | Bounded lossy broadcast; slow subscribers see lag instead of blocking ingestion. |
| `IngestConfig::max_datagram_size` | `65_542` bytes | Fixed receive buffer sized for one CCSDS Space Packet plus primary header. |
| `StationConfig` location | `35.0, -116.0, 1000 m` | Generic ground-station demo geometry. |
| `StationConfig::tle` | Public ISS (ZARYA) TLE | Public reference data; inline and file-based TLEs are supported. |
| `nominal_carrier_hz` | `437_500_000 Hz` | Used by the Doppler formula when RF metadata is present. |
| `doppler_tolerance_hz` | `150 Hz` | T-DOPPLER tolerance; see `TEST_PLAN.md` and `Methodology.md` D-012. |
| `minimum_elevation_deg` | `0 deg` | Strict below-horizon gate; use a negative value for a refraction/mask margin. |
| `min_recompute_interval_ms` | `10 ms` | Tracking-provider throttle; `0` disables cache reuse. |

### Troubleshooting and common pitfalls

- `ephemerust` must exist as a sibling checkout (`../Ephemerust`) because the workspace uses a path
  dependency.
- `cargo run` keeps running until Ctrl-C; it is a UDP service, not a one-shot smoke test.
- A TC packet on the telemetry path is expected to log `NotTelemetry` and be dropped.
- The current binary cannot set Doppler anomaly bit 0 because no measured carrier is supplied yet;
  unit tests exercise that path directly.
- The default TLE is public reference data for deterministic development, not live operational
  tracking data.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- [CCSDS public standards](https://public.ccsds.org/) — open space packet and TMTC protocol
  standards that define the gateway's framing scope.
- [`spacepackets` crate](https://crates.io/crates/spacepackets) — CCSDS/ECSS packet parsing used
  behind the `ccsds` module boundary.
- [Ephemerust](https://github.com/IsomorphicAlgo/ephemerust) — sibling SGP4/look-angle library used
  by the default propagator backend.
- [`sgp4` crate](https://crates.io/crates/sgp4) — SGP4/SDP4 propagation dependency used by
  Ephemerust.
- [Tokio](https://tokio.rs/) and [Tracing](https://tracing.rs/) — async runtime and structured
  observability used by the network path.
- [Axum](https://github.com/tokio-rs/axum) and [NASA Open MCT](https://nasa.github.io/openmct/) —
  planned M5 distribution stack and operator UI target.

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
