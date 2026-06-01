# AGENTS.md — Project Constitution (read first, follow always)

This file is the **canonical, non-negotiable instruction set** for every agent (AI or human)
working in this repository, and a place for the owner to record durable, important information.
If any other instruction conflicts with this file, **this file wins** — stop and ask the owner.

> Owner: **Michael Hansen** ([IsomorphicAlgo](https://github.com/IsomorphicAlgo)).
> Project: **ChronusGateway-RS** — asynchronous, physics-validated TMTC ground-station gateway.

---

## Non-negotiable rules

### 1. Follow ITAR and EAR
- This project is deliberately scoped to **open, international standards (CCSDS)** and is
  developed/published under the **Public Domain and Fundamental Research exclusions of ITAR/EAR**.
- **Do not** introduce, request, paste, or commit any **export-controlled or technical data**:
  no specific weapons/defense-article integration, no controlled performance specifications,
  no real mission keying material, frequencies, or operational parameters of controlled systems.
- Keep all telemetry/RF examples **synthetic and generic** (simulated CCSDS over UDP, public
  reference TLEs such as the ISS). When in doubt about whether something is controlled,
  **stop and ask the owner before proceeding** — do not guess.
- Any feature that could shift the project toward a defense article or controlled service
  requires explicit owner sign-off recorded in `Methodology.md`.

### 2. Give all credit where it's due
- **Attribute every external work**: crates, papers, standards, code snippets, and algorithms.
  Record dependencies, their licenses, and any borrowed design/idea in `Methodology.md`
  (Attribution section) and in code comments at the point of use.
- Preserve upstream **LICENSE** and **copyright** notices. Do not relicense or strip headers.
- This project builds on the owner's **Ephemerust** (SGP4/astrodynamics) and was inspired by
  the owner's **Rusty_Server** (async/Axum patterns). Credit them explicitly in docs.
- Never present third-party or AI-generated work as original without saying so.

### 3. Security is a big priority
- **No secrets in the repo.** No API keys, tokens, passwords, private keys, or `credentials.txt`.
  Use environment variables / ignored local config (see `.gitignore`). Flag any secret you find.
- **Memory safety & robustness:** prefer safe Rust. Any `unsafe` block requires a `// SAFETY:`
  justification and owner review. Validate and bound **all** untrusted network input
  (length checks, no unbounded allocation) — this is a network-facing gateway.
- **Dependencies:** minimize the tree, pin intentionally, prefer well-maintained crates, and
  record why each dependency was added. Consider `cargo audit`/`cargo deny` before release.
- **No silent failure:** surface validation/anomaly conditions explicitly; never swallow errors
  on the ingestion or distribution path.
- Do not exfiltrate code or data to external services beyond what a task explicitly requires.

### 4. Clear, layered testing — the Ephemerust standard
Testing is a first-class deliverable, **not** an afterthought. Match the rigor of the owner's
**Ephemerust** crate (~88 inline unit tests + integration tests + doctests, with documented
physics tolerances). Concretely:
- **Layer the tests:**
  - **Unit** — inline `#[cfg(test)] mod tests` in each module, covering the happy path and
    edge/error cases.
  - **Integration** — end-to-end tests in `tests/` (async via `#[tokio::test]`; use loopback
    UDP and in-process Axum/WebSocket rather than live hardware).
  - **Doctests** — runnable, asserting examples on public API items.
  - **Physics co-validation** — verify computed Doppler/look-angle/link-budget results against
    known references or numerical cross-checks (e.g. central-difference), with **every tolerance
    written down and justified** (mirrors Ephemerust's 0.25 km/s / 0.05 km style).
  - **Robustness/security** — malformed, truncated, and oversized inputs must fail gracefully
    (no panics, no unbounded allocation) on the network path.
- **Deterministic & offline:** tests must not depend on live SDR/network or wall-clock time; use
  synthetic CCSDS frames, fixed timestamps, and public reference TLEs.
- **Stage-gate protocol (from Ephemerust):** a milestone is complete only when its deliverables
  exist, **its test plan passes**, and its stage-gate criteria are confirmed. Never chain
  milestones. See `BUILD_PLAN.md` + `TEST_PLAN.md`.
- **`cargo test` must be green** before declaring work done; keep the test-count/status current
  in the plans when behavior or tests change.

---

## Working agreement for agents
- **Keep `Methodology.md` current.** Any major decision (framework, dependency, architecture,
  trade-off) must be logged there with reasoning. This is a hard requirement, not optional.
- Make **small, reviewable changes**; explain trade-offs; prefer asking over assuming on scope.
- Don't commit unless the owner asks. Never `git push --force` or rewrite shared history.
- Match existing conventions; run `cargo check`/`cargo test` before declaring work done.

---

## Owner scratchpad (durable notes)
Use this section to capture important, long-lived information for future sessions.

- **Build/linker:** this machine's MSVC `link.exe` is blocked from writing executables, so the
  repo is configured to link with the toolchain's bundled `rust-lld` (`.cargo/config.toml`).
  If builds suddenly fail with `LNK1104` / "Access is denied", check that config and Methodology
  D-008. Do **not** revert to `link.exe` without a working alternative.
- **Layout:** `ephemerust` is consumed as a sibling path dependency (`../Ephemerust`); keep both
  repos checked out next to each other.

---

*Last updated: 2026-05-31.*
