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

## Public interfaces and pipeline contracts

The implemented M1-M4 pipeline is intentionally small and explicit:

1. **Ingest** (`ingest::bind`, `ingest::run`) receives UDP datagrams as `RawFrame` values and
   broadcasts them over a bounded, lossy Tokio channel.
2. **Parse** (`ccsds::parse_telemetry`) validates the CCSDS Space Packet primary header and returns
   a `TelemetryFrame` with a zero-copy `payload()` borrow into the retained datagram.
3. **Track** (`EphemerustPropagator`, `TrackingProvider`) computes station-relative azimuth,
   elevation, slant range, and range rate from the configured TLE and ground station.
4. **Validate** (`validate::apply_physics_validation`) resets and sets `TelemetryFrame::physics_flags`
   from Doppler and elevation checks.

Stable anomaly bits:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Measured carrier differs from Doppler-derived expected carrier beyond tolerance. |
| 1 | `0x02` | Predicted elevation is below `minimum_elevation_deg`. |
| 2 | `0x04` | Reserved for future RSSI / link-budget validation; not set today. |

Key defaults live in `StationConfig`: a public ISS TLE, station `(35.0, -116.0, 1000 m)`, nominal
carrier `437_500_000 Hz`, Doppler tolerance `150 Hz`, minimum elevation `0 deg`, and a 10 ms
tracking recompute throttle. `IngestConfig` defaults to UDP loopback `127.0.0.1:7301`, channel
capacity `1024`, and `65_542` bytes maximum datagram size.

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
cargo run        # run the gateway on the default UDP socket
cargo test       # unit + integration + doctests
```

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Developer and operations runbook

### Local setup checklist

1. Check out `chronus-gateway` and `Ephemerust` as siblings, or update `Cargo.toml` if you are
   deliberately testing a different Ephemerust source.
2. Use Rust `1.88` or newer.
3. Run `cargo test` before advancing any milestone or documentation claiming implementation status.

### Smoke-test the ingestion path

Run the gateway:

```bash
cargo run -p chronus-gateway
```

In another terminal, send a synthetic telemetry packet to the default UDP socket:

```bash
python - <<'PY'
import socket

apid = 0x2A
seq = 1
payload = b"hello"
word1 = apid                       # version=0, type=TM, no secondary header
word2 = (0b11 << 14) | seq          # unsegmented sequence
data_len = len(payload) - 1         # CCSDS stores (data field octets - 1)
packet = word1.to_bytes(2, "big") + word2.to_bytes(2, "big") + data_len.to_bytes(2, "big") + payload

sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

Expected result: the gateway logs a parsed telemetry frame (`apid=42`, `seq=1`, payload length 5).
When orbital tracking is available, the same log includes look-angle fields and `physics_flags`.

### Common pitfalls

- The default UDP bind is loopback. Use a different `IngestConfig::bind_addr` for an SDR/front-end
  on another host or NIC.
- The broadcast channel is lossy by design. Slow consumers observe lag and the socket loop keeps
  receiving newer datagrams.
- Oversized UDP behavior differs by OS: Windows reports `WSAEMSGSIZE`; Unix may truncate to the
  fixed receive buffer. CCSDS length validation rejects truncated packets downstream.
- `StationConfig::tle = TleSource::File(...)` is read at propagator construction time. A missing or
  unreadable file disables physics state in `main` but does not stop ingestion/parsing.
- `main` currently passes `RfMetadata::default()`, so Doppler bit 0 is skipped until measured RF
  metadata is wired from an SDR/front-end. The elevation gate still runs when tracking succeeds.
- Keep examples synthetic/public. Do not commit mission keys, real operational frequencies, or
  controlled system parameters.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP and in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- Consultative Committee for Space Data Systems (CCSDS), **Space Packet Protocol**, CCSDS
  133.0-B-2. The `ccsds` module parses and validates the primary header defined by this open
  standard.
- [`spacepackets`](https://crates.io/crates/spacepackets) crate documentation. Used for CCSDS
  Space Packet primary-header decoding (see `Methodology.md` D-010).
- [`Ephemerust`](https://github.com/IsomorphicAlgo/ephemerust). Provides the SGP4-backed
  look-angle and range-rate API used by `propagator.rs`.
- [`sgp4`](https://crates.io/crates/sgp4) crate documentation. Provides the SGP4/SDP4 numerical
  propagation underneath Ephemerust.
- [Tokio](https://tokio.rs/) documentation. Runtime, UDP socket, broadcast channel, and async task
  primitives used by ingestion.
- [Axum](https://github.com/tokio-rs/axum) and [NASA Open MCT](https://nasa.github.io/openmct/).
  Planned references for Milestone 5 WebSocket/HTTP distribution.
- Project docs: [`BUILD_PLAN.md`](BUILD_PLAN.md), [`TEST_PLAN.md`](TEST_PLAN.md), and
  [`Methodology.md`](Methodology.md) are the local source of truth for milestone status, test
  gates, tolerances, and design decisions.

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
