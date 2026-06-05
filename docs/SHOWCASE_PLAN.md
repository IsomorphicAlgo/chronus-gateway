# ChronusGateway-RS — Showcase & Demo Enhancement Plan

**Companion documents:** `[TEST_PLAN.md](../TEST_PLAN.md)` (automated + showcase test gates), `[Demo_Test.md](Demo_Test.md)` (manual / acceptance procedures), `[BUILD_PLAN.md](BUILD_PLAN.md)` (M0–M8 product milestones), `[USER_GUIDE.md](USER_GUIDE.md)`.

This plan turns the gateway into a **credible, repeatable “AAA-style” showcase**: one-command demos, visible mission-control surfaces, optional replay, and explicit **compliance** with open-data rules in `[AGENTS.md](../AGENTS.md)`.

---

## Governance (same as `BUILD_PLAN` / `EXTENDED_COVALIDATION_PLAN`)

> **A showcase stage is complete only when its deliverables exist, its test gate is satisfied
> (`TEST_PLAN.md` + `Demo_Test.md` where applicable), and the stage-gate checklist is confirmed.
> Do not chain stages — obtain owner approval before starting the next tranche.**

Legend: `[x]` done · `[ ]` pending · **Gate** = owner sign-off required to advance.

**Compliance:** Flagship demos use **synthetic** CCSDS + HIL payloads and **public reference TLEs**
already used in-repo. Any **Tier-2** external fixtures (S4) require **written provenance + license**
in `Demo_Test.md` and owner approval — no unclear-origin RF captures or proprietary mission dumps.

---

## Ordering (recommended sequence)

1. **S0** — Lock scope and compliance boundary (charter only).
2. **S1** — Demo spine: reproducible “press play” stack + operator doc.
3. **S2** — Dashboard v1: Open MCT **or** minimal SPA consuming the existing WebSocket contract.
4. **S3** — Narrative polish: replay and/or scripted anomalies for predictable alarm footage.
5. **S4** — Optional curated public fixtures (strictly gated).

Stages **S1–S3** are the **default path** to a portfolio-grade demo. **S4** is optional enrichment.

---

## S0 — Showcase charter (documentation + gates) — **Complete (2026-06-04)**

**Objective:** Approve what “showcase done” means before investing in UI/Docker.

**Deliverables**

- [x] This file (`SHOWCASE_PLAN.md`) and [`Demo_Test.md`](Demo_Test.md) checked into `docs/`.
- [x] [`TEST_PLAN.md`](../TEST_PLAN.md) updated with **S0–S4** gates (high level).
- [x] [`README.md`](../README.md) links this plan and `Demo_Test.md` from the docs list.
- [x] Explicit **data policy** in `Demo_Test.md`: synthetic-first; external bytes only with provenance.

**Test gate:** N/A (documentation). Owner walks `Demo_Test.md` checklist template exists.

**Gate S-0:** `[x]` **Owner approval** of charter (2026-06-04) — **S1** implementation may proceed.

---

## S1 — Demo spine (reproducible stack) — **pending**

**Objective:** One documented path to “gateway + feeder + health + WebSocket + metrics” suitable
for screen recording and CI-smoke (optional).

**Deliverables**

- **Containerized or scripted stack** — e.g. `docker compose` at repo root (or `demo/compose.yaml`)
building/running `chronus-gateway` + `chronus-hil-sim` with loopback addresses and fixed tags.
- `**docs/DEMO.md`** (or `demo/README.md`) — exact commands, URLs, ports, expected first-line
JSON fields (`chronus_schema`, `physics_flags`), and troubleshooting.
- **CI hook (optional):** job step that `compose up --wait`, curls `/health`, tears down — or
documented deferral if runner constraints apply.

**Test gate:** `[TEST_PLAN.md` → S1](../TEST_PLAN.md#s1--demo-spine); procedures `[Demo_Test.md` §S1](Demo_Test.md#s1--demo-spine-acceptance).

**Gate S-1:** `[ ]` **Owner approval** — **S2** may proceed.

---

## S2 — Dashboard v1 (Open MCT *or* minimal SPA) — **pending**

**Objective:** A visible “mission control” surface that consumes the existing
`GET /telemetry/openmct` WebSocket (`chronus_schema: "openmct.realtime.v1"`) and surfaces at least
**tracking fields + `physics_flags`** (alarms / badges).

**Deliverables (pick one primary; second is backlog)**

- **Track A — Open MCT:** thin plugin or bridge page under `demo/openmct/` (or sibling repo
linked from README) with documented install steps for Open MCT stable release.
- **Track B — Minimal SPA:** static `demo/dashboard/` (e.g. Vite + TypeScript) subscribing to the
WebSocket; dark theme; sparklines or cards for az/el/range-rate/flags.

**Test gate:** `[TEST_PLAN.md` → S2](../TEST_PLAN.md#s2--dashboard-v1); procedures `[Demo_Test.md` §S2](Demo_Test.md#s2--dashboard-v1-acceptance).

**Gate S-2:** `[ ]` **Owner approval** — **S3** may proceed.

---

## S3 — Narrative polish (replay / scripted faults) — **pending**

**Objective:** Repeatable demos without live improvisation — same bytes → same flags on every run.

**Deliverables**

- **Replay path (recommended):** CLI or small tool that replays **captured JSONL** or raw UDP hex
at configurable rate; documented in `DEMO.md`.
- **Scripted anomaly mode (optional):** HIL sim or gateway test hook toggles out-of-band Doppler /
pointing / link budget for a fixed number of frames (synthetic only).

**Test gate:** `[TEST_PLAN.md` → S3](../TEST_PLAN.md#s3--narrative-polish); procedures `[Demo_Test.md` §S3](Demo_Test.md#s3--narrative-polish-acceptance).

**Gate S-3:** `[ ]` **Owner approval** — **S4** may proceed (or close showcase track without S4).

---

## S4 — Optional public fixtures (strictly gated) — **pending**

**Objective:** One or two **documented** external frame sources (e.g. amateur-sat public examples)
**only** where license and mission policy are explicit — never the only path to a green demo.

**Deliverables**

- `**demo/fixtures/README.md`** — source URL, license, date retrieved, transformation to UDP
(if any), and **AGENTS.md** compliance note.
- Optional integration test: ingest fixture bytes in-process (deterministic); or manual-only
if bytes are large — decided at **Gate S-3**.

**Test gate:** `[TEST_PLAN.md` → S4](../TEST_PLAN.md#s4--optional-public-fixtures); procedures `[Demo_Test.md` §S4](Demo_Test.md#s4--optional-public-fixtures-acceptance).

**Gate S-4:** `[ ]` **Owner approval** — fixture track closed for this tranche.

---

## Strategic value (why this plan exists)

- **Commercial / hiring:** a one-command demo + visible dashboard converts architecture docs into
**evidence**.
- **Academic / outreach:** reproducible scripts support labs and talks without live RF.
- **Engineering:** replay fixtures regress **JSON contracts** and alarm semantics without NeXosim
runtime when desired.

---

*Last updated: 2026-06-04.*