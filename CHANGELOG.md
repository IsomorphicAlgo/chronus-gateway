# Changelog

ChronusGateway-RS follows [Semantic Versioning](https://semver.org/). During `0.x`, the **minor**
version marks breaking changes and the **patch** version marks compatible ones (same policy as
the Ephemerust backend); a `1.0.0` release is reserved for a deliberately committed-stable API.
Versions cover all three workspace crates (`chronus-gateway`, `chronus-hil-sim`,
`chronus-replay`), which share one version via `[workspace.package]`.

## Unreleased

## 0.3.0 — 2026-10-11

API freeze preparation (Tranche 1 of the 1.0 plan, **D-046**): the public shapes that will keep
growing are fixed *before* the 1.0 promise. Contains **breaking** API changes, hence the minor
bump. Operator-facing behaviour and the TOML config format are unchanged, with one exception: the
toy EPS / thermal / body-rate checks (bits 4–6) now need the `hil` cargo feature.

- **`#[non_exhaustive]`** on `ConfigError`, `ConfigLoadError`, `CcsdsError`, `HilTmV1DecodeError`,
  `OemError`, `TrackingError`, `Severity`, `EphemerisSource`, `OemFrame`, `OemTimeSystem`,
  `Listener`, `OverallStatus`, `RfMetadata`, `OpenMctRealtimeMessageV1`, `GatewayMetricsSnapshot`,
  `IngestSnapshot`, `SharedGateway`, `ValidationContext` and the config structs. Build `RfMetadata`
  with `RfMetadata::new()` and `with_carrier_hz` / `with_rx_power_dbm` / `with_pointing_deg`;
  configure `StationConfig` by starting from `Default` and assigning fields.
- **`validate::ValidationContext`** (new) replaces the ten positional arguments of
  `apply_physics_validation`, which is now `(ctx, frame, state, rf)`. `SharedGateway` builds the
  context once from the station config; the HIL payload is decoded inside the validator.
- **Typed `propagator::TrackingError`** (`Config` / `Oem` / `Astro` / `Backend`) replaces
  `anyhow::Error` in `OrbitalPropagator::tracking_state`, `TrackingProvider`, the backend
  constructors, `OemEphemeris::from_file` and `explain_tracking_error`. Custom backends return
  `TrackingError::backend(..)`.
- **`StationConfig` is grouped** into `site`, `tracking`, `tolerances` and `hil`
  (`SiteConfig`, `TrackingConfig`, `ToleranceConfig`, `HilConfig`). `EphemerisSource`
  (`TleInline` / `TleFile` / `OemFile`) replaces `TleSource` and `oem_file`. New
  `ConfigError::NotATleSource` (`CG-128`). The TOML keys are unchanged.
- **`hil` cargo feature (off by default)** gates the CV-4 / CV-5 toy checks (bits 4–6),
  `HilSubsystemCvParams` and the nadir-array Sun-illumination model. Run the gateway with
  `--features hil` to use them with the HIL simulator; `chronus-hil-sim`, `chronus-demo-server`
  and the demo Docker image enable it. Without it, bits 4–6 are never set and
  `TrackingState::nadir_sun_illum_cos` is `NaN`.
- **`#![warn(missing_docs)]`** on the gateway library (all public fields documented) and a
  **`cargo-semver-checks`** CI job against the latest release tag.
- **CI** now also tests and lints the gateway on its own, without `hil`.
- **Tests:** 243 → 253 workspace tests (192 unit, 24 integration, 37 doctests); 216 when the
  gateway is tested without `hil`.

## 0.2.0 — 2026-10-11

Operator-facing diagnostics, sharper physics (Earth orientation, refraction, TLE-age-aware
Doppler), an operator-ephemeris (CCSDS OEM) backend, test-suite expansion, and the move to
**Ephemerust 1.1**. Contains **breaking** API changes (new `ConfigError` variants,
`#[non_exhaustive]` `TrackingState`, new fields on `StationConfig` and the Open MCT message
struct), hence the minor bump.

- **CCSDS OEM ephemeris backend (new `oem` module):** `station.oem_file` tracks from an operator
  ephemeris (orbit determination or GPS) instead of a TLE. KVN OEM per CCSDS 502.0-B-3: multiple
  segments, useable windows, Lagrange interpolation of the stated degree, frames GCRF/ICRF,
  EME2000/J2000, TEME and ITRF*, time systems UTC/TAI/TT/GPS (calendar or day-of-year epochs).
  Reproduces astropy/ERFA Earth-fixed states between samples to ≤ 5.5 cm. `OemPropagator` is the
  second `OrbitalPropagator`; `propagator::propagator_from_station` picks the backend. New
  diagnostics `CG-127`, `CG-320`–`CG-322` (coverage is also checked at startup). **D-045**.
- **Earth orientation:** optional `ut1_minus_utc_s`, `polar_motion_x_arcsec`,
  `polar_motion_y_arcsec` (IERS Bulletin A) feed Ephemerust 1.1's IAU 2006/2000B frame chain for
  both backends; unit mistakes are caught as `CG-119`. **D-043**.
- **Refraction in the pointing check (CV-2):** on by default (`refraction`,
  `refraction_pressure_hpa`, `refraction_temperature_c`). The pointing check compares the
  measured antenna direction with the *apparent* elevation (~0.5° above geometric at the horizon,
  which previously exceeded T-POINT on every rise and set); the horizon check stays geometric.
  New `TrackingState::apparent_elevation_deg` and JSON field `apparent_elevation_deg`; the demo
  sandbox antenna now tracks the apparent direction. **D-042**.
- **TLE-age-aware Doppler tolerance (opt-in):** `doppler_tle_age_margin` (+ `tle_error_km_at_epoch`,
  `tle_error_growth_km_per_day`) widens the Doppler band by the range-rate error the TLE's
  along-track error implies at that point of the pass, `f₀·|ρ̈|·δs/(|v|·c)`; validated within
  10 % against an actual along-track shift. New `TrackingState::range_rate_uncertainty_km_s`,
  `validate::effective_doppler_tolerance_hz`, and JSON field `doppler_tolerance_hz`. **D-044**.
- **`TrackingState` is `#[non_exhaustive]`** with `TrackingState::new` and `with_*` builders, so
  custom backends keep compiling as physics fields are added. **Breaking** for code that built it
  with struct-literal syntax. New public `propagator::StationModel` and `TleErrorModel`.
- **Docs:** rustdoc now builds clean with `-D warnings` (four pre-existing broken intra-doc links
  fixed).
- **Ephemerust 1.1 (corrected Doppler input):** the backend now depends on
  `ephemerust = { version = "1.1", default-features = false }`. Ephemerust 1.0 fixes a range-rate
  error of up to ~0.31 km/s (≈ 450 Hz at 437.5 MHz) that would have tripped the Doppler check
  (bit 0) on real RF; synthetic demo traffic was self-consistent and unaffected. Range rate now
  agrees with an independent IAU 2000 reference to 0.41 m/s. `default-features = false` keeps
  Ephemerust's CLI dependencies out of the gateway build. **Methodology D-041** (amends D-004,
  D-012, D-037).
