# ChronusGateway-RS — Demo & Showcase Acceptance

**Companion:** [`SHOWCASE_PLAN.md`](SHOWCASE_PLAN.md) (iterative, **owner-gated** roadmap),
[`TEST_PLAN.md`](../TEST_PLAN.md) (checkbox gates S0–S4). Operator runbook: [`DEMO.md`](DEMO.md).

This document holds **manual and semi-automated acceptance** for showcase work. It is **not** a
replacement for `cargo test` on the gateway library — it defines what “demo-ready” means for each
**Gate S-*** milestone.

---

## Global rules

1. **Synthetic-first:** default demos use `chronus-hil-sim` + in-repo TLEs and CCSDS/HIL patterns
   documented in [`USER_GUIDE.md`](USER_GUIDE.md).
2. **External bytes (S4 only):** require a row in **`demo/fixtures/README.md`** with **source,
   license, retrieval date**, and confirmation they are **not** export-controlled or proprietary
   operational data (see [`AGENTS.md`](../AGENTS.md)).
3. **Record evidence:** for gate reviews, attach or archive: terminal log snippet, one redacted
   WebSocket JSON line, and (for S2) a screenshot or short screen capture.

---

## Runbook alignment (`DEMO.md` ↔ acceptance)

When demo behavior or paths change, update **both** [`DEMO.md`](DEMO.md) (operator commands) and the
matching section below before closing a showcase or finalization gate. Cross-walk:

