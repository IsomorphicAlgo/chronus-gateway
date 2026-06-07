# ChronusGateway-RS — Showcase & Demo Enhancement Plan

**Companion documents:** [`TEST_PLAN.md`](../TEST_PLAN.md) (automated + showcase test gates), [`Demo_Test.md`](Demo_Test.md) (manual / acceptance procedures), [`BUILD_PLAN.md`](BUILD_PLAN.md) (M0–M8 product milestones), [`USER_GUIDE.md`](USER_GUIDE.md).

This plan turns the gateway into a **credible, repeatable “AAA-style” showcase**: one-command demos, visible mission-control surfaces, optional replay, and explicit **compliance** with open-data rules in [`AGENTS.md`](../AGENTS.md).

---

## Governance (same as `BUILD_PLAN` / `EXTENDED_COVALIDATION_PLAN`)

> **A showcase stage is complete only when its deliverables exist, its test gate is satisfied
> (`TEST_PLAN.md` + `Demo_Test.md` where applicable), and the stage-gate checklist is confirmed.
> Do not chain stages — obtain owner approval before starting the next tranche.**

Legend: `[x]` done · `[ ]` pending · **Gate** = owner sign-off required to advance.

**Compliance:** Flagship demos use **synthetic** CCSDS + HIL payloads and **public reference TLEs**
already used in-repo. Any **Tier-2** external fixtures (S4) require **written provenance + license**
in `Demo_Test.md` and owner approval — no unclear-origin RF captures or proprietary mission dumps.

### Crates.io vs showcase distribution

**Facts (Cargo):** `cargo publish` uploads only the **crate package root** — for this workspace, that is
`crates/gateway/` for `chronus-gateway` and `crates/chronus-hil-sim/` for the HIL binary. Files in the
**repository root** (`docs/`, `demo/`, `README.md`, etc.) are **not** inside those tarballs unless they are
copied under the crate directory.

**Policy:**

1. **Never** place large showcase assets (Compose stacks, Open MCT forks, SPAs, fixture zips) under
   `crates/gateway/src` or otherwise inside the publishable crate tree. Keep them at **workspace root**
   (recommended: `demo/` for scripts, compose, and static dashboard sources).
2. **`[package] exclude`** on each publishable crate lists `demo` and `showcase` as a safety net if
   those folder names are ever added *inside* a crate directory by mistake (`Methodology.md` **D-025**).
3. **Separate download for “the full booth”:** optional **GitHub Release** attachment (e.g.
   `chronus-showcase-0.1.0.zip`) built by CI containing only `demo/**`, `docs/DEMO.md`, and a short
   `README.txt` — **or** a small **sibling repository** (e.g. `chronus-gateway-showcase`) that documents
   `chronus-gateway = "…"` from crates.io plus vendored compose/SPA. Integrators who `cargo install chronus-gateway`
   get the binary; people who want the **story + UI** clone the repo or fetch the zip.

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

## S1 — Demo spine (reproducible stack) — **Complete (2026-06-05)**

**Objective:** One documented path to “gateway + feeder + health + WebSocket + metrics” suitable
for screen recording and CI-smoke (optional).

**Deliverables**

- [x] **Containerized or scripted stack** — `demo/docker-compose.yml` + `demo/Dockerfile` (build context =
      repo root; clones upstream Ephemerust in-image). Native path documented in [`docs/DEMO.md`](DEMO.md).
- [x] [`docs/DEMO.md`](DEMO.md) + [`demo/README.md`](../demo/README.md) — commands, ports, expected WebSocket JSON keys, troubleshooting.
- [x] **CI hook:** `docker compose … config --quiet` in `.github/workflows/ci.yml` (validates spec without a
      full image build each PR). Full `docker compose up --build` remains manual / release workflow.
- [ ] **(Optional)** Release attachment **`chronus-showcase-*.zip`** — deferred (still optional per S1 charter).

