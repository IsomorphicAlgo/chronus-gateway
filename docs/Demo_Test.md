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

**Procedure (Track A — Open MCT)**

1. Follow `demo/openmct/` (or linked repo) README to install Open MCT assets and the Chronus bridge/plugin.
2. Open the hosted Open MCT page; confirm a **realtime** display updates when the gateway streams.
3. Confirm **`physics_flags`** (or derived alarm state) is visible when a synthetic anomaly is injected
   (manual toggle or replay from S3 once available).

**Procedure (Track B — minimal SPA)**

1. `npm install` / `pnpm install` / `bun install` per `demo/dashboard/README.md`; `npm run dev` (or equivalent).
2. Open the local dev URL; confirm WebSocket connects to the gateway URL from env/config.
3. Confirm live fields: at minimum **azimuth_deg, elevation_deg, range_rate_km_s** (when present) and
   **physics_flags** rendered (raw hex/decimal + human-readable badges).

**Pass:** Chosen track’s procedure passes; screenshot archived; **Gate S-2** approved.

---

## S3 — Narrative polish acceptance

**Procedure**

1. **Replay (when implemented):** run replay tool against a committed **JSONL** (or UDP hex) fixture;
   assert **deterministic** flag transitions documented in the fixture README (e.g. “frame 100–110:
   bit0 set”).
2. **Scripted anomaly (optional):** trigger documented mode; WebSocket shows expected **physics_flags**
   change within **N** frames.

**Pass:** Repeatable runbook in `DEMO.md`; owner sign-off **Gate S-3**.

---

## S4 — Optional public fixtures acceptance

**Procedure**

1. Every fixture file has a **README row**: origin, license, hash, and transformation steps.
2. Owner confirms **AGENTS.md** / ITAR posture for each source.
3. If automated test ingests fixture: `cargo test` includes the test and remains **offline**
   (no network fetch during test).

**Pass:** **Gate S-4** approved; fixture set frozen for the tranche.

---

## Revision history

| Date       | Change                          |
|------------|---------------------------------|
| 2026-06-04 | Initial `Demo_Test.md` created. |
| 2026-06-05 | S1: native + Docker procedures; link `DEMO.md`. |