| `DEMO.md` path | Showcase gate | `Demo_Test.md` section |
|----------------|---------------|-------------------------|
| Path A / B (native + Docker spine) | **S1** | [§S1](#s1--demo-spine-acceptance) |
| Path C (Vite dashboard) | **S2** | [§S2](#s2--dashboard-v1-acceptance) |
| Path D (`chronus-replay` + scripted HIL) | **S3** | [§S3](#s3--narrative-polish-acceptance) |
| Path E (`demo/fixtures/` ISS + AMSAT) | **S4** | [§S4](#s4--optional-public-fixtures-acceptance) |

**Finalization plan A.5:** this table is the maintenance contract — `Demo_Test` procedures must stay
aligned with `DEMO.md` paths, ports, and commands (recorded in **`Methodology.md` D-033**).

---

## S0 — Showcase charter acceptance

**Procedure**

1. Confirm [`SHOWCASE_PLAN.md`](SHOWCASE_PLAN.md) lists S0–S4 and governance matches `BUILD_PLAN` style.
2. Confirm [`TEST_PLAN.md`](../TEST_PLAN.md) includes **Showcase tracks** with S0–S4 headings (anchors for links).
3. Confirm this file (`Demo_Test.md`) exists and is linked from [`README.md`](../README.md).

**Pass:** All three confirmed; owner records **Gate S-0** approval in `SHOWCASE_PLAN.md` / project notes.

**Status:** **Gate S-0** approved 2026-06-04 — proceed to S1 per [`SHOWCASE_PLAN.md`](SHOWCASE_PLAN.md).

---

## S1 — Demo spine acceptance

**Prerequisites**

- **Native path:** Rust MSRV+; Ephemerust sibling checkout (see [`README.md`](../README.md)).
- **Docker path:** Docker with Compose v2 only (Ephemerust is cloned **inside** the image build).

**Procedure**

1. Start stack per **[`DEMO.md`](DEMO.md)** — either **Path A (native)** or **Path B (Docker)** as written there
   (Docker: from repo root, `docker compose -f demo/docker-compose.yml up -d --build --wait`).
2. **`GET /health`** — expect HTTP **200** and JSON body indicating healthy status (exact shape per
   `crates/gateway/src/http.rs` at review time).
3. **WebSocket** — connect to `GET /telemetry/openmct` (upgrade); after HIL or UDP feed starts,
   receive **≥ 1** text frame whose JSON parses and includes:
   - `chronus_schema == "openmct.realtime.v1"`
   - numeric `apid`, `seq_count`, `physics_flags`, `payload_base64`
4. **`GET /api/v1/chronus/metrics`** — expect **200** and finite numeric fields after at least one
   frame ingested (counters move from zero).

**Pass:** Steps 2–4 succeed on a clean machine following only repo docs; **Gate S-1** approved.

**Failure triage:** port collisions → document port overrides; missing Ephemerust (native) → README layout;
Docker build failures → see `DEMO.md` troubleshooting; Windows firewall → document loopback-only binds.

---

## S2 — Dashboard v1 acceptance

**Primary (Track B — implemented):** [`demo/dashboard/`](../demo/dashboard/) Vite app per [`docs/DEMO.md`](DEMO.md) **Path C**.

**Procedure (Track B)**

1. Start gateway + UDP feeder (**Path A** or **B** in [`DEMO.md`](DEMO.md)).
2. `cd demo/dashboard && npm install && npm run dev` (Node **22+** LTS recommended; avoid EOL Node majors).
3. Open the Vite dev URL; click **Connect**; confirm **≥ 1** frame updates **APID / seq / time** and
   **physics_flags** badges (or “No physics alarms” when flags are zero).
4. Confirm **azimuth**, **elevation**, **range**, **range-rate** cells update when propagator fields are present on the wire.

**Procedure (Track A — Open MCT, backlog)**

1. When implemented, follow [`demo/openmct/README.md`](../demo/openmct/README.md) and re-run steps analogous to Track B above.

**Pass:** Track B procedure passes; screenshot archived for gate review; **Gate S-2** approved.

---

## S3 — Narrative polish acceptance

**Procedure**

1. **Replay:** start gateway (Path A or B). In another terminal, run **Path D** from [`DEMO.md`](DEMO.md) with
   `demo/replay/fixtures/golden_tm.hex` (or `.jsonl`) and **`--repeat 2`**. Confirm **no errors** from `chronus-replay`;
   WebSocket or dashboard shows ingest for the golden **APID 0x2A**; **`GET /api/v1/chronus/metrics`** counters advance.
2. **Scripted HIL anomaly:** with gateway + dashboard (optional), run  
   `cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 500 --scripted-anomaly body-rate --anomaly-after-frame 50 --anomaly-frame-count 30`  
   Confirm the dashboard **`physics_flags`** shows **CV-5** (bit **6**, body rate) during the injection window, then clears when the sim returns to nominal ramp (see [`HIL.md`](HIL.md)).

**Pass:** Repeatable runbook in `DEMO.md` Path D + scripted HIL above; **Gate S-3** approved (2026-06-04).

---

## S4 — Optional public fixtures acceptance

**Prerequisites:** [`demo/fixtures/README.md`](../demo/fixtures/README.md) committed; owner has read compliance rows.

**Procedure**

1. **Provenance:** Confirm README rows for **ISS** (`iss/clean.hex`, `iss/robustness.hex`) and **AMSAT**
   (`amsat/clean.hex`, `amsat/robustness.hex`) — source, license, SHA-256, transformation, AGENTS note.
2. **Owner compliance:** Confirm each source satisfies [`AGENTS.md`](../AGENTS.md) / ITAR-EAR posture (no
   operational dumps, no controlled RF parameters).
3. **Automated:** `cargo test -p chronus-gateway --test s4_fixtures` — **offline**, no network fetch.
4. **Manual replay (clean):** gateway up; run Path E from [`DEMO.md`](DEMO.md) for `iss/clean.hex` and
   `amsat/clean.hex`; WebSocket or dashboard shows APID **0x155** and **0x073** respectively.
5. **Robustness (optional narrative):** replay `iss/robustness.hex`; confirm gateway stays healthy and
   invalid lines do not crash ingest (metrics / logs only).

**Pass:** Steps 1–4 succeed; **Gate S-4** approved.

**Status:** **Gate S-4** approved 2026-06-19 — showcase track **S0–S4** complete.

---

## Revision history

| Date       | Change                          |
|------------|---------------------------------|
| 2026-06-04 | Initial `Demo_Test.md` created. |
| 2026-06-05 | S1: native + Docker procedures; link `DEMO.md`. |
| 2026-06-05 | S2: Track B dashboard acceptance; Open MCT backlog pointer. |
| 2026-06-19 | **Gate S-4** approved; runbook alignment table (finalization **A.5**). |
