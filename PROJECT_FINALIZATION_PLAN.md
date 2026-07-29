# ChronusGateway-RS — iterative project finalization plan

**Audience:** Owner (Michael Hansen). **Scope:** Prepare the workspace for crates.io release and
for confident discussion in Rust community spaces (Discord, forums, etc.), without changing the
public compliance posture (ITAR/EAR, open standards only).

**How to use this document:** Work **one tranche at a time**; do not chain gates. After each
tranche, run `cargo test`, `cargo clippy --all-targets`, and (before release) `cargo publish
--dry-run` for each crate you intend to publish.

**Versioning through this plan:** each tranche that lands bumps the shared
`[workspace.package] version` — **patch** for compatible work (docs, review, hardening), **minor**
for anything breaking (0.x policy, see [`CHANGELOG.md`](CHANGELOG.md)) — with a matching changelog
entry, so the version history mirrors the plan's progress.

---

## Tranche R — Mission-readiness review (Rust-strengths audit)

**Goal:** Before the remaining release tranches, audit the codebase against the reasons Rust is
chosen for mission-critical space software — memory safety without garbage collection, fearless
concurrency, exhaustive compile-time error modeling, and zero-cost abstractions — and confirm the
project **demonstrates** each one well enough to teach from. Every finding becomes either a fix
item in a later tranche or a documented, deliberate exception in `Methodology.md`. This is a
**review** gate: read, measure, record — code changes land in their own tranche afterward.

