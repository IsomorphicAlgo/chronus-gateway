# ChronusGateway-RS

ChronusGateway-RS (Chronus-GS) is an asynchronous, physics-validated Telemetry and Command
(TMTC) ground-station gateway written in Rust. It ingests raw spacecraft downlink frames,
parses them against open CCSDS standards, cross-checks each frame against the spacecraft's
computed orbital physics, and is being built to distribute validated telemetry to web-based
mission control dashboards such as NASA Open MCT — in a single, memory-safe,
garbage-collection-free executable.

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
                  (Ephemerust today, nyx-space later)                       ▼
                                                Axum WebSocket (planned M5) ──▶ NASA Open MCT
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion today; the planned M5
  Axum layer will fan validated telemetry out to concurrent WebSocket clients.
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
│   │   ├── config.rs       Ingestion + station configuration
│   │   ├── ingest.rs       Asynchronous UDP ingestion loop (RawFrame, stats, shutdown)
│   │   ├── ccsds.rs        CCSDS Space Packet parsing (TelemetryFrame, validation)
│   │   ├── validate.rs     Physics–Telemetry Co-Validation (Doppler, elevation, physics_flags)
│   │   ├── propagator.rs   OrbitalPropagator trait, Ephemerust backend, TrackingProvider cache
│   │   └── main.rs         Entrypoint (UDP ingest → parse → track → validate demo)
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
cargo run        # run the UDP ingestion server (parse → track → validate demo)
cargo test       # unit + integration + doctests
```

### Current runtime defaults and constraints

`cargo run` starts a local UDP telemetry listener and demonstration consumer:

- **Bind address:** `127.0.0.1:7301` (`IngestConfig::default()`), with a bounded 1,024-frame
  lossy broadcast channel and a fixed 65,542-byte receive buffer.
- **Station defaults:** synthetic development station coordinates, nominal carrier
  `437_500_000 Hz`, public ISS (ZARYA) TLE text, 10 ms tracking recompute throttle,
  ±150 Hz Doppler tolerance, and a 0° minimum elevation threshold.
- **Configuration loading:** no CLI, environment, or config-file loader exists yet; defaults are
  compiled in and public/synthetic by design.
- **Telemetry parsing:** the gateway accepts CCSDS TM packets, records primary-header fields, keeps
  the packet data field zero-copy, and drops malformed/truncated/TC datagrams with structured logs.
- **Runtime validation:** the live binary builds a tracking state and applies elevation validation.
  Doppler validation requires `RfMetadata::measured_carrier_hz`; `main.rs` currently passes
  `RfMetadata::default()`, so bit 0 is skipped until SDR/front-end metadata is wired.
- **Graceful degradation:** if station/TLE propagation setup fails, ingestion and CCSDS parsing
  continue and frames are logged without physics state.

Synthetic loopback smoke example:

```bash
# Terminal 1: start the listener.
cargo run
```

```bash
# Terminal 2: send a minimal synthetic CCSDS TM packet.
python3 - <<'PY'
import socket

payload = b"hello"
apid = 0x02A
seq = 1
word1 = apid                         # version=0, TM packet, no secondary header
word2 = (0b11 << 14) | seq            # unsegmented packet, sequence count 1
data_len = len(payload) - 1           # CCSDS data-length field encoding
packet = (
    word1.to_bytes(2, "big")
    + word2.to_bytes(2, "big")
    + data_len.to_bytes(2, "big")
    + payload
)
socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(packet, ("127.0.0.1", 7301))
PY
```

Expected log outcome: a parsed telemetry frame with APID `42`, sequence `1`, payload length `5`,
and a `physics_flags` value. The default ISS/station geometry may set the below-horizon bit.

### Public pipeline contracts

- `RawFrame` carries the original datagram bytes, capture timestamp, and UDP source address.
- `TelemetryFrame` exposes CCSDS primary-header fields and a zero-copy `payload()` borrow.
- `TrackingProvider` caches `OrbitalPropagator` output within the configured throttle window.
- `physics_flags` is a stable bitfield for downstream clients:

| Bit | Mask | Meaning |
|-----|------|---------|
| 0 | `0x01` | Doppler anomaly: measured carrier differs from expected beyond tolerance. |
| 1 | `0x02` | Horizon/elevation anomaly: predicted elevation is below the configured minimum. |
| 2 | `0x04` | Reserved for future RSSI/link-budget validation. |

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

- [`crates/gateway/src/ingest.rs`](crates/gateway/src/ingest.rs) — UDP ingestion, lossy
  backpressure, shutdown, and ingest counters.
- [`crates/gateway/src/ccsds.rs`](crates/gateway/src/ccsds.rs) — CCSDS primary-header parsing,
  validation order, and `TelemetryFrame` public fields.
- [`crates/gateway/src/config.rs`](crates/gateway/src/config.rs) — current default runtime
  settings, public ISS TLE fixture, and station validation rules.
- [`crates/gateway/src/propagator.rs`](crates/gateway/src/propagator.rs) — `OrbitalPropagator`,
  Ephemerust backend, and throttled `TrackingProvider`.
- [`crates/gateway/src/validate.rs`](crates/gateway/src/validate.rs) — Doppler formula,
  `RfMetadata` skip rules, and `physics_flags` bit assignments.
- [`BUILD_PLAN.md`](BUILD_PLAN.md), [`TEST_PLAN.md`](TEST_PLAN.md), and
  [`Methodology.md`](Methodology.md) — stage gates, test coverage, tolerances, and decision log.
- Open standards and open-source projects: [CCSDS](https://public.ccsds.org/),
  [spacepackets](https://crates.io/crates/spacepackets),
  [Tokio](https://tokio.rs/), [Ephemerust](https://github.com/IsomorphicAlgo/ephemerust),
  [`sgp4`](https://crates.io/crates/sgp4), and
  [NASA Open MCT](https://nasa.github.io/openmct/).

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
- **[`spacepackets`](https://crates.io/crates/spacepackets)** — CCSDS Space Packet parsing used by
  the Milestone 2 framing layer.
- **[Tokio](https://tokio.rs/)** — the asynchronous runtime that drives UDP ingestion today.
- **[Axum](https://github.com/tokio-rs/axum)** — the planned WebSocket/HTTP framework for the
  Milestone 5 Open MCT distribution layer.
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
