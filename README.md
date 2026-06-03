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
SDR/front-end ──▶ Async UDP ingestion ──▶ CCSDS zero-copy parser ──▶ Physics-Telemetry
   (UDP)            (Tokio)                  (TelemetryFrame)         Co-Validation engine
                                                                           │
                 OrbitalPropagator trait ◀── range-rate / look-angles ─────┤
                 (Ephemerust today, nyx-space later)                       ▼
                                                        M5 Axum WebSocket ──▶ NASA Open MCT
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion today. Milestone 5 adds
  Axum WebSocket fan-out to operator screens without changing the ingest/parser/validation seams.
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
│   │   └── main.rs         Entrypoint (runs ingest → parse → tracking → validation logging)
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
cargo build             # compile the workspace
RUST_LOG=info cargo run # listen on UDP 127.0.0.1:7301 and log parsed telemetry
cargo test              # unit + integration + doctests
```

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

### Local ingest workflow

`cargo run` uses in-code defaults; there is no file or environment config loader yet.

- **Bind address:** `127.0.0.1:7301` (`IngestConfig::default`), with a bounded 65,542-byte receive
  buffer and a lossy 1,024-frame broadcast channel.
- **Station defaults:** a synthetic/public ISS TLE, latitude `35.0`, longitude `-116.0`, altitude
  `1000 m`, nominal carrier `437.5 MHz`, Doppler tolerance `150 Hz`, minimum elevation `0°`.
- **Logging:** `tracing_subscriber` reads `RUST_LOG`; if unset, the binary defaults to `info`.
- **Shutdown:** press Ctrl-C. The ingestion loop observes the shutdown future, stops, aborts the
  demo logger task, and prints final frame/error counters.
- **Degraded physics mode:** if the station/TLE cannot build a propagator, ingest and CCSDS parsing
  continue and frames are logged without physics state.
- **RF metadata:** the demo consumer passes `RfMetadata::default()`, so Doppler bit 0 is skipped.
  Elevation bit 1 is still evaluated whenever tracking state is available.

To send one synthetic CCSDS telemetry packet to a running local gateway:

```bash
python - <<'PY'
import socket
pkt = bytes.fromhex("002ac0070004") + b"hello"  # TM, APID 0x02a, seq 7, 5-byte payload
socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(pkt, ("127.0.0.1", 7301))
PY
```

The gateway should log `telemetry frame parsed` with APID `42`, sequence `7`, payload length `5`,
and any available tracking fields / `physics_flags`.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
integration tests over loopback UDP, future in-process WebSockets, doctests, and physics
co-validation tests with explicitly documented tolerances — enforced at every milestone's stage
gate. Today, M2-M4 behavior is covered by inline unit tests and M1 by loopback integration tests.
The full strategy and per-milestone test matrix are defined in [`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- [`BUILD_PLAN.md`](BUILD_PLAN.md) — milestone scope, stage gates, and remaining work.
- [`TEST_PLAN.md`](TEST_PLAN.md) — layered test matrix, test counts, and tolerance register.
- [`Methodology.md`](Methodology.md) — decision log, trade-offs, and dependency attribution.
- [`AGENTS.md`](AGENTS.md) — compliance, security, attribution, and testing constitution.
- [CCSDS](https://public.ccsds.org/) — open packet and telemetry standards used for wire formats.
- [NASA Open MCT](https://nasa.github.io/openmct/) — target dashboard framework for Milestone 5.
- [Tokio](https://tokio.rs/) and [Axum](https://github.com/tokio-rs/axum) — async runtime and
  planned web distribution framework.

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
