# Methodology — ChronusGateway-RS

A living record of **why** the project is built the way it is: major decisions, frameworks,
trade-offs, and the reasoning behind them. Append new entries as decisions are made; do not
silently rewrite history (mark superseded entries). Required reading + maintenance per
`AGENTS.md`.

> Status: **Foundation** (workspace + propagator seam). Ingestion, CCSDS parsing, validation
> engine, and Open MCT WebSocket fan-out are upcoming milestones.

---

## Decision log

### D-001 — Language: Rust
**Decision:** Implement the gateway natively in Rust.
**Why:** The ground segment must process continuous high-rate downlinks under tight latency
without garbage-collection pauses (Java/Yamcs) or GIL contention (Python/cFS GS). Rust gives
memory safety + predictable, GC-free performance, matching the aerospace industry's move toward
memory-safe flight/ground software.
**Trade-off:** Compile-time strictness and no dynamic scripting; acceptable for a static,
high-assurance gateway.

### D-002 — Brand-new project (not an extension of Rusty_Server)
**Decision:** Start a clean repo rather than building onto Rusty_Server.
**Why:** Rusty_Server is a poll-cache-and-serve REST API for space-weather data; ChronusGateway
is a streaming, real-time ingestion + fan-out gateway. The reusable parts of Rusty_Server are
*patterns* (Tokio/Axum setup, config/error/logging layering, the Ephemerust path-dependency
integration), not its domain logic. A focused repo keeps the portfolio narrative clean.
**Credit:** Architectural inspiration from the owner's **Rusty_Server**.

### D-003 — Cargo workspace + centralized dependency versions
**Decision:** Use a Cargo workspace (`crates/*`) with `[workspace.dependencies]` and
`[workspace.package]`; first member is `crates/gateway` (binary + lib).
**Why:** Anticipates clean separation as the system grows (e.g. future `ccsds`/`validation`
crates) and keeps dependency versions and metadata defined once. Members opt in via
`field.workspace = true`.
**Trade-off:** Slightly more structure than a single crate up front; pays off as modules split.

### D-004 — Trait-based astrodynamics (Ephemerust now, nyx-space later)
**Decision:** The validation engine depends only on the `OrbitalPropagator` trait
(`crates/gateway/src/propagator.rs`), which returns a `TrackingState` (az/el/range/range-rate).
The default backend `EphemerustPropagator` wraps `ephemerust::look_angles`.
**Why:** Decouples the network/validation pipeline from any specific math library. Ephemerust
already provides exactly the primitives the co-validation engine needs — crucially
`range_rate_km_s` (line-of-sight velocity) for Doppler, plus azimuth/elevation/slant range for
look-angle checks. A clean seam lets a high-fidelity `nyx-space` backend drop in later without
touching ingestion, validation, or distribution code.
**Credit:** **Ephemerust** (owner's SGP4/astrodynamics crate, built atop the `sgp4` crate).
**Limits noted:** Ephemerust is teaching-grade (~arcminute; no precession/nutation, WGS72 gravity
vs WGS84 geodetic). Adequate for foundation/look-angle/Doppler work; revisit precision tolerances
(e.g. the ±150 Hz Doppler bound) against this error budget before claiming hard accuracy numbers.

### D-005 — Dependency source for Ephemerust: local path
**Decision:** `ephemerust = { path = "../Ephemerust" }` (sibling checkout next to this repo).
**Why:** Tight local co-development; mirrors the proven approach used in Rusty_Server's
`EPHEMERUST_INTEGRATION_PLAN.md`. If third-party builds ever matter, switch to a pinned git `rev`
or a crates.io version (and update this entry).
**Reproducibility:** `0.x` crate — pin intentionally and bump deliberately on breaking minors.

### D-006 — MSRV 1.88 (advisory), no forced toolchain pin yet
**Decision:** Set `rust-version = "1.88"` in `[workspace.package]` to match Ephemerust's MSRV;
do **not** add a `rust-toolchain.toml` forcing a channel for now.
**Why:** The installed toolchain (1.90) satisfies the MSRV. Forcing an exact channel that may not
be installed would trigger surprise downloads/build failures. Add a pinned `rust-toolchain.toml`
later if/when CI reproducibility demands it.

### D-007 — Async runtime: Tokio (multi-threaded)
**Decision:** Use Tokio (`features = ["full"]`) as the async runtime.
**Why:** It's the de-facto standard for high-throughput async networking in Rust and underpins
the planned UDP ingestion loop, broadcast channel fan-out, and Axum WebSocket distribution.
Propagators are `Send + Sync` so a single instance can be shared (`Arc`) across worker threads.

### D-008 — Linker: bundled `rust-lld` instead of MSVC `link.exe` (Windows)
**Decision:** `.cargo/config.toml` points the `x86_64-pc-windows-msvc` target at the toolchain's
bundled `rust-lld.exe` with `-C linker-flavor=lld-link`.
**Why:** On this dev machine the MSVC `link.exe` is blocked from writing freshly-linked
executables — build-script binaries (first hit: `num-traits`) fail deterministically with
`LNK1104` / "Access is denied", even with **Windows Defender stopped** and no process holding the
handle and the build dir writable. This is consistent with an EDR/application-control policy on
`link.exe` itself. `rust-lld` ships with the toolchain, isn't subject to that policy, and links
the identical artifacts; verified clean build + run.
**Alternatives:** A Defender/AV folder exclusion for the toolchain/target would also work but
requires admin. **Scope:** affects only the Windows MSVC target; non-Windows builds/CI unaffected.
**Brittleness:** the absolute path embeds `stable-x86_64-pc-windows-msvc`, which is stable across
`rustup update` (only changes if a specific toolchain version is pinned — see D-006). Update the
path if that changes.

---

## Open decisions (to resolve as milestones land)

- **OD-A — CCSDS parsing crate.** `space-packet` (Kani-verified, `no_std`, **primary header
  only**) vs `spacepackets` (us-irs, secondary headers / PUS) vs a thin in-house parser.
  Likely need secondary-header support for real telemetry. Decide at Milestone 1.
- **OD-B — Web/distribution stack.** Axum (mirrors Rusty_Server) for the WebSocket + HTTP API to
  Open MCT. Confirm the Open MCT telemetry dictionary + JSON format contract.
- **OD-C — Doppler/RSSI tolerance budget.** Validate the ±150 Hz Doppler and link-budget margins
  against Ephemerust's accuracy posture (D-004) before publishing hard numbers.
- **OD-D — HIL simulation (NeXosim).** Optional Milestone 4; scope a single simulated spacecraft
  on a laptop before any multi-node/rack topology.

---

## Attribution
External works this project builds on or is inspired by (keep current per `AGENTS.md` rule 2):

| Work | Role here | Source / License |
|------|-----------|------------------|
| **Ephemerust** (owner) | SGP4 propagation, look-angles, range-rate | local sibling crate, MIT |
| `sgp4` crate | Underlying SGP4/SDP4 numerics (via Ephemerust) | crates.io |
| **Rusty_Server** (owner) | Architectural inspiration (async/Axum/config patterns) | sibling repo |
| Tokio, Axum, Tracing, Serde, Chrono, Anyhow, Thiserror | Runtime/infra crates | crates.io, MIT/Apache-2.0 |
| CCSDS standards | TMTC framing/packet specifications | open international standards |
| NASA Open MCT | Target mission-control dashboard | open source (NASA) |

---

*Last updated: 2026-05-31.*
