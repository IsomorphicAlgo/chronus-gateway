# ChronusGateway-RS — Iterative Build Plan

An **iterative, stage-gated** roadmap from the current foundation to a physics-validated TMTC
gateway. Governance mirrors Ephemerust's protocol (see `../TEST_PLAN.md` / stage-gate notes below):

> **A milestone is complete only when its deliverables exist, its test gate passes
> (`../TEST_PLAN.md`), and its stage-gate checklist is confirmed. Do not chain milestones.**

Prefer small PRs (one milestone or slice per PR). Each milestone cross-references its test gate
in `../TEST_PLAN.md` and records rationale in `../Methodology.md`.

Legend: `[x]` done · `[ ]` pending · **Gate** = owner approval required to advance.

---

## Milestone 0 — Foundation ✅ **Complete (2026-05-31)**

**Objective:** A compiling workspace with the astrodynamics seam proven end to end.

**Deliverables**
- [x] Cargo workspace (`crates/gateway`), centralized `[workspace.dependencies]`, MSRV 1.89.
- [x] `OrbitalPropagator` trait + `EphemerustPropagator` backend (`src/propagator.rs`).
- [x] `main.rs` smoke test producing a real `TrackingState` from a reference ISS TLE.
- [x] Governance: `../Methodology.md`; build unblocked via `rust-lld` (D-008).

