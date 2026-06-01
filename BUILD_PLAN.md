# ChronusGateway-RS — Iterative Build Plan

An **iterative, stage-gated** roadmap from the current foundation to a physics-validated TMTC
gateway. Governance mirrors Ephemerust's protocol (see `AGENTS.md` rule 4):

> **A milestone is complete only when its deliverables exist, its test gate passes
> (`TEST_PLAN.md`), and its stage-gate checklist is confirmed. Do not chain milestones.**

Prefer small PRs (one milestone or slice per PR). Each milestone cross-references its test gate
in `TEST_PLAN.md` and records rationale in `Methodology.md`.

Legend: `[x]` done · `[ ]` pending · **Gate** = owner approval required to advance.

---

## Milestone 0 — Foundation ✅ **Complete (2026-05-31)**

**Objective:** A compiling workspace with the astrodynamics seam proven end to end.

**Deliverables**
- [x] Cargo workspace (`crates/gateway`), centralized `[workspace.dependencies]`, MSRV 1.88.
- [x] `OrbitalPropagator` trait + `EphemerustPropagator` backend (`src/propagator.rs`).
- [x] `main.rs` smoke test producing a real `TrackingState` from a reference ISS TLE.
- [x] Governance: `AGENTS.md`, `Methodology.md`; build unblocked via `rust-lld` (D-008).