- **Tolerances:** **T-DOPPLER** stays ±150 Hz with a new justification (TLE age, not library math,
  is now the dominant term); **T-RANGERATE** tightens from 0.25 km/s to **1 m/s** and is enforced
  by the new `propagator` test `range_rate_matches_numerical_derivative_of_range`.
- **TLE age warning tightened for Doppler:** `CG-301` is now a *warning* once a TLE is more than
  **3 days** old (was *info* after 14 days), explaining that the Doppler (bit 0) and pointing
  (bit 3) checks are the ones at risk and that it can be ignored when no measured carrier or
  antenna angles are supplied. `CG-302` (> 30 days) is unchanged. **Breaking:** the public
  constant `diagnose::TLE_AGING_DAYS` (14) is replaced by `diagnose::TLE_DOPPLER_DAYS` (3).

- **New `diagnose` module:** every error type (`ConfigError`, `ConfigLoadError`, `CcsdsError`,
  `HilTmV1DecodeError`, socket bind failures, Ephemerust TLE/propagation errors) can explain
  itself as a `Diagnosis` — a `CG-*` code, severity, what happened, likely cause and numbered
  fixes. Reuses Ephemerust's `TleError::hint()` for TLE problems.
- **Smart catches:** swapped latitude/longitude; tolerance entered as a percentage; carrier
  frequency entered in MHz/kHz; TLE too old for Doppler/pointing (> 3 days) or for any check
  (> 30 days), or future-dated; port in use (suggests the
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
- **Tests:** 68 → 169 gateway unit tests (new coverage for `diagnose`, `http`, `ingest`, `metrics`,
  `state`, ingest validation, `oem`, refraction, Earth orientation and the TLE-age Doppler model),
  plus the `oem_backend` integration suite against astropy/ERFA fixtures; doctests 1 → 32.
  Workspace total: 243.
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
