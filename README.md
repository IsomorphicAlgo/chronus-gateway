# ChronusGateway-RS

ChronusGateway-RS (Chronus-GS) is an asynchronous, physics-validated Telemetry and Command
(TMTC) ground-station gateway written in Rust. It ingests raw spacecraft downlink frames,
parses them against open CCSDS standards, and cross-checks each frame against the spacecraft's
computed orbital physics before downstream distribution — in a single, memory-safe,
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

The gateway is built around two principles: a bounded asynchronous network core and a clean
abstraction boundary between the pipeline and any astrodynamics backend.

```
Synthetic/public RF source ──▶ Async UDP ingestion ──▶ CCSDS zero-copy parser
       (UDP)                     (Tokio)                 (TelemetryFrame)
                                                              │
                                                              ▼
                  OrbitalPropagator trait ◀── range-rate / look-angles
                  (Ephemerust today, nyx-space later)         │
                                                              ▼
                                          Physics-Telemetry Co-Validation
                                          (Doppler + elevation flags)
                                                              │
                                                              ▼
                                          M5: Axum WebSocket / Open MCT
                                          adapter (planned, not shipped)
```

- **Asynchronous core.** A Tokio runtime drives non-blocking UDP ingestion and a pool of
  bounded broadcast subscribers. The WebSocket/Open MCT distribution layer is planned for M5.
- **Trait-based astrodynamics.** Physical-state computation is abstracted behind the
  `OrbitalPropagator` trait, decoupling the network and validation pipelines from the math
  library. The default backend is the SGP4-based Ephemerust library; the trait boundary leaves a
  clean path to a high-fidelity `nyx-space` backend without rewriting the gateway.
- **Stable validation contract.** `TelemetryFrame::physics_flags` carries bitwise anomaly flags:
  bit 0 = Doppler out of tolerance, bit 1 = below configured elevation, bit 2 reserved for future
  RSSI/link-budget checks.

The reasoning behind these and other choices is recorded in [`Methodology.md`](Methodology.md).

---

## Repository layout

```
chronus-gateway/
├── Cargo.toml              Workspace manifest (centralized dependency versions, MSRV 1.88)
├── crates/gateway/         The gateway binary + library
│   ├── src/
│   │   ├── lib.rs          Crate documentation and module wiring
│   │   ├── config.rs       Ingestion + station/TLE configuration
│   │   ├── ingest.rs       Asynchronous UDP ingestion loop (RawFrame, stats, shutdown)
│   │   ├── ccsds.rs        CCSDS Space Packet parsing (TelemetryFrame, validation)
│   │   ├── validate.rs     Physics–Telemetry Co-Validation (Doppler, elevation, physics_flags)
│   │   ├── propagator.rs   OrbitalPropagator trait, Ephemerust backend, TrackingProvider
│   │   └── main.rs         Entrypoint (ingest → parse → track → validate → log)
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
cargo run        # run the UDP ingest/parse/validate demo loop
cargo test       # unit + integration + doctests
```

`cargo run` binds UDP on `127.0.0.1:7301` by default, builds a tracking provider from the
public ISS TLE, and logs each valid telemetry packet after CCSDS parsing and physics validation.
Invalid, truncated, or non-telemetry datagrams are logged and dropped; the receive loop continues.

> **Windows note:** on the maintainer's machine the MSVC `link.exe` is blocked from writing
> freshly linked executables. The repository is therefore configured (`.cargo/config.toml`) to
> link with the toolchain's bundled `rust-lld`. See `Methodology.md` (D-008) for details.

---

## Operational runbook (current binary)

The current executable is a deterministic development gateway, not a live SDR integration.

- **Default ingest:** `127.0.0.1:7301`, channel capacity `1024`, max datagram `65_542` bytes.
- **Default station:** latitude `35.0°`, longitude `-116.0°`, altitude `1000 m`, nominal carrier
  `437_500_000 Hz`, ISS (ZARYA) public TLE, tracking recompute throttle `10 ms`.
- **Validation thresholds:** Doppler tolerance `±150 Hz`; minimum elevation `0°` with strict
  `elevation_deg < minimum_elevation_deg` flagging.
- **Logging:** set `RUST_LOG` to control tracing output, for example
  `RUST_LOG=debug cargo run`. Without `RUST_LOG`, the binary uses `info`.
- **Shutdown:** press Ctrl-C. The gateway logs final ingest counters: frames, bytes, oversized
  datagrams, and receive errors.
- **Graceful degradation:** if the station/TLE configuration cannot build a propagator, ingestion
  and CCSDS parsing continue and logs omit physics fields.
- **Current RF metadata limit:** the demo consumer passes `RfMetadata::default()`, so Doppler
  validation is skipped until measured carrier metadata is wired in; elevation validation still
  runs whenever tracking state is available.

Example synthetic TM packet for local verification while `cargo run` is active:

```bash
python3 - <<'PY'
import socket

# CCSDS TM, APID 0x02A, sequence 7, 5-byte payload "hello".
packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(packet, ("127.0.0.1", 7301))
PY
```

All examples and defaults are synthetic or public reference data in keeping with the project's
ITAR/EAR guardrails.

---

## Testing

Testing is a first-class deliverable. The project follows a layered strategy — inline unit tests,
loopback UDP integration tests, doctests, and physics co-validation tests with explicitly
documented tolerances — enforced at every milestone's stage gate. In-process WebSocket tests land
with M5. The full strategy and per-milestone test matrix are defined in
[`TEST_PLAN.md`](TEST_PLAN.md).

---

## References

- [`BUILD_PLAN.md`](BUILD_PLAN.md) — milestone roadmap and stage-gate status.
- [`TEST_PLAN.md`](TEST_PLAN.md) — test matrix, tolerance register, and `physics_flags` contract.
- [`Methodology.md`](Methodology.md) — decision log, trade-offs, and attribution register.
- [`AGENTS.md`](AGENTS.md) — compliance, attribution, security, and testing rules.
- Source modules: `crates/gateway/src/ingest.rs`, `ccsds.rs`, `propagator.rs`, `validate.rs`,
  and `main.rs`.
- Open standards and upstream projects: CCSDS Space Packet standards, Ephemerust, `spacepackets`,
  `sgp4`, Tokio, Tracing, Serde, NASA Open MCT, and NeXosim.

---

## Acknowledgements

ChronusGateway-RS builds directly on prior work, and credit is given accordingly:

- **[Ephemerust](https://github.com/IsomorphicAlgo/ephemerust)** — the SGP4-based orbital
  mechanics and satellite-tracking library that provides the look-angle and range-rate
  computations underpinning the co-validation engine. Authored by the same maintainer.
- **Rusty_Server** — an earlier asynchronous networking and REST service by the same maintainer,
  whose Tokio/Axum, configuration, and integration patterns informed this gateway's design.
- **[`sgp4`](https://crates.io/crates/sgp4)** — the validated SGP4/SDP4 propagator that
  Ephemerust delegates to for numerical orbit propagation.
- **[Tokio](https://tokio.rs/)**, **Tracing**, and **Serde** — the runtime, observability, and
  serialization crates used by the current gateway.
- **[Axum](https://github.com/tokio-rs/axum)** — the planned M5 WebSocket/HTTP framework, chosen
  to match the Rusty_Server pattern.
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
