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
 (UDP today)      (Tokio)                  (validated frames)         Co-Validation engine
                                                                            │
                  OrbitalPropagator trait ◀── range-rate / look-angles ─────┤
                  (Ephemerust today, nyx-space later)                       ▼
                                                       M5 Axum WebSocket ──▶ NASA Open MCT
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
cargo run        # run the UDP gateway on 127.0.0.1:7301 until Ctrl-C
cargo test       # unit + integration + doctests
```

### Local operator smoke test

The current binary uses code defaults only (no CLI/env/file configuration yet): it binds UDP on
`127.0.0.1:7301`, receives datagrams, parses CCSDS telemetry packets, computes a throttled
tracking state from the default public ISS TLE, and applies the M4 physics flags. Doppler is
skipped in the default run because no SDR/measured-carrier side channel is wired yet; the
elevation gate still runs when propagation succeeds.

Run the gateway with logs:

```bash
RUST_LOG=info cargo run
```

From another shell, send a synthetic CCSDS telemetry packet (APID `0x02a`, sequence `7`, payload
`hello`) to the loopback socket:

```bash
python3 - <<'PY'
import socket

packet = bytes([
    0x00, 0x2A,  # version/type/sec-hdr/APID: TM, APID 0x02a
    0xC0, 0x07,  # sequence flags: unsegmented, sequence count 7
    0x00, 0x04,  # data length: five-byte packet data field minus one
]) + b"hello"

sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

Expected log shape:

- startup: `ChronusGateway-RS listening for telemetry` and, if the default TLE validates,
  `orbital tracking provider ready`;
- valid telemetry: `telemetry frame parsed` with `apid`, `seq`, `payload`, look-angle fields, and
  `physics_flags`;
- malformed or non-telemetry datagrams: `dropping invalid/non-telemetry datagram`, then the loop
  continues;
- shutdown: press Ctrl-C and check the final `shutdown complete` counters.

If the propagator cannot be built (for example, a bad configured TLE in future configuration
wiring), the gateway degrades to ingest + CCSDS parsing and logs telemetry as
`telemetry frame parsed (no physics state)`.

### Configuration defaults

Runtime configuration is currently expressed in Rust defaults (`crates/gateway/src/config.rs`).
Until a CLI/env/local-TOML layer is added, deployments that need different values must change
`IngestConfig` / `StationConfig` in code:

| Setting | Default | Notes |
|---------|---------|-------|
| UDP bind address | `127.0.0.1:7301` | Loopback keeps local development quiet; production will need an explicit NIC or `0.0.0.0`. |
| Broadcast capacity | `1024` frames | Lossy by design: slow subscribers lag/drop instead of blocking the socket loop. |
| Max datagram size | `65_542` bytes | Fixed receive buffer: CCSDS max packet data plus primary header, with bounded allocation. |
| Station | `35.0°`, `-116.0°`, `1000 m` | Synthetic developer default; not an operational ground-station parameter. |
| TLE | Public ISS (ZARYA) inline TLE | Public reference data only; file-backed TLEs are supported by `TleSource::File` in code. |
| Nominal carrier | `437_500_000 Hz` | Used by Doppler validation when measured carrier metadata exists. |
| Recompute throttle | `10 ms` | Caches look-angle/range-rate results for frames in the same short time window. |
| Doppler tolerance | `150 Hz` | See `TEST_PLAN.md` `T-DOPPLER` and `Methodology.md` D-012. |
| Minimum elevation | `0°` | Frames strictly below this threshold set `physics_flags` bit 1. |

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

- [`crates/gateway/src/config.rs`](crates/gateway/src/config.rs) — source of current ingest,
  station, TLE, throttle, Doppler, and elevation defaults.
- [`crates/gateway/src/main.rs`](crates/gateway/src/main.rs) — live runtime wiring:
  ingest → CCSDS parse → tracking → physics co-validation.
- [`crates/gateway/src/ccsds.rs`](crates/gateway/src/ccsds.rs) — CCSDS Space Packet validation
  order and the synthetic packet shape used in tests and smoke commands.
- [`crates/gateway/src/validate.rs`](crates/gateway/src/validate.rs) — Doppler model and stable
  `physics_flags` bitfield contract.
- [CCSDS](https://public.ccsds.org/) — open international space data standards used for packet
  framing.
- [NASA Open MCT](https://nasa.github.io/openmct/) — target dashboard framework for the planned
  M5 distribution layer.

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