| Step | Action | Exit criterion |
| ---- | ------ | ---------------- |
| R.1 | **Memory-safety posture.** Confirm the workspace is `unsafe`-free (grep; if any appears later it needs a Methodology entry). Audit panic discipline on the flight-style hot path (ingest → parse → validate → distribute): `unwrap`/`expect`/indexing panics belong at **startup/config only**, never per-frame. Consider `#![forbid(unsafe_code)]` + scoped `clippy::unwrap_used` lints as enforcement. | Done (2026-07-29): **zero `unsafe`** in all three crates; hot-path panic surface = two invariant-backed sites (**F-1**, **F-5** in [`docs/RUST_MISSION_READY.md`](docs/RUST_MISSION_READY.md)); enforcement (`forbid(unsafe_code)`, scoped lints) ticketed as **F-2** → Tranche C. |
| R.2 | **Fearless concurrency.** Verify the sharing story the docs claim: propagator shared `Arc<dyn OrbitalPropagator>` across Tokio workers via `&self` (`Send + Sync`), metrics as atomics, fan-out via `broadcast` with defined lag/drop behavior — **no locks on the per-frame path**. | Done: inventory in [`docs/RUST_MISSION_READY.md`](docs/RUST_MISSION_READY.md); the one `Mutex` (`TrackingProvider` throttle cache) **justified** — `Copy`-only critical sections, SGP4 computed outside the lock (**D-040**); poison-tolerance ticketed (**F-1**). |
| R.3 | **Errors as types, faults as data.** Every boundary fault is a typed `thiserror` enum with a teaching-grade message (Ephemerust's structured-diagnostics standard); telemetry anomalies are **flags, not panics** (`physics_flags`); nothing is silently dropped without a counter. | Done: `CcsdsError` / `HilTmV1DecodeError` / `ConfigError` / `ConfigLoadError` inventory checks out at the Ephemerust bar; two counter gaps found (**F-3** JSON-serialize drop, **F-4** silent propagator degrade) → Tranche C. |
| R.4 | **Zero-cost + determinism.** Re-run Criterion baselines post-0.1.0 (edition 2024 + dep bumps); confirm the parse path stays zero-copy/allocation-lean and physics tests use fixed epochs (no wall-clock flake). | Done: baseline **`chronus-0.1.x-2026-07-29`** saved — `parse_telemetry` ≈ 17 ns, `apply_physics_validation` ≈ 15 ns; physics tests use fixed epochs (HIL clock pinned at `2020-07-12T21:00:00Z`). |
| R.5 | **Compile-time contracts as ops discipline.** Confirm the "if it compiles and CI is green, it deploys" chain: MSRV 1.89 + resolver v3 (enforced floor), edition 2024, clippy `-D warnings`, `cargo audit`/`cargo deny` supply-chain gates, loopback-only defaults. This mirrors how a mission toolchain would gate a build. | Done: `ci.yml` chain verified (MSRV-pinned toolchain → test → clippy `-D warnings` → bench compile → pinned audit/deny → Compose + dashboard guards); MSRV floor proven locally via `cargo +1.89 check` (**D-039**). |
| R.6 | **Educational overlay.** Map each strength above to where the code shows it (let-chains in `validate`, trait-object propagator seam, typed CCSDS errors, …) so docs can point at **living examples**, not claims. Feeds the Ephemerust-style "physical reasoning alongside the code" doc pass. | Done: [`docs/RUST_MISSION_READY.md`](docs/RUST_MISSION_READY.md) — six-strength map + findings table; doc-pass backlog = F-1…F-5 dispositions folded into Tranche C. |

**Tranche R status:** Review **executed** (R.1–R.6, findings **F-1…F-5** recorded in
[`docs/RUST_MISSION_READY.md`](docs/RUST_MISSION_READY.md) and **Methodology D-040**). **Gate
approved 2026-07-29** (owner directed Tranche C execution); all findings landed via **C.0** in
**0.1.2**.

**Dependencies:** None — read/measure/record only. Best done **before** Tranche C (housekeeping)
so the comment/layout pass can fold in R.6's map.

---

## Tranche A — Secondary testing plan (beyond primary `cargo test`)

**Goal:** Add a documented **second line of defense** that catches integration, operational, and
supply-chain issues that unit/integration tests alone rarely cover.

| Step | Action | Exit criterion |
| ---- | ------ | ---------------- |
| A.1 | Extend `TEST_PLAN.md` with a **“Secondary testing”** section: scheduled `cargo mutants` (or agreed mutation tool), `cargo hack` / `--all-features` matrix if features are added later, `cargo miri` on pure unsafe-free hot paths (or document why skipped on Windows), optional `loom` only if concurrency primitives warrant it. | Section merged; commands documented; CI follow-up tracked as optional jobs to avoid flaky overload. |
| A.2 | Add **release rehearsal** checklist: `cargo package -p chronus-gateway`, `cargo package -p chronus-hil-sim`, `cargo package -p chronus-replay`; verify `include`/`exclude` in each `Cargo.toml` matches `Methodology.md` D-025. | Done: see **`TEST_PLAN.md` → Release rehearsal (`cargo package`)**; `exclude` on all three crates. All three crates now package fully — Ephemerust is a published crates.io pin (**D-037**), closing the former **E.2** blocker. |
| A.3 | **Performance regression guard:** document baseline procedure for `cargo bench -p chronus-gateway` (or saved Criterion baselines on a reference machine); optional CI job `bench` on manual dispatch only. | Done: **`TEST_PLAN.md` → Performance regression guard (Criterion)**; **`.github/workflows/bench.yml`** (`workflow_dispatch` + report artifact); **D-030**. |
| A.4 | **Cross-target smoke:** document one non-Windows target (e.g. `x86_64-unknown-linux-gnu` via CI or WSL) as the reference “publish shape” if MSVC-only quirks exist. | Done: see **`TEST_PLAN.md` → Cross-target smoke (Linux publish shape)**; canonical **`ci.yml`** `test` job on `ubuntu-latest` (**D-031**). |
| A.5 | **Manual demo path** (already chartered): keep `docs/Demo_Test.md` in sync when behavior changes; treat S4 fixtures as separate compliance tranche. | Done: **`Demo_Test.md` → Runbook alignment** cross-walk to **`DEMO.md`** Paths A–E; S4 gate closed (**D-033**). |

**Tranche A status:** **Complete** (A.1–A.5). Next: **Tranche B** (narrative, README, acknowledgments).

**Dependencies:** None blocking code; primarily documentation + optional CI workflows.

---

## Tranche B — Narrative, README, and acknowledgments

**Goal:** A visitor understands **why** the project exists in one screenful, then **how** it works,
with every external dependency and inspiration **cited**.

| Step | Action | Exit criterion |
| ---- | ------ | ---------------- |
| B.1 | **README intro narrative:** Lead with a short story: problem (telemetry trust), approach
(co-validation with orbit physics), outcome (validated fan-out to Open MCT–style clients). Move
dense status bullets slightly lower or into a “Current status” subsection so the narrative reads
cleanly on GitHub and crates.io (crates.io shows description + first paragraphs from README if
duplicated in crate metadata — align `description` in `Cargo.toml` with the pitch). | Done: README narrative + **Current status** table; `chronus-gateway` `description` aligned (**D-034**). |
| B.2 | **Acknowledgments audit:** Cross-check `README.md`, `Methodology.md` Attribution / decision
log, and `lib.rs` crate docs against `Cargo.toml` workspace dependencies. Fix broken markdown (e.g.
`sgp4` link formatting in README if still malformed). Add crates.io links for crates that are
first-class (spacepackets, nexosim, etc.) where missing. | Done: README § Acknowledgements tables + Methodology § Attribution aligned with workspace deps; `lib.rs` attribution pointer (**D-035**). |
| B.3 | **Public user guide:** Keep `docs/USER_GUIDE.md` as the operator-facing doc; add a single
sentence in README pointing to it after the narrative. | Done: README sentence after **In short**; USER_GUIDE § doc split + README cross-link (**D-036**). |
| B.4 | **Ephemerust publishing story:** Document in Methodology (or README FAQ) how consumers
without a sibling checkout will get `ephemerust` once it is on crates.io; until then, fork CI
pattern stays canonical. | Done: Ephemerust **0.7** published; workspace pins it from crates.io and CI/Docker drop the sibling checkout (**Methodology D-037**; README § Building and running). |

---

## Tranche C — Housekeeping (comments and layout)

**Goal:** Third-person, reader-facing comments; predictable module layout.

| Step | Action | Exit criterion |
| ---- | ------ | ---------------- |
| C.0 | **Tranche R findings (F-1…F-5):** poison-tolerant lock in `TrackingProvider` (F-1); `#![forbid(unsafe_code)]` on all three crates + scoped `clippy::unwrap_used` consideration (F-2); counter for the JSON-serialize drop path (F-3); `tracking_errors` counter / rate-limited warn for silent propagator degrade (F-4); invariant comment at `TelemetryFrame::payload` index (F-5). See [`docs/RUST_MISSION_READY.md`](docs/RUST_MISSION_READY.md). | Done (0.1.2): all five landed — poison-tolerant lock; `forbid(unsafe_code)` on **all five crate roots** + `cfg_attr(not(test), warn(clippy::unwrap_used))` on the gateway lib; `serialize_errors` + `tracking_errors` counters in `/metrics`; invariant comment. Tests + clippy (with new lints) green. |
| C.1 | **Comment voice pass:** Prefer “Computes …”, “Returns … on error”, not “we/I”. Scan
`crates/gateway/src/**/*.rs`, `crates/chronus-hil-sim`, `chronus-replay` for second-person or
rambling TODOs; convert to imperative or neutral third person per AGENTS.md tone. | Done (0.1.2): full-workspace scan found **one** first-person comment (a test in `tests/ingest.rs`) — reworded; library comments already third-person. |
| C.2 | **Module boundaries:** Confirm each `src/*.rs` has a one-line module doc where public
surface is non-obvious; keep `config/`, `hil_tm` split as today unless a future extract to a
`chronus-ccsds` crate is chartered. | Done (0.1.2): every module carries a `//!` doc header; `lib.rs` module list matches on-disk layout exactly. |
| C.3 | **Dead paths / naming:** Grep for `TODO|FIXME|HACK`; either resolve, ticket in BUILD_PLAN,
or delete stale comments. | Done (0.1.2): grep returns **zero** `TODO`/`FIXME`/`HACK` across `crates/`. |
| C.4 | **demo/ vs crates:** Ensure no `demo/` paths referenced from published crate roots (already
guarded by `exclude`); re-verify after any refactor. | Done (0.1.2): `cargo package --list` on all three crates contains no `demo/`/`showcase/` paths. |

**Tranche C status:** **Complete** (C.0–C.4, shipped as **0.1.2**). Next: **Tranche D** (publish mechanics).

---

## Tranche D — crates.io and community release mechanics

**Goal:** Publishing is boring and repeatable.

| Step | Action | Exit criterion |
| ---- | ------ | ---------------- |
| D.1 | **Versioning policy:** Move `chronus-gateway` from `0.0.0` to `0.1.0` (or agreed `0.x`) for
first publish; document semver expectations for the public API (`lib.rs` exports). | Done: all three
crates at **0.1.0** via shared `[workspace.package] version`; semver policy + release notes in
[`CHANGELOG.md`](CHANGELOG.md); full `cargo package` verified on all three crates. Git tag at owner's
discretion on the release commit. |
| D.2 | **LICENSE:** MIT already; ensure `LICENSE` file at repo root is what crates.io will ship
(workspace members inherit or duplicate per Cargo rules — verify). | Done (0.1.3): MIT `LICENSE`
created at repo root (was **missing** despite the manifest SPDX claim) and copied into each crate
root; `cargo package --list` confirms `LICENSE` in all three tarballs. |
| D.3 | **README on crates.io:** Crates.io displays README from the **crate root**; ensure
`crates/gateway/README.md` exists if the root README should not be the packaged one (Cargo picks
package readme field). | Done (0.1.3): `chronus-gateway` packages the root narrative README via
`readme = "../../README.md"`; `chronus-hil-sim` and `chronus-replay` ship small dedicated crate
READMEs. All three verified in `cargo package --list`. |
| D.4 | **Discord / social one-pager:** Prepare 3–5 bullets + link to repo + “synthetic CCSDS only”
compliance line; store in this file’s appendix or a private note if it should not be public. | Done (0.1.3):
see [Appendix — community one-pager](#appendix--community-one-pager) below. |

**Tranche D status:** **Complete** (D.1–D.4, shipped as **0.1.3**). Publishing itself (`cargo publish`,
git tag) stays an owner action. **Publish order matters:** `chronus-gateway` (and `chronus-replay`)
first, then `chronus-hil-sim` — its registry dependency on `chronus-gateway 0.1.x` must already
exist on crates.io, so a `publish --dry-run` of `chronus-hil-sim` is expected to fail until the
gateway is up. **E** covers post-release upkeep.

---

## Tranche E — Post-release maintenance

| Step | Action |
| ---- | ------ |
| E.1 | After first publish: monitor `cargo audit` / `cargo deny`; bump MSRV only with Methodology entry. |
| E.2 | ~~When Ephemerust hits crates.io: switch path dep to version dep in a deliberate PR; update CI.~~ **Done early** — executed with the Ephemerust 0.7 alignment (**Methodology D-037**): `ephemerust = "0.7"` from crates.io; CI and `demo/Dockerfile` sibling-checkout steps removed. |

---

## Suggested order

1. **A** (secondary test charter + package dry-runs) — low risk, high clarity for release day. **✅ Complete.**  
2. **B** (narrative + acks) — maximizes first-impression quality for GitHub + Discord.  
3. **R** (mission-readiness / Rust-strengths review) — read-only audit; do **before C** so findings
   feed the housekeeping pass instead of following it.  
4. **C** (comments/layout) — incremental; folds in R's fix items and the R.6 strengths map.  
5. **D** then **E** — mechanical publish steps.

---

## Appendix — community one-pager

Paste-ready for Discord / forums / social (D.4). Adjust the opener to taste.

> **ChronusGateway-RS** — a Rust gateway that turns satellite telemetry into trusted, web-ready
> data, and checks it against real orbital physics before your dashboard sees it.
>
> - **Physics as a sanity check:** every CCSDS frame is co-validated against a live SGP4 solution
>   (Doppler, elevation, link budget, eclipse-aware power) — spoofed or corrupt telemetry gets
>   `physics_flags`, not silent trust.
> - **Mission-grade Rust, measurably:** zero `unsafe` (compiler-enforced), panic-free hot path,
>   lock-free fan-out, ~17 ns CCSDS parse — the audit trail is in the repo
>   (`docs/RUST_MISSION_READY.md`).
> - **Batteries included:** NeXosim hardware-in-the-loop simulator, deterministic UDP replay tool,
>   Docker demo stack, and an Open MCT–compatible WebSocket JSON feed.
> - **Built on published work:** SGP4 propagation via the maintainer's
>   [Ephemerust](https://crates.io/crates/ephemerust) crate; CCSDS parsing via `spacepackets`;
>   Tokio/Axum async core.
> - **Teaching-grade docs:** every design decision logged with rationale (`Methodology.md`),
>   tolerances registered per test (`TEST_PLAN.md`).
>
> Repo: <https://github.com/IsomorphicAlgo/chronus-gateway> — MIT licensed.
> *All demo traffic is synthetic CCSDS only; built strictly on open international standards
> (ITAR/EAR public-domain posture — see the README).*

---

*Living finalization plan at repo root until tranches complete; then archive or trim as the owner prefers.*
