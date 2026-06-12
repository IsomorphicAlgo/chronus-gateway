# ChronusGateway-RS — Iterative Test Plan

The testing companion to [`docs/BUILD_PLAN.md`](docs/BUILD_PLAN.md). It encodes the **Ephemerust testing standard**
required by the project's stage-gate protocol: layered tests, documented physics tolerances, deterministic and
offline, enforced at every stage gate.

**Showcase & demos (post-M8):** iterative, **owner-gated** stages **S0–S4** live in [`docs/SHOWCASE_PLAN.md`](docs/SHOWCASE_PLAN.md); manual acceptance steps in [`docs/Demo_Test.md`](docs/Demo_Test.md). Gates are listed under [Showcase tracks](#showcase-tracks) below.

---

## Testing philosophy (mirrors Ephemerust)

1. **Layered coverage**
  - **Unit** — inline `#[cfg(test)] mod tests` in each module (happy path + edge/error cases).
  - **Integration** — `tests/*.rs`, async via `#[tokio::test]`; exercise the real pipeline over
  **loopback UDP** and **in-process Axum/WebSocket** (no live hardware).
  - **Doctests** — runnable, asserting examples on public API items.
  - **Physics co-validation** — computed results checked against references or numerical
  cross-checks, with **every tolerance written down and justified**.
  - **Robustness/security** — malformed/truncated/oversized inputs fail gracefully (no panic, no
  unbounded allocation).
2. **Deterministic & offline** — no live SDR/network, no wall-clock dependence. Use synthetic
  CCSDS frames, **fixed timestamps**, and public reference TLEs (ISS).
3. **Green before gate** — `cargo test` (lib + integration + doctests) must pass before a
  milestone advances. Keep the **status/counts table** below current.
4. **Tolerances are documented** — see the Tolerance Register; mirror Ephemerust's style
  (e.g. its 0.25 km/s central-difference and 0.05 km WGS84-vs-WGS72 bounds).

**Commands**

```
cargo test               # unit + integration + doctests
cargo test --lib         # fast unit-only loop
cargo test --test <name> # a specific integration suite
cargo clippy --all-targets
```

---

## Secondary testing plan (release / depth; optional for stage gates)

The layers above — especially **`cargo test`** — remain the **primary** quality gate for milestone
and showcase stage gates. This section charters **additional** checks that catch weak assertions,
feature-interaction bugs, and undefined behavior in dependencies, without blocking every PR unless
the owner promotes a given tool to **required**.

**Principles**

1. **Offline & deterministic** where possible (same rule as primary tests). Mutation and Miri runs
   may be slower; treat them as **pre-release** or **weekly** cadence unless CI adds optional jobs.
2. **Workspace context:** `ephemerust` is a **path** dependency (`../Ephemerust`). Run secondary
   commands from the **workspace root** with both repos checked out, matching CI and local dev.
3. **CI posture:** keep these as **optional** (`workflow_dispatch` or scheduled) jobs until the
   owner decides latency and flake budget allow promotion to required checks — see
   [Optional CI follow-ups](#optional-ci-follow-ups-secondary) below.

### Mutation testing (`cargo-mutants`)

**Goal:** Prove tests actually fail when implementation code is perturbed (finds “tests that never
assert behavior”).

**Tool:** [`cargo-mutants`](https://mutants.rs/) (`cargo install cargo-mutants`).

**Suggested invocations** (from workspace root; adjust timeout if the machine is slow):

```text
cargo mutants --no-shuffle -p chronus-gateway
cargo mutants --no-shuffle -p chronus-hil-sim
cargo mutants --no-shuffle -p chronus-replay
```

- Start with **`chronus-gateway`** — largest surface (parse, validate, config).
- If mutants escape the default timeout, add `--timeout-multiplier 2` (or higher) per upstream docs.
- **Baseline / triage:** capture `mutants.out` / logs for any **unviable** or **caught** mutants and
  either add a targeted test or document an explicit exclusion with rationale (same honesty bar as
  tolerance rows).

### Feature-matrix builds (`cargo-hack`)

**Goal:** When a crate exposes **`[features]`**, ensure every feature combination at least **compiles**
and tests that are feature-gated still pass.

**Tool:** [`cargo-hack`](https://github.com/taiki-e/cargo-hack) (`cargo install cargo-hack`).

**Current workspace baseline (2026-06):** workspace members **`chronus-gateway`**, **`chronus-hil-sim`**,
and **`chronus-replay`** do **not** define optional Cargo features. Until `[features]` are added,
the matrix check collapses to the primary gate:

```text
cargo test --workspace
```

**When features land:** run, from the workspace root:

```text
cargo hack test --workspace --each-feature --exclude-no-default-features
```

Revisit this subsection when the first `[features]` table appears in any member `Cargo.toml`.

### Undefined-behavior hygiene (`cargo miri`)

**Goal:** Exercise the **safe** Rust under Miri to catch UB in the crate’s own code and in
`unsafe` inside dependencies (subject to Miri’s model).

**Tool:** Rustup component: `rustup component add miri` then `cargo miri setup`.

**Project-owned `unsafe`:** none in `crates/*/src` at charter time — any `unsafe` lives in
dependencies (Tokio, etc.).

**Suggested scope:** prefer **library** tests first (lighter than full multi-threaded integration):

```text
cargo miri test -p chronus-gateway --lib
```

**Platform notes:** Miri on **Windows hosts** is often slower or more constrained than on **Linux**
or **WSL2**; if local Miri is impractical, run the same command in CI on `ubuntu-latest` or in WSL.
Full **`#[tokio::test]`** integration suites may require Miri flags or may be **out of scope** for
Miri until the owner narrows a supported subset — document failures instead of silencing them.

### Concurrency model-checking (`loom`)

**Goal:** Exhaust small-state models of **custom** lock-free or atomics-heavy code.

**Charter for this repository:** concurrent structure relies on **Tokio** channels, broadcast, and
the async runtime rather than bespoke lock-free queues. **Loom is not required** until the codebase
introduces custom `std::sync::atomic` or hand-rolled synchronization worth modeling. If that
changes, add a subsection here with the exact `loom` test harness and invariants.

### Optional CI follow-ups (secondary)

Track (do not chain gates): add **non-required** GitHub Actions workflows or jobs, for example:

| Job | Trigger | Notes |
| --- | --- | --- |
| `mutants` | `workflow_dispatch` + optional `schedule` | Long-running; cache `target/` where safe. |
| `miri` | `workflow_dispatch` or weekly cron | `ubuntu-latest`; `-p chronus-gateway --lib` first. |
| `hack` | on `Cargo.toml` / member manifest changes | No-op useful until `[features]` exist; then `--each-feature`. |
| `bench` | `workflow_dispatch` only | Criterion run for `chronus-gateway`; uploads HTML report artifact (`.github/workflows/bench.yml`). **Not** a PR gate — see [Performance regression guard (Criterion)](#performance-regression-guard-criterion). |

Promotion to **required** checks is an owner decision recorded in `Methodology.md`.

### Release rehearsal (`cargo package`)

**Goal:** Before `cargo publish`, confirm each crate’s **tarball contents** match **D-025** (no
`demo/` or `showcase/` trees inside `crates/*`) and that **verify** builds succeed where the
dependency graph is fully resolvable from **crates.io**.

**D-025 alignment:** `chronus-gateway`, `chronus-hil-sim`, and `chronus-replay` each declare
`exclude = ["demo", "showcase"]` in their `[package]` section (defensive if those folder names are
ever created under the crate root by mistake).

**1 — Inspect the packaged file list (always; no registry resolve):**

```text
cargo package -p chronus-gateway --list
cargo package -p chronus-hil-sim --list
cargo package -p chronus-replay --list
```

**Pass:** no path containing `demo/` or `showcase/` appears in the listing. (The workspace root
`demo/` directory is **never** under these crate roots and must not be pulled in via mistaken
`include` patterns.)

**2 — Full package + verify (when the graph allows):**

```text
cargo package -p chronus-replay
cargo package -p chronus-gateway
cargo package -p chronus-hil-sim
```

Use **`--allow-dirty`** only when rehearsing with **uncommitted** manifest changes (CI should run
on clean trees without this flag).

**`chronus-replay`:** Has no path dependency on **Ephemerust**; full `cargo package` is expected to
**pass** on a clean tree once the index is reachable.

**`chronus-gateway` / `chronus-hil-sim`:** Today **`ephemerust`** is a **path-only** sibling
(`../Ephemerust`, **D-005**). Cargo then either (a) errors that a **version** must be specified for
packaging/publish, or (b) after adding `version = "…"` next to `path`, errors that **`ephemerust`**
is missing from **crates.io** until **E.2** is executed. That is **expected** until Ephemerust is
published and this workspace pins it for the registry. Until then, rely on **§1** above plus
`cargo test --workspace` for integration confidence; treat a **green** `cargo package` on these
two crates as a **release-day** check after the Ephemerust story is closed.

**3 — LICENSE / README in the tarball:** `cargo package --list` should be reviewed for a **`LICENSE`**
(or `LICENSE-MIT`) file if crates.io policy requires an explicit file in the crate root; today the
workspace declares `license = "MIT"` in `[workspace.package]` — confirm the published crate layout
against crates.io requirements before the first upload (**finalization plan D.2**).

### Performance regression guard (Criterion)

**Goal:** Catch accidental slowdowns in the **CCSDS parse** and **`apply_physics_validation`** hot
paths before a release, using the same **Criterion** harness as Milestone 6
([`crates/gateway/benches/parse_validate.rs`](crates/gateway/benches/parse_validate.rs)).

**Routine commands** (workspace root; sibling **`../Ephemerust`** present):

```text
cargo bench -p chronus-gateway --no-run    # compile benches only (matches required CI)
cargo bench -p chronus-gateway             # run all gateway benches (Criterion)
cargo bench -p chronus-gateway --bench parse_validate
```

**Saving and comparing baselines (reference machine)**

Criterion forwards arguments after `--` to the benchmark binary:

1. On a **quiet**, **idle** machine (fixed power profile if on a laptop), same **`rustc`** / MSRV as
   CI, run once and **save** a named baseline (pick a stable name, e.g. include date or release tag):

   ```text
   cargo bench -p chronus-gateway --bench parse_validate -- --save-baseline chronus-2026-06-12
   ```

2. After code changes on the **same** machine, **compare**:

   ```text
   cargo bench -p chronus-gateway --bench parse_validate -- --baseline chronus-2026-06-12
   ```

3. **Read the output:** Criterion reports typical timing, noise, and **regression / improvement**
   estimates with confidence. Investigate large regressions or record an intentional change in
   `Methodology.md` (same bar as tolerance register updates).

**Where baselines live:** under `target/criterion/` (already under `/target`, git-ignored). A
`cargo clean` removes them — **re-record** a saved baseline after a clean if comparisons should
continue. Do **not** commit raw `target/` artifacts; if the owner ever checks in golden numbers, do
so via a small documented note (hash + machine class), not the whole Criterion tree.

**CI vs local baselines:** GitHub-hosted runners are **noisy** and differ CPU-to-CPU; treat optional
**`bench`** workflow results as **smoke / artifact capture**, not as a substitute for a
**reference-machine** baseline comparison before release.

---

## Shared fixtures

- **Reference TLE:** public ISS (ZARYA) 3-line set (same family Ephemerust tests use).
- **Synthetic CCSDS frames:** helper builders producing valid + deliberately-malformed packets
(truncated header, bad length, wrong packet type, oversized payload).
- **Fixed instants:** evaluate near the TLE epoch so SGP4 stays in its accurate window.
- **Mock propagator:** a deterministic `OrbitalPropagator` returning scripted `TrackingState`s
for validation-engine tests (decouples M4 from astrodynamics).

---

## Per-milestone test gates

### M0 — Foundation

- Binary smoke run produces a finite `TrackingState` (az/el/range/range-rate) from the ISS TLE.
- Doctest on `EphemerustPropagator::new` showing construction + a bounded assertion.
- Unit tests: finite tracking state near epoch; invalid TLE rejected.

### M1 — Ingestion

- **Integration:** bind to an ephemeral loopback port, send `N` datagrams, assert `N` frames
observed on the channel in order (`receives_all_datagrams_in_order`).
- **Shutdown:** cancellation stops the loop promptly with no leaked task/panic
(`shutdown_stops_loop_promptly`).
- **Security:** oversized datagram handled safely; loop keeps delivering valid frames; buffer
fixed at `max_datagram_size` (`oversized_datagram_does_not_break_loop`).
- **Backpressure:** a slow subscriber observes `Lagged` while the socket loop receives all
datagrams (`lagging_subscriber_never_blocks_socket`).

### M2 — CCSDS parsing

- **Unit:** parse a golden primary header → correct type/APID/seq/length
(`parses_valid_tm_packet`, `parses_known_golden_bytes`).
- **Round-trip:** canonical bytes → parse equals original fields, incl. min/max APID & seq
(`round_trip_preserves_fields`).
- **Robustness:** short header, truncated payload, and all-`0xFF` garbage return structured
errors, never a panic (`short_datagram_is_rejected`, `truncated_payload_is_rejected`,
`garbage_does_not_panic`).
- **Routing:** TM accepted; TC rejected with `NotTelemetry` (`telecommand_is_rejected`).

### M3 — Propagator integration

- **Unit:** config validation (valid default; invalid lat/lon/freq/altitude; empty inline TLE;
missing TLE file; **HIL TM v1** APID range) with specific errors (`default_station_is_valid`, `rejects_out_of_range_fields`,
`resolves_inline_tle_and_rejects_empty`, `missing_tle_file_is_reported`, `apid_allows_hil_tm_v1_respects_range`).
- **Deterministic:** fixed TLE + fixed instant → stable `TrackingState`, baseline-locked within
tolerance (`from_station_is_deterministic_and_in_tolerance`).
- **Trait swap + throttle:** a counting mock propagator proves the seam and the
recompute-throttle cache (`provider_uses_mock_and_throttles_recompute`).

### M4 — Co-validation

- **Doppler in-band:** measured carrier within **T-DOPPLER ±150 Hz** of `expected_carrier_hz` →
no bit 0 (`doppler_in_band_no_flag_t_doppler_150hz`).
- **Doppler out-of-band:** deviation beyond bound → bit 0 (`doppler_out_of_band_sets_bit0`).
- **Look-angle / elevation:** predicted elevation strictly below `minimum_elevation_deg` → bit 1
(`below_horizon_sets_bit1`); at-threshold edge (`elevation_at_horizon_not_flagged_when_minimum_is_zero`).
- **Bitfield:** independent anomalies set independent bits; clean frame = `0`
(`combined_anomalies_set_both_bits`, `independent_bits_doppler_only`, `no_measured_carrier_skips_doppler_even_if_would_be_bad`).
- **Non-finite RF:** NaN measured carrier skips Doppler without panic (`nan_measured_skips_doppler_no_panic`).
- **Formula:** non-relativistic Doppler identity locked by unit test (`expected_carrier_matches_non_relativistic_formula`).

### M5 — Distribution

- **End-to-end:** in-process `ingest → parse → validate → WebSocket`; a connected client
receives well-formed Open MCT JSON including `physics_flags`.
- **Health:** `GET /health` returns `200`.
- **Lifecycle:** client disconnect is handled without affecting other clients or the loop.

### M6 — Hardening

- **Benchmarks:** `criterion` parse + validate hot paths (`cargo bench -p chronus-gateway`). Baseline save/compare before releases: [Performance regression guard (Criterion)](#performance-regression-guard-criterion).
- **Fuzz/property:** randomized byte streams never panic the parser (`ccsds` proptest).
- **Supply chain:** `cargo audit` / `cargo deny` in CI (`deny.toml`).

### M7 — HIL simulation

- **Smoke:** NeXosim sim → gateway delivers validated frames to a client (`chronus-hil-sim` +
real `ingest::run` on loopback).
- **Soak:** sustained simulated-rate run (400 frames) with `recv_errors == 0` and full parse.

### M8 — File configuration

- **TOML:** valid file → merged `IngestConfig` + `StationConfig`; startup runs `validate()` and
TLE resolution (inline or readable file).
- **Errors:** ambiguous/missing TLE keys, bad `SocketAddr`, unknown top-level keys, missing TLE
file → structured `ConfigLoadError` / `ConfigError` (unit tests in `config/file.rs`).

### CV-0 — Extended co-validation charter (documentation only)

Charter lives in `**docs/EXTENDED_COVALIDATION_PLAN.md`** and `**Methodology.md` D-016**. No code
behavior change beyond documenting contracts.

- `**physics_flags` bit map** — bits 2–5 per CV-1 / CV-2 / CV-4; bit 6 (**CV-5**); bit 7 reserved;
JSON evolution policy if >8 alarms (`physics_flags_v2`).
- **RfMetadata vs payload** — ground-chain measurements on `RfMetadata`; spacecraft scalars via
versioned synthetic CCSDS payload decode after **CV-3**.
- **Tolerance register** — **T-RSSI**, **T-POINT**, **T-EPS**, **T-THERMAL**, **T-BODYRATE** rows below (charter
values; rebaseline when models land in CV-1 / CV-2 / CV-4 / CV-5).
- **Explicit deferrals** — listed under D-016 (atmosphere, full PUS, SPICE-grade ephemeris,
uncalibrated hardware RSSI).

**Gate CV-0:** `[x]` **Owner approval** obtained — **CV-1** implementation may proceed (see extension plan).

### CV-1 — Link budget (free-space, **T-RSSI**)

- **Unit:** `free_space_path_loss_matches_manual`; in-band / out-of-band vs **T-RSSI**
(`link_budget_in_band_no_flag_t_rssi`, `link_budget_out_of_band_sets_bit2`).
- **Skip paths:** no measured Rx; NaN measured; zero range — no bit 2
(`no_measured_rx_skips_link_budget_even_if_would_be_bad`, `nan_measured_rx_skips_link_no_panic`,
`zero_range_skips_link_budget_no_flag`).
- **Config:** invalid `link_budget_tolerance_db` and non-finite `tx_power_dbm` rejected
(`rejects_out_of_range_fields`).

### CV-2 — Pointing residual vs **T-POINT** (great-circle, bit 3)

- **Unit:** `angular_separation_same_direction_near_zero`; `angular_separation_orthogonal_ninety_deg`.
- **In band / out of band:** measured (az, el) within **T-POINT** of computed → no bit 3 (`pointing_within_t_point_no_bit3`); separation strictly greater than tolerance → bit 3 (`pointing_exceeds_t_point_sets_bit3`).
- **Skip paths:** only one of az/el `Some` → no bit 3 (`pointing_only_azimuth_skips_no_bit3`); `pointing_tolerance_deg` not finite / not positive → pointing skipped (`non_finite_pointing_tolerance_skips_pointing`).
- **Config:** `StationConfig::pointing_tolerance_deg` default **0.25°**; optional TOML `station.pointing_tolerance_deg` (`gateway.example.toml`); invalid `pointing_tolerance_deg` rejected (`rejects_out_of_range_fields`).

### CV-3 — Synthetic HIL TM v1 payload (`chronus.hil.tm.v1`)

- **Unit (`hil_tm`):** `golden_encode_then_decode_round_trip`; `truncated_payload_rejected`; `empty_payload_rejected`; `wrong_magic_rejected`; `wrong_version_rejected`; `non_zero_reserved_rejected`; `longer_slice_decodes_first_24_only`.
- **Config:** default APID band **0x7B0…0x7BF**; invalid range rejected; `apid_allows_hil_tm_v1_respects_range`.
- **Integration:** `chronus-hil-sim` HIL ingest smoke/soak decode v1 payloads on synthetic APIDs (`hil_ingest.rs`).

### CV-4 — HIL subsystem vs toy Sun proxy (bits 4–5)

- **Propagator:** `nadir_sun_illumination_cos_is_deterministic`; `TrackingState::nadir_sun_illum_cos` populated by `EphemerustPropagator`.
- **Validate:** `hil_cv4_voltage_and_thermal_in_band`, `hil_cv4_bad_voltage_sets_bit4`, `hil_cv4_bad_thermal_sets_bit5`, `hil_cv4_skips_when_illum_non_finite`.
- **Config:** CV-4 voltage/temperature endpoints + tolerances on `StationConfig`; invalid EPS relative tolerance / thermal tolerance rejected (`rejects_invalid_hil_cv4_tolerance`).
- **Distribution:** WebSocket path decodes HIL v1 on allowed APIDs and passes `HilSubsystemCvParams` from station (**D-021**).
- **HIL sim:** `chronus-hil-sim` uses Ephemerust + gateway `nadir_sun_illumination_cos` with the same linear maps as default station endpoints.

### CV-5 — HIL ADCS body-rate envelope (bit 6, **T-BODYRATE**)

- **Validate:** `hil_cv5_body_rate_within_ceiling_no_bit6`, `hil_cv5_body_rate_exceeds_ceiling_sets_bit6`, `hil_cv5_skips_when_body_rate_non_finite`, `hil_cv5_skips_when_max_not_positive`.
- **Config:** `hil_body_rate_max_abs_deg_s` on `StationConfig` (default **5** deg/s); invalid ceiling in `rejects_invalid_hil_cv4_tolerance`; optional TOML `station.hil_body_rate_max_abs_deg_s`.
- **Distribution:** same WebSocket HIL decode path as CV-4 (**D-022**).

---

## Showcase tracks

Companion roadmap: [`docs/SHOWCASE_PLAN.md`](docs/SHOWCASE_PLAN.md).

Automated `cargo test` remains the **primary** software quality gate for gateway crates. Stages **S0–S4** add
**demo / delivery** acceptance. Detailed procedures: [`docs/Demo_Test.md`](docs/Demo_Test.md). **Do not chain**
stages — obtain **owner Gate S-*** approval between tranches (same governance as `BUILD_PLAN` / CV milestones).

### S0 — Showcase charter

- [`docs/SHOWCASE_PLAN.md`](docs/SHOWCASE_PLAN.md) and [`docs/Demo_Test.md`](docs/Demo_Test.md) committed;
  [`README.md`](README.md) lists both; compliance expectations acknowledged per `Demo_Test.md` global rules.

**Gate S-0:** `[x]` **Owner approval** (2026-06-04) — **S1** implementation may proceed.

### S1 — Demo spine

- Documented stack — [`docs/DEMO.md`](docs/DEMO.md) + [`demo/README.md`](demo/README.md) + [`demo/docker-compose.yml`](demo/docker-compose.yml); native `cargo run` two-terminal flow and Docker Compose path.
- `GET /health` → **200**; WebSocket `GET /telemetry/openmct` delivers **≥ 1** valid `openmct.realtime.v1`
JSON message; `GET /api/v1/chronus/metrics` → **200** with finite fields after ingest (manual / `Demo_Test.md` §S1).
- CI validates Compose file: `docker compose -f demo/docker-compose.yml config --quiet` (`.github/workflows/ci.yml`).

**Gate S-1:** `[x]` **Owner approval** — **S2** may proceed.

### S2 — Dashboard v1

- [x] **Track B:** [`demo/dashboard/`](demo/dashboard/) Vite + TypeScript app; `physics_flags` badges + latest az/el/range/range-rate; documented in [`docs/DEMO.md`](docs/DEMO.md) Path C and [`demo/dashboard/README.md`](demo/dashboard/README.md).
- [x] **Track A backlog:** [`demo/openmct/README.md`](demo/openmct/README.md) describes Open MCT adapter scope.

**Gate S-2:** `[x]` **Owner approval** — **S3** may proceed.

### S3 — Narrative polish

- [x] **Replay:** `chronus-replay` binary (`crates/chronus-replay/`) sends synthetic UDP datagrams from **hex lines** or **JSONL** (`udp_hex`); `--delay-ms`, **`--repeat`**; fixtures under [`demo/replay/fixtures/`](../demo/replay/fixtures/); runbooks [`demo/replay/README.md`](../demo/replay/README.md) and [`docs/DEMO.md` → Path D](docs/DEMO.md#path-d--udp-replay-showcase-s3).
- [x] **Scripted HIL:** `chronus-hil-sim --scripted-anomaly {eps-voltage,thermal,body-rate}` with `--anomaly-after-frame` / `--anomaly-frame-count` (CV-4/CV-5 bits **4–6**); [`docs/HIL.md`](HIL.md).

**Gate S-3:** `[x]` **Owner approval** (2026-06-04) — **S4** optional; owner holding S4 for now.

### S4 — Optional public fixtures

- Each external fixture documented in `demo/fixtures/README.md` (source, license, date); owner compliance
sign-off (`Demo_Test.md` §S4).

**Gate S-4:** `[ ]` **Owner approval** — fixture tranche closed.

---

## Tolerance Register (justify every number)

Populate as engines land; keep rationale next to the value (Ephemerust style).


| ID          | Quantity                                                                        | Tolerance                                                  | Rationale / source                                                                                                                                                                                                                                     |
| ----------- | ------------------------------------------------------------------------------- | ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| T-DOPPLER   | Carrier Δf deviation                                                            | ±150 Hz                                                    | **Locked (M4 / OD-C).** PDF atmospheric/ionospheric drift band; Ephemerust `range_rate_km_s` is validated to ~0.25 km/s vs central difference — at 437.5 MHz that is sub-kHz from propagation math, so ±150 Hz is conservative for physics-only error. |
| T-ELEVATION | Minimum elevation for valid TM                                                  | Configurable (`minimum_elevation_deg`, default **0°**)     | Flag when `elevation_deg < threshold` (strict inequality). Default: at or above mathematical horizon passes; use negative threshold for refraction margin.                                                                                             |
| T-RANGERATE | Range-rate vs numerical                                                         | 0.25 km/s                                                  | Matches Ephemerust's central-difference check (reused convention).                                                                                                                                                                                     |
| T-RSSI      |                                                                                 | P_{rx,\mathrm{meas}} - P_{rx,\mathrm{pred}}                | on **free-space** link budget                                                                                                                                                                                                                          |
| T-POINT     | Great-circle angular separation between measured (Az,El) and computed boresight | **0.25°**                                                  | **Charter (CV-0 / D-016).** Design-paper encoder vs computed residual; implementation in CV-2 uses spherical geometry. Revisit if station mount flex or refraction dominates in a given demo.                                                          |
| T-EPS       | Decoded HIL bus voltage vs linear map from toy Sun illumination                 | **±10 %** of configured voltage span (default **24–28 V**) | **Locked (CV-4 / D-021).** Proxy for “array current” in the CV-0 charter: the v1 payload carries **abstract bus voltage (V)**; tolerance applies to                                                                                                    |
| T-THERMAL   | Decoded HIL panel °C vs the same illumination linear map                        | **±10 K**                                                  | **Locked (CV-4 / D-021).** Not flight thermal analysis; `chronus-hil-sim` emits self-consistent demo values.                                                                                                                                           |
| T-BODYRATE  | Decoded HIL                                                                     | `body_rate_deg_s`                                          | vs configured ceiling                                                                                                                                                                                                                                  |


---

## Status / counts (keep current)


| Layer                | Count | Notes                                                                                                                 |
| -------------------- | ----- | --------------------------------------------------------------------------------------------------------------------- |
| Unit tests           | 68    | `chronus-gateway` lib (63) + `chronus-replay` (3) + `chronus-hil-sim` lib (2). |
| Integration tests    | 10    | `crates/gateway/tests/*.rs` (7) + `crates/chronus-hil-sim/tests/hil_ingest.rs` (3).                                   |
| Doctests             | 1     | `EphemerustPropagator::new`.                                                                                          |
| Showcase gates S0–S4 | 4 / 5 | **S0–S3** gates approved; **S4** on hold. [`docs/SHOWCASE_PLAN.md`](docs/SHOWCASE_PLAN.md); [`docs/Demo_Test.md`](docs/Demo_Test.md). |


*Last updated: 2026-06-13.*