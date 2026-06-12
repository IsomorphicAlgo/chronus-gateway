# ChronusGateway-RS — iterative project finalization plan

**Audience:** Owner (Michael Hansen). **Scope:** Prepare the workspace for crates.io release and
for confident discussion in Rust community spaces (Discord, forums, etc.), without changing the
public compliance posture (ITAR/EAR, open standards only).

**How to use this document:** Work **one tranche at a time**; do not chain gates. After each
tranche, run `cargo test`, `cargo clippy --all-targets`, and (before release) `cargo publish
--dry-run` for each crate you intend to publish.

---

## Tranche A — Secondary testing plan (beyond primary `cargo test`)

**Goal:** Add a documented **second line of defense** that catches integration, operational, and
supply-chain issues that unit/integration tests alone rarely cover.

| Step | Action | Exit criterion |
| ---- | ------ | ---------------- |
| A.1 | Extend `TEST_PLAN.md` with a **“Secondary testing”** section: scheduled `cargo mutants` (or agreed mutation tool), `cargo hack` / `--all-features` matrix if features are added later, `cargo miri` on pure unsafe-free hot paths (or document why skipped on Windows), optional `loom` only if concurrency primitives warrant it. | Section merged; commands documented; CI follow-up tracked as optional jobs to avoid flaky overload. |
| A.2 | Add **release rehearsal** checklist: `cargo package -p chronus-gateway`, `cargo package -p chronus-hil-sim`, `cargo package -p chronus-replay`; verify `include`/`exclude` in each `Cargo.toml` matches `Methodology.md` D-025. | Done: see **`TEST_PLAN.md` → Release rehearsal (`cargo package`)**; `exclude` on all three crates; `chronus-replay` passes full `cargo package`; gateway/HIL blocked until Ephemerust + version pins (**E.2**). |
| A.3 | **Performance regression guard:** document baseline procedure for `cargo bench -p chronus-gateway` (or saved Criterion baselines on a reference machine); optional CI job `bench` on manual dispatch only. | Done: **`TEST_PLAN.md` → Performance regression guard (Criterion)**; **`.github/workflows/bench.yml`** (`workflow_dispatch` + report artifact); **D-030**. |
| A.4 | **Cross-target smoke:** document one non-Windows target (e.g. `x86_64-unknown-linux-gnu` via CI or WSL) as the reference “publish shape” if MSVC-only quirks exist. | At least one clean Linux build recorded before first crates.io push. |
| A.5 | **Manual demo path** (already chartered): keep `docs/Demo_Test.md` in sync when behavior changes; treat S4 fixtures as separate compliance tranche. | Demo_Test steps still match `docs/DEMO.md`. |

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
duplicated in crate metadata — align `description` in `Cargo.toml` with the pitch). | README
skimmable in ~60 seconds; narrative is owner-accurate. |
| B.2 | **Acknowledgments audit:** Cross-check `README.md`, `Methodology.md` Attribution / decision
log, and `lib.rs` crate docs against `Cargo.toml` workspace dependencies. Fix broken markdown (e.g.
`sgp4` link formatting in README if still malformed). Add crates.io links for crates that are
first-class (spacepackets, nexosim, etc.) where missing. | No orphan dependency without rationale
in Methodology or README. |
| B.3 | **Public user guide:** Keep `docs/USER_GUIDE.md` as the operator-facing doc; add a single
sentence in README pointing to it after the narrative. | No duplicate maintenance burden. |
| B.4 | **Ephemerust publishing story:** Document in Methodology (or README FAQ) how consumers
without a sibling checkout will get `ephemerust` once it is on crates.io; until then, fork CI
pattern stays canonical. | Clear expectations for `cargo add` users. |

---

## Tranche C — Housekeeping (comments and layout)

**Goal:** Third-person, reader-facing comments; predictable module layout.

| Step | Action | Exit criterion |
| ---- | ------ | ---------------- |
| C.1 | **Comment voice pass:** Prefer “Computes …”, “Returns … on error”, not “we/I”. Scan
`crates/gateway/src/**/*.rs`, `crates/chronus-hil-sim`, `chronus-replay` for second-person or
rambling TODOs; convert to imperative or neutral third person per AGENTS.md tone. | Spot-check:
no large blocks of “I” in library code. |
| C.2 | **Module boundaries:** Confirm each `src/*.rs` has a one-line module doc where public
surface is non-obvious; keep `config/`, `hil_tm` split as today unless a future extract to a
`chronus-ccsds` crate is chartered. | `lib.rs` module list matches on-disk layout. |
| C.3 | **Dead paths / naming:** Grep for `TODO|FIXME|HACK`; either resolve, ticket in BUILD_PLAN,
or delete stale comments. | Grep results are intentional. |
| C.4 | **demo/ vs crates:** Ensure no `demo/` paths referenced from published crate roots (already
guarded by `exclude`); re-verify after any refactor. | `cargo package` tree inspection clean. |

---

## Tranche D — crates.io and community release mechanics

**Goal:** Publishing is boring and repeatable.

| Step | Action | Exit criterion |
| ---- | ------ | ---------------- |
| D.1 | **Versioning policy:** Move `chronus-gateway` from `0.0.0` to `0.1.0` (or agreed `0.x`) for
first publish; document semver expectations for the public API (`lib.rs` exports). | Tags + CHANGELOG
(or `docs/BUILD_PLAN` release notes) agreed. |
| D.2 | **LICENSE:** MIT already; ensure `LICENSE` file at repo root is what crates.io will ship
(workspace members inherit or duplicate per Cargo rules — verify). | `cargo package` contains
license text. |
| D.3 | **README on crates.io:** Crates.io displays README from the **crate root**; ensure
`crates/gateway/README.md` exists if the root README should not be the packaged one (Cargo picks
package readme field). | Published page looks correct. |
| D.4 | **Discord / social one-pager:** Prepare 3–5 bullets + link to repo + “synthetic CCSDS only”
compliance line; store in this file’s appendix or a private note if it should not be public. | Owner can paste
without rereading the whole repo. |

---

## Tranche E — Post-release maintenance

| Step | Action |
| ---- | ------ |
| E.1 | After first publish: monitor `cargo audit` / `cargo deny`; bump MSRV only with Methodology entry. |
| E.2 | When Ephemerust hits crates.io: switch path dep to version dep in a deliberate PR; update CI. |

---

## Suggested order

1. **A** (secondary test charter + package dry-runs) — low risk, high clarity for release day.  
2. **B** (narrative + acks) — maximizes first-impression quality for GitHub + Discord.  
3. **C** (comments/layout) — incremental; can run in parallel with B on different branches.  
4. **D** then **E** — mechanical publish steps.

---

*Living finalization plan at repo root until tranches complete; then archive or trim as the owner prefers.*