**Test gate:** [`TEST_PLAN.md` → S1](../TEST_PLAN.md#s1--demo-spine); procedures [`Demo_Test.md` §S1](Demo_Test.md#s1--demo-spine-acceptance).

**Gate S-1:** `[x]` **Owner approval** (2026-06-05) — **S2** implementation may proceed.

---

## S2 — Dashboard v1 (Open MCT *or* minimal SPA) — **Complete (2026-06-04); Gate S-2 `[x]`**

**Objective:** A visible “mission control” surface that consumes the existing
`GET /telemetry/openmct` WebSocket (`chronus_schema: "openmct.realtime.v1"`) and surfaces at least
**tracking fields + `physics_flags`** (alarms / badges).

**Deliverables**

- [x] **Track B — Minimal SPA:** [`demo/dashboard/`](../demo/dashboard/) (Vite + TypeScript), dark theme, cards for
      azimuth / elevation / range / range-rate / source / APID / seq, and **`physics_flags`** alarm badges
      (semantics aligned with `validate` / **D-016**). Runbook: [`demo/dashboard/README.md`](../demo/dashboard/README.md).
- [x] **Track A — Open MCT (backlog pointer):** [`demo/openmct/README.md`](../demo/openmct/README.md) documents the
      intended adapter approach and defers full wiring.

**Test gate:** [`TEST_PLAN.md` → S2](../TEST_PLAN.md#s2--dashboard-v1); procedures [`Demo_Test.md` §S2](Demo_Test.md#s2--dashboard-v1-acceptance).

**Gate S-2:** `[x]` **Owner approval** — **S3** may proceed.

---

## S3 — Narrative polish (replay / scripted faults) — **Complete; Gate S-3 `[x]`** (2026-06-04)

**Objective:** Repeatable demos without live improvisation — same bytes → same flags on every run.

**Deliverables**

- [x] **Replay path:** Workspace binary **`chronus-replay`** (`crates/chronus-replay/`) replays **hex lines**
      or **JSONL** with `udp_hex` at configurable `--delay-ms` and **`--repeat`**; fixtures and runbook in
      [`demo/replay/README.md`](../demo/replay/README.md); operator path **Path D** in [`docs/DEMO.md`](DEMO.md).
- [x] **Scripted anomaly mode (optional):** `chronus-hil-sim` **`--scripted-anomaly`** (`eps-voltage` \| `thermal` \| `body-rate`) with **`--anomaly-after-frame`** / **`--anomaly-frame-count`** injects synthetic **CV-4 / CV-5** faults for a bounded 0-based frame window (`physics_flags` bits **4–6**). See [`docs/HIL.md`](HIL.md) and `chronus-hil-sim --help`.

**Test gate:** [`TEST_PLAN.md` → S3](../TEST_PLAN.md#s3--narrative-polish); procedures [`Demo_Test.md` §S3](Demo_Test.md#s3--narrative-polish-acceptance).

**Gate S-3:** `[x]` **Owner approval** (2026-06-04) — **S4** may proceed when desired (owner **holding S4** for now), or close showcase track without S4.

---

## S4 — Optional public fixtures (strictly gated) — **pending (on hold)**

**Objective:** One or two **documented** external frame sources (e.g. amateur-sat public examples)
**only** where license and mission policy are explicit — never the only path to a green demo.

**Deliverables**

- [ ] **`demo/fixtures/README.md`** — source URL, license, date retrieved, transformation to UDP
      (if any), and **AGENTS.md** compliance note.
- [ ] Optional integration test: ingest fixture bytes in-process (deterministic); or manual-only
      if bytes are large — **owner decides when pursuing S4** (currently on hold).

**Test gate:** [`TEST_PLAN.md` → S4](../TEST_PLAN.md#s4--optional-public-fixtures); procedures [`Demo_Test.md` §S4](Demo_Test.md#s4--optional-public-fixtures-acceptance).

**Gate S-4:** `[ ]` **Owner approval** — fixture track closed for this tranche.

---

## Strategic value (why this plan exists)

- **Commercial / hiring:** a one-command demo + visible dashboard converts architecture docs into **evidence**.
- **Academic / outreach:** reproducible scripts support labs and talks without live RF.
- **Engineering:** replay fixtures regress **JSON contracts** and alarm semantics without NeXosim
  runtime when desired.

---

*Last updated: 2026-06-04.*