**Test gate:** [TEST_PLAN.md → M0](../TEST_PLAN.md#m0--foundation) — smoke run succeeds.

**Gate 0:** [x] Foundation approved; proceed to M1.

---

## Milestone 1 — Asynchronous ingestion loop ✅ **Complete (2026-05-31)**

**Objective:** Bind a UDP socket and stream raw datagrams onto an internal channel under load,
with clean startup/shutdown. (Derived from the PDF's Milestone 1 / Rusty_Server async patterns.)

**Deliverables**
- [x] `ingest` module: `tokio::net::UdpSocket` bound to a configurable addr; receive loop into a
      reusable buffer; forward `RawFrame { bytes, received_at, source }` (cheap-clone `Arc<[u8]>`)
      on a `tokio::sync::broadcast` channel.
- [x] Bounded, lossy broadcast channel; a lagging subscriber observes `Lagged` and never blocks
      the socket loop.
- [x] Minimal config (`config` module): bind address, channel capacity, max datagram size.
- [x] Graceful shutdown (Ctrl-C in `main`, any `Future` in the lib); `#[tracing::instrument]`
      span + atomic counters (`frames_received`, `bytes_received`, `oversized_dropped`,
      `recv_errors`).

**Security:** receive buffer fixed at `max_datagram_size`; no allocation from attacker-controlled
length; oversized datagrams dropped (Windows `WSAEMSGSIZE`) / truncated (Unix) without desync.

**Test gate:** [TEST_PLAN.md → M1](../TEST_PLAN.md#m1--ingestion) — **all green**: ordered delivery,
prompt shutdown, oversized-datagram resilience, backpressure/lag. (4 integration + 2 unit + 1
doctest.)

**Gate 1:** [x] Ingestion loop implemented; tests + clippy green. Ready for M2 on approval.

---

## Milestone 2 — CCSDS framing & zero-copy parsing ✅ **Complete (2026-05-31)**

**Objective:** Turn raw datagrams into validated, structured telemetry frames.

**Resolved decision:** **OD-A** → **`spacepackets` 0.17** (us-irs), primary **and** secondary
header support. Recorded in `../Methodology.md` D-010.

**Deliverables**
- [x] `ccsds` module: parses the CCSDS Space Packet primary header (version, type, APID, sequence
      count, data length, secondary-header flag) via `SpacePacketHeader::from_be_bytes`.
- [x] `TelemetryFrame { apid, seq_count, has_secondary_header, received_at, source,
      physics_flags }` with a **zero-copy** `payload()` borrow into the retained `Arc<[u8]>`.
- [x] Strict validation: header length → header decode → declared-vs-available length → TM/TC
      routing; structured, educational `CcsdsError` (Ephemerust style). `main` now parses frames.

**Test gate:** [TEST_PLAN.md → M2](../TEST_PLAN.md#m2--ccsds-parsing) — **all green**: golden bytes,
round-trip, short/truncated/garbage rejected without panic, TM/TC routing. (7 unit tests.)

**Gate 2:** [x] Parser + `spacepackets` choice implemented; tests + clippy green. Ready for M3.

---

## Milestone 3 — Propagator integration & station configuration ✅ **Complete (2026-05-31)**

**Objective:** Make the live astrodynamics state available to the pipeline.

**Deliverables**
- [x] `StationConfig`: observer lat/lon/alt, nominal carrier frequency, `TleSource` (inline/file),
      recompute-throttle interval; with `validate()` and `resolve_tle_text()` (`ConfigError`).
- [x] `EphemerustPropagator::from_station` + shareable `TrackingProvider` (`Arc<dyn
      OrbitalPropagator>`) that caches/throttles recomputation to the configured look-angle rate.
      `main` now computes a `TrackingState` per parsed frame.
- [x] TLE source supports inline now and file load; CelesTrak fetch deferred (backlog).

**Test gate:** [TEST_PLAN.md → M3](../TEST_PLAN.md#m3--propagator-integration) — **all green**: config
validation (valid/invalid/missing-file), deterministic tracking-state (baseline-locked), mock
trait-swap + throttle. (4 unit tests added.)

**Gate 3:** [x] Integration implemented; tests + clippy green. Ready for M4.

---

## Milestone 4 — Physics-Telemetry Co-Validation engine ✅ **Complete (2026-06-01)**

**Objective:** The project's differentiator — cross-check telemetry against physics and flag
anomalies bitwise. (PDF's co-validation model.)

**Resolved decision:** **OD-C** — Doppler band **±150 Hz** (T-DOPPLER) with rationale vs Ephemerust
range-rate accuracy; elevation gate uses configurable `minimum_elevation_deg` (default `0`°).
Recorded in `../Methodology.md` D-012 and `../TEST_PLAN.md`.

**Deliverables**
- [x] Doppler check: `expected_carrier_hz` from nominal + `range_rate_km_s`; `RfMetadata` optional
      measured carrier; flag if `|Δ| > doppler_tolerance_hz`. Sets `physics_flags` bit 0.
- [x] Elevation gate: flag when `elevation_deg < minimum_elevation_deg` (bit 1). Defaults reject
      below-horizon passes for synthetic demo geometry.
- [x] RSSI / link budget: bit 2 **reserved**, documented; not implemented in this slice.
- [x] Stable `physics_flags` bitfield documented in `validate` module and `../TEST_PLAN.md`.

**Test gate:** [TEST_PLAN.md → M4](../TEST_PLAN.md#m4--co-validation) — **all green**: in-band Doppler,
out-of-band Doppler, horizon, combined, independent bits, no-measured skip, NaN-safe.

**Gate 4:** [x] Validation engine + tolerances implemented; tests + clippy green. Ready for M5.

---

## Milestone 5 — Distribution: WebSocket + Open MCT adapter ✅ **Complete (2026-06-01)**

**Objective:** Serve validated telemetry to web mission control in real time.

**Resolved decision:** **OD-B** — Axum for HTTP + WebSocket; JSON contract `chronus_schema:
"openmct.realtime.v1"` (documented in `../Methodology.md` D-013).

**Deliverables**
- [x] Axum server; `GET /telemetry/openmct` WebSocket upgrade; subscribe to the broadcast channel
      and stream JSON frames (including `physics_flags`).
- [x] Open MCT-shaped payloads (telemetry dictionary + historical query endpoint stub).
- [x] HTTP `GET /health`; per-client lifecycle, backpressure, and disconnect handling.

**Test gate:** [TEST_PLAN.md → M5](../TEST_PLAN.md#m5--distribution) — in-process WS test
(ingest → parse → validate → receive JSON), health endpoint, client-drop handling.

**Gate 5:** [x] Distribution approved; tests green. **Core gateway (M1–M5) functional.**

---

## Milestone 6 — Hardening & observability ✅ **Complete (2026-06-01)**

**Objective:** Production-grade resilience and the numbers to back up performance claims.

**Deliverables**
- [x] Metrics: gateway counters + average processing latency (sum/count), ingest snapshot JSON at
      `GET /api/v1/chronus/metrics`; WebSocket client/message counters.
- [x] `criterion` benchmarks for parse + validate hot paths (`cargo bench -p chronus-gateway`).
- [x] `cargo audit` / `cargo deny` in CI (`deny.toml`); property tests on `parse_telemetry` (no panic).

**Test gate:** [TEST_PLAN.md → M6](../TEST_PLAN.md#m6--hardening) — benches compile/run; audit/deny in CI;
property tests on the parser.

**Gate 6:** [x] Hardening approved.

---

## Milestone 7 — Hardware-in-the-loop simulation (NeXosim) ✅ **Complete (2026-06-03)**

**Objective:** Drive the gateway from a simulated spacecraft for realistic profiling.

**Resolved decision:** **OD-D** — single synthetic spacecraft on the laptop (`chronus-hil-sim` +
NeXosim); multi-node scope is **OD-E** backlog (`../Methodology.md`).

**Deliverables**
- [x] NeXosim model emitting EPS/thermal/ADCS values as CCSDS over UDP (`SpacecraftDemo` +
      `UdpDownlinkBridge` + `encode_synthetic_tm` in `chronus-gateway`).
- [x] Load/latency profiling harness; documented recipe in `docs/HIL.md` (M6 metrics endpoint).

**Test gate:** [TEST_PLAN.md → M7](../TEST_PLAN.md#m7--hil-simulation) — sim→gateway smoke + a
sustained simulated-rate soak with bounded `recv_errors`.

**Gate 7:** [x] HIL approved.

---

## Milestone 8 — File-backed gateway configuration ✅ **Complete (2026-06-01)**

**Objective:** Deploy the gateway with explicit bind addresses, station geometry, and TLE source
without recompiling.

**Resolved decision:** **D-015** — TOML (`toml` crate) with optional top-level `[ingest]` and
`[station]` sections; CLI `--config` / `-c` and `CHRONUS_GATEWAY_CONFIG` (CLI wins when both set).
Recorded in `../Methodology.md`.

**Deliverables**
- [x] `config::file`: deserialize → merge with defaults for omitted sections → `validate()` +
      `resolve_tle_text()` at startup (fail fast on bad TOML, bad sockets, invalid station, or
      unreadable TLE file).
- [x] `chronus-gateway` entrypoint uses `load_effective_gateway_config()` (file if set, else
      historical defaults).
- [x] `gateway.example.toml` at workspace root documents the schema.

**Test gate:** [TEST_PLAN.md → M8](../TEST_PLAN.md#m8--file-configuration) — TOML parse/merge/validation
unit tests in `config/file.rs`.

**Gate 8:** [x] File configuration approved.

---

## Dependency / ordering notes
- M1 → M2 → M3 → M4 → M5 is the critical path; M6 runs alongside M4–M5; M7 is optional; M8 is
  operational polish after the core portfolio.
- **OD-B** (Open MCT contract) resolved at M5 (`../Methodology.md` D-013). **OD-A** (M2) and **OD-C** (M4) are resolved; record any future changes in `../Methodology.md`.

**Extended co-validation (post-M8):** link budget / RSSI, antenna pointing residual, and synthetic subsystem checks are planned under **[`EXTENDED_COVALIDATION_PLAN.md`](EXTENDED_COVALIDATION_PLAN.md)** — same stage-gate rules; **owner approval required between CV milestones** (do not chain). **CV-0** charter is documented (**`../Methodology.md` D-016**, `../TEST_PLAN.md`, `validate` docs); **Gate CV-0** is **approved** — **CV-1** may proceed.

*Last updated: 2026-06-03.*