**Test gate:** [TEST_PLAN.md → M0](TEST_PLAN.md#m0--foundation) — smoke run succeeds.

**Gate 0:** [x] Foundation approved; proceed to M1.

---

## Milestone 1 — Asynchronous ingestion loop

**Objective:** Bind a UDP socket and stream raw datagrams onto an internal channel under load,
with clean startup/shutdown. (Derived from the PDF's Milestone 1 / Rusty_Server async patterns.)

**Deliverables**
- [ ] `ingest` module: `tokio::net::UdpSocket` bound to a configurable addr; receive loop into a
      reusable buffer; forward `RawFrame { bytes, received_at, source }` on a
      `tokio::sync::broadcast` channel.
- [ ] Bounded, backpressure-aware channel; lagging-receiver behavior defined (count drops, never
      block the socket).
- [ ] Minimal config (`config` module): bind address, channel capacity, max datagram size.
- [ ] Graceful shutdown (Ctrl-C / cancellation token); structured `tracing` spans + counters
      (frames received, bytes, drops).

**Security:** cap datagram size; never allocate based on attacker-controlled length.

**Test gate:** [TEST_PLAN.md → M1](TEST_PLAN.md#m1--ingestion) — loopback UDP integration test
(send N datagrams → receive N frames), shutdown test, oversized-datagram test.

**Gate 1:** [ ] Ingestion loop reviewed; tests green; proceed to M2.

---

## Milestone 2 — CCSDS framing & zero-copy parsing

**Objective:** Turn raw datagrams into validated, structured telemetry frames.

**Open decision (resolve first):** **OD-A** in `Methodology.md` — crate choice. Default lean:
`spacepackets` (us-irs) for primary **and** secondary headers; revisit `space-packet`
(Kani-verified, primary-only) if a formally-verified hot path is desired.

**Deliverables**
- [ ] `ccsds` module: parse the CCSDS Space Packet primary header (version, type, APID,
      sequence flags/count, data length) with zero-copy slicing where practical.
- [ ] `TelemetryFrame { apid, seq_count, timestamp, payload, physics_flags }` (payload borrowed or
      `bytes::Bytes` to avoid copies; decide and document).
- [ ] Strict validation: length consistency, truncation, APID/type routing; reject TC where TM is
      expected. Educational, structured errors (Ephemerust style).

**Test gate:** [TEST_PLAN.md → M2](TEST_PLAN.md#m2--ccsds-parsing) — golden-frame round-trips,
truncated/oversized/garbage inputs fail gracefully (no panic).

**Gate 2:** [ ] Parser + crate choice approved; tests green; proceed to M3.

---

## Milestone 3 — Propagator integration & station configuration

**Objective:** Make the live astrodynamics state available to the pipeline.

**Deliverables**
- [ ] Station config: observer lat/lon/alt, nominal carrier frequency, TLE source (inline/file).
- [ ] Shared `Arc<dyn OrbitalPropagator>` in app state; compute `TrackingState` for a frame's
      timestamp (with caching/throttling to the documented look-angle rate).
- [ ] TLE refresh strategy stub (manual load now; CelesTrak fetch deferred — see backlog).

**Test gate:** [TEST_PLAN.md → M3](TEST_PLAN.md#m3--propagator-integration) — config parsing,
deterministic tracking-state for fixed TLE+time, trait swap (mock propagator) works.

**Gate 3:** [ ] Integration approved; tests green; proceed to M4.

---

## Milestone 4 — Physics-Telemetry Co-Validation engine

**Objective:** The project's differentiator — cross-check telemetry against physics and flag
anomalies bitwise. (PDF's co-validation model.)

**Open decision:** **OD-C** — finalize tolerance budget against Ephemerust's accuracy posture
(arcminute-level; no precession/nutation). Document each bound in `TEST_PLAN.md`.

**Deliverables**
- [ ] Doppler check: expected Δf from `TrackingState.range_rate_km_s` vs SDR metadata; flag if
      |deviation| > documented bound. Set `physics_flags` bit 0.
- [ ] Look-angle gate: elevation/az sanity vs computed pass geometry (bit 1).
- [ ] (Optional) link-budget / RSSI check (bit 2); solar-aspect/EPS checks deferred.
- [ ] `physics_flags` documented as a stable bitfield for downstream consumers.

**Test gate:** [TEST_PLAN.md → M4](TEST_PLAN.md#m4--co-validation) — in-band frame passes,
out-of-band Doppler/elevation flagged; tolerances asserted with rationale.

**Gate 4:** [ ] Validation engine + tolerances approved; tests green; proceed to M5.

---

## Milestone 5 — Distribution: WebSocket + Open MCT adapter

**Objective:** Serve validated telemetry to web mission control in real time.

**Open decision:** **OD-B** — Axum (mirrors Rusty_Server) for WS + HTTP; confirm Open MCT
telemetry dictionary + JSON contract.

**Deliverables**
- [ ] Axum server; `GET /telemetry/openmct` WebSocket upgrade; subscribe to the broadcast channel
      and stream JSON frames (including `physics_flags`).
- [ ] Open MCT-shaped payloads (telemetry dictionary + historical query endpoint stub).
- [ ] HTTP `GET /health`; per-client lifecycle, backpressure, and disconnect handling.

**Test gate:** [TEST_PLAN.md → M5](TEST_PLAN.md#m5--distribution) — in-process WS test
(ingest → parse → validate → receive JSON), health endpoint, client-drop handling.

**Gate 5:** [ ] Distribution approved; tests green. **Core gateway (M1–M5) functional.**

---

## Milestone 6 — Hardening & observability

**Objective:** Production-grade resilience and the numbers to back up performance claims.

**Deliverables**
- [ ] Metrics: frame latency histograms, throughput, drop/anomaly counters.
- [ ] `criterion` benchmarks for parse + validate hot paths (real latency figures, not the
      marketing table).
- [ ] `cargo audit` / `cargo deny` in the workflow; error taxonomy review; fuzz the parser.

**Test gate:** [TEST_PLAN.md → M6](TEST_PLAN.md#m6--hardening) — benches run; audit clean;
fuzz/property tests on the parser.

**Gate 6:** [ ] Hardening approved.

---

## Milestone 7 — Hardware-in-the-loop simulation (NeXosim) — **stretch**

**Objective:** Drive the gateway from a simulated spacecraft for realistic profiling.

**Open decision:** **OD-D** — scope a single simulated spacecraft on the laptop before any
multi-node/rack topology.

**Deliverables**
- [ ] NeXosim model emitting EPS/thermal/ADCS values as CCSDS over UDP.
- [ ] Load/latency profiling harness; documented results feeding M6 metrics.

**Test gate:** [TEST_PLAN.md → M7](TEST_PLAN.md#m7--hil-simulation) — sim→gateway smoke + a
sustained-rate soak run.

**Gate 7:** [ ] HIL approved. **Portfolio-complete.**

---

## Dependency / ordering notes
- M1 → M2 → M3 → M4 → M5 is the critical path; M6 runs alongside M4–M5; M7 is optional/last.
- Resolve **OD-A** (CCSDS crate) before M2 code, **OD-C** (tolerances) before M4 code, **OD-B**
  (Open MCT contract) before M5 code. Record outcomes in `Methodology.md`.

*Last updated: 2026-05-31.*
