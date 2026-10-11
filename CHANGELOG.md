# Changelog

ChronusGateway-RS follows [Semantic Versioning](https://semver.org/). During `0.x`, the **minor**
version marks breaking changes and the **patch** version marks compatible ones (same policy as
the Ephemerust backend); a `1.0.0` release is reserved for a deliberately committed-stable API.
Versions cover all three workspace crates (`chronus-gateway`, `chronus-hil-sim`,
`chronus-replay`), which share one version via `[workspace.package]`.

## Unreleased

Operator-facing diagnostics and test-suite expansion. Contains **breaking** API additions for
exhaustive matches on `ConfigError` (two new variants), so this targets **0.2.0**.

- **New `diagnose` module:** every error type (`ConfigError`, `ConfigLoadError`, `CcsdsError`,
  `HilTmV1DecodeError`, socket bind failures, Ephemerust TLE/propagation errors) can explain
  itself as a `Diagnosis` — a `CG-*` code, severity, what happened, likely cause and numbered
  fixes. Reuses Ephemerust's `TleError::hint()` for TLE problems.
- **Smart catches:** swapped latitude/longitude; tolerance entered as a percentage; carrier
  frequency entered in MHz/kHz; stale (> 30 days) or future-dated TLE; port in use (suggests the
  next port + `netstat`/`lsof`); privileged port; IP not on this machine; missing config/TLE file
  (shows the working directory); TOML typo vs missing field vs syntax error.
- **`IngestConfig::validate`:** rejects `channel_capacity = 0` (previously a Tokio panic at
  startup) and `max_datagram_size` outside `6..=1 MiB`.
- **`GET /api/v1/chronus/status`:** plain-English health report built from live counters
  (waiting for data, nothing parses, high reject ratio, oversized packets, physics off/failing,
  mostly-anomalous stream, no dashboard connected).
- **Binary:** startup failures print a diagnosis and exit with code 2 instead of an error chain;
  config warnings and TLE age are reported at startup; a "Gateway is running" banner lists the
  UDP, WebSocket and status URLs. The first invalid packet / tracking failure is explained once.
- **Tests:** 68 → 157 unit tests (new coverage for `diagnose`, `http`, `ingest`, `metrics`,
  `state`, ingest validation); doctests 1 → 26.
- **Public per-visitor sandbox (`chronus-demo-server`, unpublished crate):** each WebSocket on
  `/sandbox/ws` gets a private time-accelerated synthetic spacecraft with its own clock and
  gateway state; visitors inject eight scenarios (Doppler spoof, eclipse power mismatch, pointing
  error, weak signal, EPS/thermal/body-rate faults) and a horizon-mask toggle. Global and per-IP
  session caps, `Origin` allow-list, hard session lifetime, tiny inbound messages. Static visitor
  page in `demo/sandbox/`; Fly.io config in `fly.toml` + `demo/Dockerfile.public`;
  runbook in `docs/PUBLIC_DEMO.md`.
- **`chronus-gateway`:** `http::process_frame` is now public and `http::process_frame_with_rf`
  accepts receiver-chain measurements (carrier, power, az/el) so embedders can exercise the
  Doppler, link-budget and pointing checks without a UDP socket.
- **`chronus-hil-sim`:** new `nominal_sample`, `default_hil_epoch_utc`; `hil_targets_from_illum`
  is public (the NeXosim driver now uses `nominal_sample`, behaviour unchanged).

## 0.1.4 — 2026-07-29

crates.io discoverability polish. No code changes.

- **Keywords + categories** added to all three crate manifests (e.g. `ccsds`, `telemetry`,
  `sgp4`, `hil`; categories under `aerospace::*`, `network-programming`, `simulation`,
  `development-tools::testing`) so the crates surface in crates.io keyword and category browsing.

## 0.1.3 — 2026-07-29

Finalization **Tranche D** — crates.io release mechanics. No code changes.

- **License shipped (D.2):** MIT `LICENSE` file created at the repo root — it was missing
  entirely despite the manifest's SPDX declaration — and copied into each crate root, so all
  three crates.io tarballs now contain the license text.
- **crates.io READMEs (D.3):** `chronus-gateway` packages the root narrative README
  (`readme = "../../README.md"`); `chronus-hil-sim` and `chronus-replay` ship small dedicated
  READMEs. Verified present in all three `cargo package` listings.
- **Community one-pager (D.4):** paste-ready project summary with the synthetic-CCSDS compliance
  line added as an appendix to `PROJECT_FINALIZATION_PLAN.md`.
- **Publish-order note:** `chronus-hil-sim` depends on `chronus-gateway` from the registry —
  publish the gateway first.

## 0.1.2 — 2026-07-29

Finalization **Tranche C** — housekeeping + Tranche R findings (F-1…F-5 all landed). Compatible
changes only; the metrics JSON gains two additive fields.

- **Hardening (F-1, F-2):** `TrackingProvider`'s throttle-cache lock is now poison-tolerant
  (`PoisonError::into_inner`) with the invariant documented; `#![forbid(unsafe_code)]` added to
  **all five crate roots** and `#![cfg_attr(not(test), warn(clippy::unwrap_used))]` to the gateway
  library — CI's `-D warnings` makes both enforced, and the workspace passes with zero violations.
- **Observability (F-3, F-4):** new `GatewayMetrics` counters exposed at
  `/api/v1/chronus/metrics` — `serialize_errors` (JSON-encode drop path, expected zero) and
  `tracking_errors` (per-frame propagator failures, distinguishing "physics off" from "physics
  failing", e.g. a stale TLE outside SGP4 validity).
- **Housekeeping (C.1–C.4):** comment-voice pass (one first-person test comment reworded; zero
  `TODO`/`FIXME`/`HACK` in `crates/`); module docs verified against `lib.rs` layout; `cargo
  package` listings re-verified clean of demo/showcase paths. Tranche R gate approved; findings
  dispositions updated in [`docs/RUST_MISSION_READY.md`](docs/RUST_MISSION_READY.md).

## 0.1.1 — 2026-07-29

Finalization **Tranche R** — mission-readiness review (Rust-strengths audit). Documentation and
measurement only; no code behavior changed.

- **New:** [`docs/RUST_MISSION_READY.md`](docs/RUST_MISSION_READY.md) — the map from each Rust
  mission-critical strength (memory safety, fearless concurrency, typed errors, zero-cost
  abstractions, compile-time contracts, graceful degradation) to the code that demonstrates it,
  plus review findings **F-1…F-5** with dispositions (scheduled into Tranche C).
- **Measured:** zero `unsafe` across all crates; hot-path panic surface limited to two
  invariant-backed sites; fresh Criterion baseline `chronus-0.1.x-2026-07-29`
  (`parse_telemetry` ≈ 17 ns, `apply_physics_validation` ≈ 15 ns). Recorded as Methodology
  **D-040**; owner gate on findings pending.
- **Policy:** each finalization-plan tranche now bumps the shared workspace version with a
  changelog entry (patch = compatible, minor = breaking during 0.x).

## 0.1.0 — 2026-07-29

First versioned release line (previously `0.0.0` pre-release development). Not yet on crates.io.

### Highlights

- **Full pipeline (roadmap M0–M8):** async UDP ingest → CCSDS Space Packet parsing →
  Physics–Telemetry Co-Validation → Axum WebSocket distribution (`openmct.realtime.v1` JSON),
  metrics, NeXosim HIL simulator, and optional TOML configuration.
- **Extended co-validation CV-1…CV-5:** Doppler, elevation mask, link budget, pointing residual,
  and synthetic HIL subsystem envelopes (`physics_flags` bits; see
  `docs/EXTENDED_COVALIDATION_PLAN.md`).
- **Showcase S0–S4:** Docker Compose demo stack, Vite dashboard, `chronus-replay` UDP replay
  tool, scripted HIL anomalies, and curated public CCSDS fixtures.

### Changed in the run-up to 0.1.0

- **Ephemerust from crates.io** (`0.7` pin) replaces the sibling-checkout path dependency;
  CV-4 eclipse geometry upgraded to Ephemerust's conical umbra/penumbra model
  (Methodology **D-037**, **D-038**).
- **Rust edition 2024** with the MSRV-aware resolver (v3); MSRV **1.89** verified on the real
  toolchain. Dependency currency pass: axum 0.8, tower 0.5, tower-http 0.7, thiserror 2, toml 1,
  spacepackets 0.18, base64 0.23, criterion 0.8, tokio-tungstenite 0.30 (Methodology **D-039**).
- **Release mechanics:** all three crates pass full `cargo package` with verify builds;
  `chronus-hil-sim` declares `chronus-gateway` with both `path` and `version` for publishing.
