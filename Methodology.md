# Methodology — ChronusGateway-RS

A living record of **why** the project is built the way it is: major decisions, frameworks,
trade-offs, and the reasoning behind them. Append new entries as decisions are made; do not
silently rewrite history (mark superseded entries). Required reading + maintenance per the
contributor expectations in `README.md` (keep this file current when decisions change).

> Status: **M1–M8** complete. **CV-0** charter is documented in
> [`docs/EXTENDED_COVALIDATION_PLAN.md`](docs/EXTENDED_COVALIDATION_PLAN.md) and **D-016**; **Gate CV-0** is approved.
> **Gate CV-2** is approved; **CV-3** (synthetic HIL TM v1 payload + decoder + APID policy) is **implemented** — **Gate CV-3** approved.
> **CV-4** (HIL subsystem vs toy Sun proxy) is **implemented** — **Gate CV-4** approved.
> **CV-5** (HIL ADCS body-rate envelope) is **implemented** — **Gate CV-5** pending owner sign-off.
> **Showcase track (S0–S4):** [`docs/SHOWCASE_PLAN.md`](docs/SHOWCASE_PLAN.md) + [`docs/Demo_Test.md`](docs/Demo_Test.md); **Gate S-0** through **Gate S-4** approved (2026-06-19). **S3:** `chronus-replay`, scripted `chronus-hil-sim`, `demo/replay/`. **S4:** `demo/fixtures/` (ISS + AMSAT).
> **Manual demo path (finalization A.5):** [`docs/DEMO.md`](docs/DEMO.md) ↔ [`docs/Demo_Test.md`](docs/Demo_Test.md) runbook alignment — **D-033**.
> **Secondary testing** (optional pre-release depth): [`TEST_PLAN.md`](TEST_PLAN.md) + **D-029** (`cargo-mutants`, `cargo-hack` when features exist, Miri scope, Loom deferred).
> **Release rehearsal** (`cargo package`): [`TEST_PLAN.md`](TEST_PLAN.md) — **§ Release rehearsal**; **`chronus-replay`** full package verified; gateway/HIL gated on Ephemerust crates.io (**D-005** / **E.2**).
> **Criterion / benches:** [`TEST_PLAN.md`](TEST_PLAN.md) — **§ Performance regression guard**; optional **`bench`** workflow (**D-030**); PR CI stays **`cargo bench --no-run`** only.
> **Cross-target smoke:** [`TEST_PLAN.md`](TEST_PLAN.md) — **§ Cross-target smoke (Linux publish shape)**; reference **`x86_64-unknown-linux-gnu`** via **`ci.yml`** on `ubuntu-latest` (**D-031**).
> **Finalization Tranche A:** secondary testing + release rehearsal + cross-target smoke + manual demo alignment — **complete** (**A.1–A.5**, **D-033**). **Tranche B.1** README narrative — **complete** (**D-034**). **Tranche B.2** acknowledgments audit — **complete** (**D-035**). **Tranche B.3** operator doc split — **complete** (**D-036**). Next: **B.4** ([`PROJECT_FINALIZATION_PLAN.md`](PROJECT_FINALIZATION_PLAN.md)).

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

### D-005 — Dependency source for Ephemerust: local path *(superseded by D-037)*
**Decision:** `ephemerust = { path = "../Ephemerust" }` (sibling checkout next to this repo).
**Why:** Tight local co-development; mirrors the proven approach used in Rusty_Server's
`EPHEMERUST_INTEGRATION_PLAN.md`. If third-party builds ever matter, switch to a pinned git `rev`
or a crates.io version (and update this entry).
**Reproducibility:** `0.x` crate — pin intentionally and bump deliberately on breaking minors.
**CI:** `.github/workflows/ci.yml` always clones **`IsomorphicAlgo/Ephemerust`** (not `github.repository_owner`) into a sibling directory so fork pull requests still resolve `../Ephemerust`; `actions/checkout@v5` tracks current GitHub runner guidance for the checkout action.

### D-006 — MSRV 1.89 (advisory), no forced toolchain pin yet *(edition + resolver updated by D-039)*
**Decision:** Set `rust-version = "1.89"` in `[workspace.package]`; GitHub CI uses the same
`dtolnay/rust-toolchain@stable` pin. Do **not** add a `rust-toolchain.toml` forcing a channel for now.
**Why:** **`nexosim` 1.x** (HIL / `chronus-hil-sim`) pulls **`smol_str` 0.3.6**, which declares
**rustc 1.89** as its minimum — building on 1.88 fails with Cargo’s MSRV check. Ephemerust remains
compatible at this floor. Forcing an exact channel that may not be installed would trigger surprise
downloads/build failures; add a pinned `rust-toolchain.toml` later if CI reproducibility demands it.

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

### D-009 — Ingestion frame type and backpressure (Milestone 1)
**Decision:** `RawFrame.bytes` is an `Arc<[u8]>`; datagrams are fanned out on a **lossy**
`tokio::sync::broadcast` channel; the receive buffer is fixed at `max_datagram_size`; shutdown is
any `impl Future<Output=()>`.
**Why:**
- `Arc<[u8]>` makes the per-subscriber broadcast clone a refcount bump, not a payload copy, while
  avoiding a new `bytes` dependency (revisit `bytes::Bytes` at M2 if the parser benefits).
- A lossy broadcast satisfies the hard requirement that a slow consumer never stalls the socket:
  the oldest frames are dropped and laggards see `RecvError::Lagged`. Telemetry favors freshest
  data over guaranteed delivery of stale frames.
- A fixed buffer means no allocation is driven by attacker-controlled length (security rule 3).
  Oversized datagrams error on Windows (`WSAEMSGSIZE`, counted) and truncate on Unix; the loop
  stays in sync either way.
- A generic `Future` shutdown keeps the lib runtime-agnostic and trivially testable (oneshot in
  tests, `ctrl_c` in `main`) without mandating a particular cancellation crate in the library API.
**Update (M5–M6):** the binary uses `tokio_util::sync::CancellationToken` so Axum graceful shutdown
and the UDP ingest loop stop together; the `ingest::run` contract is still `impl Future<Output=()>`.
**Tested by:** `tests/ingest.rs` (order, shutdown, oversized, backpressure).

### D-010 — CCSDS parsing crate: `spacepackets` (resolves OD-A)
**Decision:** Use **`spacepackets` 0.17** (us-irs) for CCSDS Space Packet parsing, wrapped behind
the `ccsds` module so the rest of the gateway depends on our `TelemetryFrame`, not on the crate.
**Why:** It supports the full primary header plus secondary-header/PUS handling we will need for
real telemetry, is actively maintained, and parses with a clean `from_be_bytes` returning the
header and remaining slice. `space-packet` is Kani-verified but primary-header-only; an in-house
parser would duplicate well-tested work and own the correctness burden (against the project's security
posture). Keeping it behind the module boundary preserves the option to swap later.
**Frame representation:** `TelemetryFrame` retains the original `Arc<[u8]>` datagram and exposes
the packet data field via a zero-copy `payload()` borrow (no `bytes` crate needed — extends D-009).
**Validation:** length → decode → declared-vs-available → TM/TC; recoverable `CcsdsError` per case,
no panics or unbounded allocation on untrusted input.
**Tested by:** inline unit tests in `ccsds.rs` (golden bytes, round-trip, truncation, garbage, routing) plus a `proptest` case that random byte vectors never panic the parser (M6). The public `encode_synthetic_tm` helper is exercised by `chronus-hil-sim` (M7).

### D-011 — Station config + throttled tracking provider (Milestone 3)
**Decision:** A `StationConfig` (observer lat/lon/alt, nominal carrier frequency, `TleSource`,
recompute interval) with `validate()`/`resolve_tle_text()`; `EphemerustPropagator::from_station`;
and a `TrackingProvider` that wraps an `Arc<dyn OrbitalPropagator>` and **caches/throttles**
recomputation to the configured look-angle rate.
**Why:**
- Validation up front (range-checked lat/lon/altitude/frequency, non-empty TLE) turns bad config
  into clear errors rather than downstream `NaN`s — and keeps untrusted file input bounded.
- A throttle (default 10 ms ≈ 100 Hz) avoids redundant SGP4 propagation when many frames share a
  timestamp window; the cache is read under a short `Mutex` and the propagation runs **outside**
  the lock so concurrent clients never serialize on SGP4 work.
- `from_station` keeps TLE-source resolution (inline now, file load; CelesTrak deferred) in config,
  not in the network path.
**Determinism:** locked by a baseline regression test (range/az/el within tolerance of the
foundation smoke run) so propagation changes are caught.
**Tested by:** `config` unit tests (validation, file errors) and `propagator` tests (deterministic
state, counting-mock trait-swap + throttle).

### D-012 — Physics–Telemetry Co-Validation thresholds (Milestone 4; resolves OD-C)
**Decision:** Implement `validate::apply_physics_validation` with:
- **Doppler:** non-relativistic `f_expected = f_nominal − f_nominal × (v_m/s / c)` where
  `v_m/s = range_rate_km_s × 1000` (Ephemerust sign: positive = receding). Compare to optional
  `RfMetadata::measured_carrier_hz`; if `|measured − expected| > doppler_tolerance_hz`, set bit 0.
  Default tolerance **150 Hz** on `StationConfig` (`T-DOPPLER` in `TEST_PLAN.md`).
- **Elevation:** if `elevation_deg < minimum_elevation_deg`, set bit 1. Default threshold **0°**
  (strict: below mathematical horizon is anomalous). Negative thresholds allow a refraction mask.
- **Bit 2 (link budget, CV-1 / D-017):** optional `RfMetadata::measured_rx_power_dbm` (dBm) vs
  free-space \(P_{rx,\mathrm{pred}}\) from `StationConfig` synthetic `tx_power_dbm`, `tx_gain_dbi`,
  `rx_gain_dbi`, slant range, and carrier wavelength; if `|P_{rx,\mathrm{meas}} - P_{rx,\mathrm{pred}}| >`
  `link_budget_tolerance_db` (default **3 dB**, **T-RSSI**), set **`FLAG_LINK_BUDGET_ANOMALY`**
  (same value as legacy **`FLAG_RSSI_RESERVED`**). `None` measured power skips the check.
  **Charter:** bits 3–7 remain per **D-016** / [`docs/EXTENDED_COVALIDATION_PLAN.md`](docs/EXTENDED_COVALIDATION_PLAN.md).
- **`RfMetadata::measured_carrier_hz == None`:** Doppler check skipped (no bit 0); production SDR
  wiring comes with M5 or a side channel.
**Why OD-C is closed:** Ephemerust documents `range_rate_km_s` to ~0.25 km/s vs a 1 s central
difference; at L-band (~437 MHz) that maps to sub-kHz frequency uncertainty from propagation math
alone. The ±150 Hz band is therefore dominated by atmosphere, receiver chain, and clock effects,
not SGP4 truncation at the teaching-grade arcminute level (D-004).
**`TelemetryFrame`:** `raw` and `payload_len` are `pub(crate)` so `validate` unit tests can build
minimal frames without exposing internals on the public API.
**Tested by:** `validate` unit tests (Doppler, horizon, link budget / **T-RSSI**, combined flags,
NaN-safe skips, formula identity); see also **D-017**.

### D-013 — Web distribution stack + Open MCT JSON contract (Milestone 5; resolves OD-B)
**Decision:** Use **Axum** (`axum` 0.7 with `ws`) + `tower-http` tracing for HTTP and WebSocket.
Each downlink frame is one WebSocket **text** JSON object with `chronus_schema: "openmct.realtime.v1"`,
decoded TM fields (`apid`, `seq_count`, `physics_flags`, `received_at`, `source`), optional
look-angle / range fields when a propagator is configured, and `payload_base64` for the CCSDS
packet data field (adapter-friendly for Open MCT plugins or external bridges). Stub routes:
`GET /api/v1/chronus/openmct/dictionary`, `GET /api/v1/chronus/history` (empty list).
**Why:** Matches proven patterns from the owner's **Rusty_Server**; Axum integrates cleanly with
Tokio and the existing `broadcast::Sender<RawFrame>` fan-out. A versioned schema string keeps
clients forward-compatible.
**Metrics (M6):** `GatewayMetrics` + `GET /api/v1/chronus/metrics` (ingest snapshot + gateway counters
+ average processing latency).
**Tested by:** `tests/distribution.rs` (health, WebSocket JSON, second client after peer disconnect).

### D-014 — NeXosim HIL driver (Milestone 7; closes OD-D for single-spacecraft laptop scope)
**Decision:** Add workspace member **`chronus-hil-sim`** using **`nexosim` 1.x** (asynchronics): a
discrete-event `SpacecraftDemo` emits `TelemSample` (synthetic EPS / thermal / ADCS
scalars) on an `Output` port; a `ProtoUdpBridge` builds `UdpDownlinkBridge` with
`ProtoModel` so a `std::net::UdpSocket` lives in non-serialized `BridgeEnv` and sends `encode_synthetic_tm` datagrams (see `crates/gateway/src/ccsds.rs`) to the gateway UDP ingest. Binary `chronus-hil-sim` accepts `HOST:PORT` and frame count for manual
profiling against M6 metrics (`docs/HIL.md`).
**Why OD-D is closed at this scope:** one cooperating model + one I/O bridge matches the “single
simulated spacecraft on the laptop” gate; multi-node / rack-scale co-simulation is explicitly
out of scope until a future decision.
**Why NeXosim:** open-source DES aligned with the README portfolio narrative; MIT OR Apache-2.0
dual license fits the workspace `deny.toml` policy.
### D-015 — File-backed gateway configuration (Milestone 8)
**Decision:** Optional **TOML** file (`toml` 0.8) loaded at process start. Top-level tables `[ingest]`
and `[station]` are optional; omitted tables use the same defaults as pre-M8 binaries. When
`[station]` is present, exactly one of `tle_inline` (string) or `tle_file` (path) is required.
`ingest.bind_addr` and `ingest.http_bind` are parsed as `SocketAddr` strings. Discovery order:
`--config` / `-c` / `--config=` from argv, else `CHRONUS_GATEWAY_CONFIG`, else in-process defaults.
**Why:** Operations need bind addresses and station geometry without rebuilds; TOML is human-editable
and keeps the dependency surface small (serde already in-tree).
**Security:** `deny_unknown_fields` on the root document; bounded file read via `read_to_string` for
config only (TLE files remain subject to `max_datagram_size` on the UDP path, unchanged).
**Tested by:** `config::file` unit tests (parse, merge, ambiguous TLE, bad addr, missing file).

### D-016 — Extended co-validation charter (CV-0; `physics_flags`, `RfMetadata`, tolerances)
**Decision:** Freeze contracts for post-M4 co-validation work (**CV-1…CV-5** in
[`docs/EXTENDED_COVALIDATION_PLAN.md`](docs/EXTENDED_COVALIDATION_PLAN.md)). This entry **supplements**
D-012; it does not change shipped Doppler/elevation behavior. **CV-1** implements bit 2; **CV-2** implements bit 3; **CV-4** implements bits 4–5; **CV-5** implements bit 6 per this charter.

**`physics_flags` (u8) — bit assignment**

| Bit | Mask | Semantics | Milestone |
|-----|------|-----------|-----------|
| 0 | `0x01` | Doppler anomaly (`FLAG_DOPPLER_ANOMALY`) | M4 (shipped) |
| 1 | `0x02` | Below minimum elevation (`FLAG_BELOW_HORIZON`) | M4 (shipped) |
| 2 | `0x04` | Link budget: measured received power vs **free-space** prediction; anomaly if \(\|P_{rx,\mathrm{meas}} - P_{rx,\mathrm{pred}}\| >\) **T-RSSI** | CV-1 (**shipped**) |
| 3 | `0x08` | Pointing: great-circle separation between measured and computed (az, el) \(>\) **T-POINT** | CV-2 (**shipped**) |
| 4 | `0x10` | EPS: decoded **abstract bus voltage (V)** vs toy linear map from Sun illumination + decoded TM (`FLAG_EPS_SUBSYSTEM_ANOMALY`) | CV-4 (**shipped**) |
| 5 | `0x20` | Thermal: decoded **panel °C** vs toy band from same illumination proxy (`FLAG_THERMAL_SUBSYSTEM_ANOMALY`) | CV-4 (**shipped**) |
| 6 | `0x40` | ADCS: HIL v1 \|`body_rate_deg_s`\| exceeds **T-BODYRATE** (`FLAG_ADCS_BODY_RATE_ANOMALY`) | CV-5 (**shipped**) |
| 7 | `0x80` | **Reserved** — do not assign without updating this table and `TEST_PLAN.md` | — |

If more than eight independent alarms are needed, add a **new** JSON field (e.g. `physics_flags_v2: u16`)
alongside the existing `physics_flags` for one release cycle; do **not** repurpose bits 6–7 silently.

**Measurement routing**

- **Ground / receiver chain** (SDR metadata, AGC-derived power if calibrated to a synthetic dBm
  contract, servo or encoder azimuth/elevation): optional fields on **`RfMetadata`** (sidecar to the
  UDP datagram path; same pattern as `measured_carrier_hz` today).
- **Spacecraft-reported** engineering scalars (battery temperature, array current, attitude
  quaternions for co-validation): decoded from the **CCSDS packet data field** using a **versioned
  synthetic layout** for HIL/tests — **`chronus.hil.tm.v1`** in the `hil_tm` module (**CV-3**, **D-020**); production spacecraft would need an
  explicitly documented mapping per mission — out of scope for the open generic gateway until
  declared.

**Explicitly out of scope for CV-1–CV-4 v1** (defer unless a future decision reopens)

- Ionospheric / tropospheric absorption, rain fade, multipath, polarization and pointing loss
  beyond the free-space + T-RSSI band.
- Full ECSS PUS / timecode (CUC/CDS) parsing for arbitrary missions.
- SPICE-grade ephemeris or body-fixed attitude from ops products; CV-4 uses **toy** sun geometry and
  synthetic TM only.
- Absolute calibration of real hardware RSSI to dBm (project stays on **synthetic** numeric contracts).

**Why:** Unblocks implementation without thrashing Open MCT JSON or the stable bitfield; keeps ITAR/EAR
posture (no real mission parameters) while matching the design paper’s roadmap in controlled slices.

### D-017 — Free-space link budget co-validation (CV-1)
**Decision:** Implement `validate::free_space_path_loss_db`, `validate::expected_rx_power_dbm`, and extend
`apply_physics_validation` with `Option<LinkBudgetStationParams>` (in `validate`) built from `StationConfig`
(`tx_power_dbm`, `tx_gain_dbi`, `rx_gain_dbi`, `link_budget_tolerance_db`; synthetic defaults). Set bit 2
when `RfMetadata::measured_rx_power_dbm` is `Some` and outside **T-RSSI** (see `TEST_PLAN.md`).
**Why:** Delivers the chartered CV-1 slice without atmosphere or cable models (v1); keeps the hot path
bounded and NaN-safe.
**Tested by:** `validate` link-budget unit tests and `config` validation for new station fields.

### D-018 — Antenna pointing residual co-validation (CV-2)
**Decision:** Extend `RfMetadata` with optional `measured_azimuth_deg` / `measured_elevation_deg`; add
`validate::angular_separation_deg` (ENU unit vectors, great-circle angle). `apply_physics_validation`
takes `pointing_tolerance_deg` (**T-POINT**, default **0.25°** from `StationConfig`); when both
measured angles are `Some` and finite, set bit 3 (`FLAG_POINTING_ANOMALY`) if separation **strictly exceeds**
the tolerance. Skip when either angle is missing, non-finite, or tolerance is not finite and positive.
**Why:** Encoder vs computed boresight check from the design roadmap without SPICE-grade attitude;
matches `TrackingState` azimuth (clockwise from north) / elevation (above horizon) convention.
**Tested by:** `validate` unit tests (`angular_separation_*`, pointing in/out of band, skip paths) and
`config` validation for `pointing_tolerance_deg`.

### D-019 — `cargo-deny` exceptions for transitive unmaintained advisories
**Decision:** List **RUSTSEC-2025-0141** (`bincode` 2.x) and **RUSTSEC-2024-0436** (`paste`) in
`deny.toml` `[advisories].ignore` with recorded reasons. Both are **unmaintained** (not vulnerability)
reports on transitive dependencies: **`nexosim`** → `bincode`, **`spacepackets`** 0.17 → `paste`.
**Why:** `cargo deny check` is a CI gate (M6); failing the build on advisories we cannot resolve without
forking or dropping HIL / CCSDS stacks would block all merges. Revisit when `nexosim` or `spacepackets`
publishes releases that remove these crates; remove ignores and re-run `cargo deny check`.

### D-020 — Synthetic HIL TM payload contract + decode (**CV-3** / `chronus.hil.tm.v1`)
**Decision:** Add `hil_tm` with fixed **24-byte** big-endian layout (magic **`CHI1`**, version byte,
zeroed reserved bytes, then `seq` + three `f32` demo scalars). `decode_hil_tm_v1` returns
`DecodedHilTmV1` or `HilTmV1DecodeError` with **no heap allocation** on the decode path.
`StationConfig` gains inclusive `hil_tm_v1_apid_min` / `hil_tm_v1_apid_max` (defaults **0x7B0…0x7BF**)
and `apid_allows_hil_tm_v1`. `chronus-hil-sim` emits this layout via `encode_hil_tm_v1_payload`.
**Why:** Fulfils CV-3 charter: bounded, versioned binary contract in the CCSDS data field so CV-4
subsystem checks do not reinterpret arbitrary bytes. APID band documents the synthetic lane vs
arbitrary TM.
**Tested by:** `hil_tm` unit tests (truncation, magic, version, reserved, round-trip) + `config`
validation + `chronus-hil-sim` integration decode on the ingest path.

### D-021 — Subsystem toy co-validation vs Sun proxy (**CV-4**) *(eclipse part amended by D-038)*
**Decision:** Extend `TrackingState` with `nadir_sun_illum_cos` ∈ \([0,1]\) ∪ \{NaN\}, computed in
`propagator` from SGP4 TEME position (via Ephemerust `propagate`) and the crate’s low-precision geocentric Sun direction
(`celestial::calculate_position` for `CelestialObject::Sun` — equator-of-date, **not** SPICE fidelity).
Toy nadir-fixed illumination: `max(0, −û_sat·û_sun)` with a **spherical WGS84 equatorial** ray–sphere test to zero the factor in Earth occultation. Expected HIL `eps_bus_voltage_v` and `thermal_panel_c` are linear in that factor using tunable `StationConfig` endpoints; **T-EPS** is enforced as ±10 % of the configured voltage span, **T-THERMAL** as ±10 K (`FLAG_EPS_SUBSYSTEM_ANOMALY`, `FLAG_THERMAL_SUBSYSTEM_ANOMALY`). WebSocket distribution decodes **chronus.hil.tm.v1** when the APID is in the HIL band and passes decoded values into `apply_physics_validation`. `chronus-hil-sim` recomputes the same factor and linear maps so synthetic passes stay self-consistent.
**Why:** Implements the CV-4 extension charter as a bounded, NaN-safe demo without flight hardware semantics.
**Tested by:** `propagator::nadir_sun_illumination_cos_is_deterministic`, `validate::hil_cv4_*`, `config::rejects_invalid_hil_cv4_tolerance`, existing HIL ingest tests.

### D-022 — HIL ADCS body-rate envelope (**CV-5**)
**Decision:** When **chronus.hil.tm.v1** is decoded on an allowed APID, compare \|`body_rate_deg_s`\| to a finite positive ceiling **`hil_body_rate_max_abs_deg_s`** on `StationConfig` (default **5 deg/s**, synthetic demo). Anomaly sets **`physics_flags` bit 6** (`FLAG_ADCS_BODY_RATE_ANOMALY`). Skip when the ceiling is non-finite or non-positive, or when the reported rate is non-finite — no propagator cross-check in v1 (not a gyro calibration claim).
**Why:** Uses the existing third HIL scalar for a minimal ADCS sanity flag without expanding the v1 payload; keeps the check independent of the Sun proxy (**CV-4**).
**Tested by:** `validate::hil_cv5_*`, `config::rejects_invalid_hil_cv4_tolerance` (includes invalid body-rate ceiling).

### D-023 — Operator user guide (`docs/USER_GUIDE.md`)
**Decision:** Maintain a **user-facing** guide separate from `README.md` (contributor/onboarding
focus) and from `BUILD_PLAN.md` / `TEST_PLAN.md` (stage-gate and QA contracts). The guide opens with
a **plain-language** introduction: UDP datagram as one telemetry frame, **CCSDS header as envelope**
vs **data field as letter**, split between **payload bytes**, optional **`RfMetadata`** (ground
measurements), and **orbit/station-derived physics**; **Starlink** only as a **public mental model**
for “dense proprietary TM,” explicitly **not** what Chronus decodes; **synthetic `chronus.hil.tm.v1`**
called out as a bounded demo. Subsequent sections are to track the same plan files so documentation
does not fork from tested behavior.
**Why:** Gives integrators and mission-style readers a single entry point without reading the crate
graph first; keeps ITAR/EAR posture clear (synthetic examples, public TLEs).
**Credit:** Guide tone and analogies authored for this repository (AI-assisted draft; owner review).

**Update (2026-06-05):** `docs/USER_GUIDE.md` expanded with **First run** (defaults, TOML, two-terminal
HIL smoke, health/metrics URLs) and **`physics_flags`** (bit table in operator language, `RfMetadata`
skip behavior, pointer to **T-\*** register).

### D-024 — Showcase & demo roadmap (`docs/SHOWCASE_PLAN.md`, `docs/Demo_Test.md`)
**Decision:** Track **demo/delivery** work (Docker/Compose spine, Open MCT or SPA dashboard, replay,
optional curated public fixtures) in **`docs/SHOWCASE_PLAN.md`** with **owner-gated** stages **S0–S4**
(same “do not chain milestones” discipline as `BUILD_PLAN` / `EXTENDED_COVALIDATION_PLAN`). Manual and
semi-automated acceptance lives in **`docs/Demo_Test.md`**. High-level checkboxes and counts hook into
**`TEST_PLAN.md`** under **Showcase tracks** — **not** mixed into M/CV automated test sections beyond
references.
**Why:** Separates **product correctness** (`cargo test`) from **showcase readiness** (reproducible
operator path, visuals, compliance evidence) without diluting physics gates.
**Compliance:** Synthetic-first demos; external fixtures only with provenance and owner sign-off per
**`AGENTS.md`**.

### D-025 — crates.io package vs showcase materials
**Decision:** Treat **crates.io** as shipping **library/binary source only** inside each member’s
**crate root** (`crates/gateway/`, `crates/chronus-hil-sim/`, **`crates/chronus-replay/`**). Showcase and booth assets (Compose,
Open MCT bridge, SPA, large fixtures) live at **workspace root** (e.g. `demo/`) or in a **separate
download** (GitHub Release zip or sibling repo) — never required inside the published tarball. All three
publishable crates set **`[package] exclude = ["demo", "showcase"]`** so those directory names are
dropped if mistakenly created under the crate folder.
**Why:** `cargo publish` only packages the crate directory; keeping demos out of `crates/*` avoids
bloat, accidental IP drift, and confusion for dependents who only need the gateway API/binary.
**Companion:** [`docs/SHOWCASE_PLAN.md`](docs/SHOWCASE_PLAN.md) → *Crates.io vs showcase distribution*.

**Addendum (Showcase S1 — Docker):** [`demo/Dockerfile`](demo/Dockerfile) originally cloned
**IsomorphicAlgo/Ephemerust** at image build time to satisfy the D-005 sibling path. Since **D-037**
the image build resolves `ephemerust` from **crates.io** like every other dependency, pinned by the
committed `Cargo.lock` — the clone step and its `git` install are gone.

### D-026 — Showcase S2 demo dashboard (Vite + TypeScript)
**Decision:** Ship **Track B** first as [`demo/dashboard/`](demo/dashboard/) (Vite + TS) consuming the existing
`openmct.realtime.v1` WebSocket JSON. **Track A** (full NASA Open MCT wiring) stays a documented backlog under
[`demo/openmct/README.md`](demo/openmct/README.md).
**Why:** Gives a zero–Open-MCT-clone demo surface for portfolio and CI; keeps Node tooling isolated under `demo/`
per **D-025**. CI uses **Node 22 LTS** and runs `npm install && npm run build` to guard the bundle (Node **20** reached EOL **2026-04-30**; use a supported LTS for security fixes).

### D-027 — Showcase S3 UDP replay (`chronus-replay`)
**Decision:** Add workspace member **`chronus-replay`** — a small Tokio CLI that reads **synthetic** UDP payloads from a text fixture (**hex lines** or **JSONL** with `udp_hex`) and sends them to the gateway ingest socket, with **`--delay-ms`** pacing and **`--repeat`** for deterministic loops.
**Why:** Satisfies **SHOWCASE_PLAN** S3 “narrative polish” without capturing real RF: portfolio demos and screenshots can replay **the same bytes** every time. Keeps tooling in Rust next to `chronus-hil-sim`; fixtures live under **`demo/replay/`** (not inside publishable crate roots per **D-025**).
**Compliance:** Fixtures must be lab-generated only (`AGENTS.md`); no operational dumps.
**Dependencies:** **`clap`** (derive) added to `[workspace.dependencies]` for argv parsing.

### D-028 — Showcase S3 scripted HIL anomalies (`chronus-hil-sim`)
**Decision:** Extend **`chronus-hil-sim`** with optional **`HilScriptedAnomaly`** (kind + start frame + duration) applied inside [`SpacecraftDemo`](crates/chronus-hil-sim/src/lib.rs) before UDP send. CLI: **`--scripted-anomaly`** (`eps-voltage` \| `thermal` \| `body-rate`), **`--anomaly-after-frame`**, **`--anomaly-frame-count`**, plus **`--apid`**. Library entrypoint **`run_nexosim_udp_hil_with_script`**. Uses **`clap`** in the HIL binary (workspace dep).
**Why:** Delivers the SHOWCASE S3 “scripted fault” path using **only synthetic CV-4/CV-5** scalars already on the wire — no RF capture, no gateway fork — so operators get repeatable **`physics_flags`** bit **4–6** footage with one command.
**Limits:** Does not set Doppler / RSSI / pointing bits (**0–3**); those still require measured RF fields in [`RfMetadata`](crates/gateway/src/validate.rs) if we extend the ingest path later.

### D-029 — Secondary testing charter (mutation, feature matrix, Miri, Loom)
**Decision:** Charter **optional** depth checks in [`TEST_PLAN.md`](TEST_PLAN.md) under **Secondary testing plan**: [`cargo-mutants`](https://mutants.rs/), [`cargo-hack`](https://github.com/taiki-e/cargo-hack) when `[features]` exist, **`cargo miri`** on a scoped subset (library tests first), and **Loom** only if bespoke atomics/lock-free code appears. **`cargo test`** remains the **primary** stage-gate; secondary tools are **pre-release / periodic** until explicitly promoted to required CI.
**Why:** Strengthens confidence before crates.io and public discussion without inflating every PR’s latency or flaking on environment-specific Miri/mutants runs.
**CI:** Optional `workflow_dispatch` / scheduled jobs are listed in `TEST_PLAN.md`; promotion to required checks needs a new Methodology note.

### D-030 — Performance regression guard (Criterion baselines + optional bench workflow)
**Decision:** Document **local** Criterion **baseline save/compare** in [`TEST_PLAN.md`](TEST_PLAN.md) under **Performance regression guard (Criterion)**; keep PR CI at **`cargo bench --no-run`** (compile-only) per existing [`ci.yml`](.github/workflows/ci.yml). Add optional [`.github/workflows/bench.yml`](.github/workflows/bench.yml) on **`workflow_dispatch`** that runs **`cargo bench -p chronus-gateway`** and uploads the **HTML** report from **`target/criterion/report`** as an artifact.
**Why:** Gives the owner a repeatable **pre-release** comparison on a **reference machine** without turning noisy full benches into a merge blocker. Shared runners are unsuitable for strict regression thresholds.
**Credit:** **Criterion** ([user guide / book](https://bheisler.github.io/criterion.rs/book/criterion_rs.html)) — same crate as M6 benches.

### D-031 — Cross-target smoke (`x86_64-unknown-linux-gnu` publish shape)
**Decision:** Treat **`x86_64-unknown-linux-gnu`** on GitHub Actions **`ubuntu-latest`** as the **reference publish shape** for pre-crates.io verification. Document commands and pass criteria in [`TEST_PLAN.md`](TEST_PLAN.md) under **Cross-target smoke (Linux publish shape)**. The existing **`test`** job in [`.github/workflows/ci.yml`](.github/workflows/ci.yml) (`cargo test`, clippy, `cargo bench --no-run`, audit/deny, demo Compose + dashboard build) is the **canonical** recorded Linux build; optional local reproduction via **WSL2** uses the same triple without the Windows-only **`rust-lld`** linker override (**D-008**).
**Why:** Downstream consumers and crates.io CI overwhelmingly build on Linux; the owner’s MSVC **`link.exe`** policy is host-specific and must not be mistaken for publish requirements. One documented non-Windows target satisfies finalization plan **A.4** without adding a second mandatory workflow.
**Exit:** Green **`ci.yml`** `test` on `ubuntu-latest` before first publish; no extra artifact beyond CI run history.

### D-032 — Showcase S4 curated fixtures (ISS + AMSAT CCSDS tracks)
**Decision:** Add **`demo/fixtures/`** with two **owner-gated** tracks — **ISS** and **AMSAT** — each providing a **clean** CCSDS TM hex line plus a **robustness** companion (truncated, TC-on-TM-path, short, garbage) derived locally. Bytes follow **CCSDS 133.0-B** Space Packet layout (public standard + `spacepackets` educational cross-check); they are **not** operational ISS/AMSAT RF captures (ISS amateur APRS is AX.25; FUNcube uses custom FEC). Provenance, SHA-256, and compliance rows live in [`demo/fixtures/README.md`](demo/fixtures/README.md). Automated gate: [`crates/gateway/tests/s4_fixtures.rs`](crates/gateway/tests/s4_fixtures.rs); operator path: [`docs/DEMO.md`](docs/DEMO.md) Path E.
**Why:** Closes showcase **S4** with credible amateur-space **narrative** while staying inside AGENTS.md synthetic/open-standards posture; robustness pairs demo parser resilience without NeXosim.
**Credit:** CCSDS public Blue Books; [`spacepackets`](https://github.com/us-irs/spacepackets); [gr-satellites CCSDS README](https://github.com/daniestevez/gr-satellites) (GPL-3.0, pedagogical APID reference); AMSAT-UK CCSDS outreach pages.
**Gate S-4:** Owner approval **2026-06-19**.

### D-033 — Manual demo path maintenance (`DEMO.md` ↔ `Demo_Test.md`)
**Decision:** Record a **cross-walk table** in [`docs/Demo_Test.md`](docs/Demo_Test.md) mapping **`DEMO.md`** Paths **A–E** to showcase gates **S1–S4** and matching acceptance sections. Any change to demo commands, ports, or paths must update **both** files before a showcase or finalization gate closes.
**Why:** Satisfies finalization plan **A.5** — operators and gate reviewers use one runbook (`DEMO.md`) and one checklist (`Demo_Test.md`) that cannot drift.
**Exit:** Tranche **A** complete after **Gate S-4** and alignment table land.

### D-034 — README intro narrative (finalization Tranche B.1)
**Decision:** Lead [`README.md`](README.md) with plain-language **problem → three-step story** (fast CCSDS→JSON translator, Ephemerust “space map,” physics co-validation with Doppler and illumination-style checks). Move milestone/status density into a **Current status** table below the narrative. Align `chronus-gateway` `[package] description` in [`crates/gateway/Cargo.toml`](crates/gateway/Cargo.toml) with the one-line pitch for crates.io.
**Why:** GitHub and crates.io first impressions should explain *why* before *how*; technical accuracy (flags vs RF block, synthetic demo posture) preserved per `AGENTS.md`.
**Credit:** Narrative adapted from owner draft; Rusty_Server + Ephemerust attribution unchanged.

### D-035 — Acknowledgments audit (finalization Tranche B.2)
**Decision:** Restructure [`README.md`](README.md) § Acknowledgements into maintainer projects, first-class crates (with [crates.io](https://crates.io) links), Ephemerust-transitive deps, standards, dev/CI/demo tooling, and design-analysis-only crates. Expand [`Methodology.md`](Methodology.md) § Attribution to cover **every** workspace dependency (including **`nexosim`**, **`tokio-util`**, **`tracing-subscriber`**, **`tokio-tungstenite`**, **`tempfile`**) plus CCSDS/Open MCT and S4 pedagogical cites. Point [`crates/gateway/src/lib.rs`](crates/gateway/src/lib.rs) crate docs at README + Methodology. Fix broken `sgp4` markdown; correct NeXosim from “planned” to **M7 shipped**; Ephemerust GitHub URL casing.
**Why:** Satisfies **B.2** — no orphan dependency without documented rationale; crates.io-first links for publishable deps.
**Maintenance:** Re-run this cross-check when adding a workspace dependency or promoting a dev-tool to required CI.

### D-036 — Operator doc split (finalization Tranche B.3)
**Decision:** [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md) is the **canonical operator surface** (install, first run, TOML, **`physics_flags`**). [`README.md`](README.md) carries the narrative and **one sentence** after **In short** pointing operators to the user guide; repeatable demo steps stay in [`docs/DEMO.md`](docs/DEMO.md) only. README **Current status** table labels USER_GUIDE as canonical — no duplicated first-run prose in README.
**Why:** Satisfies **B.3** — visitors get “why” on GitHub/crates.io first page, “how to run” in one maintained doc.
**Maintenance:** Change operator steps or alarm text in **USER_GUIDE** first; README links only.

### D-037 — Dependency source for Ephemerust: crates.io (supersedes D-005)
**Decision:** `ephemerust = "0.7"` from **crates.io** replaces the `path = "../Ephemerust"` sibling
checkout. CI (`ci.yml`, `bench.yml`) drops the "Checkout Ephemerust (upstream sibling)" steps —
`cargo` resolves the published crate like any other dependency.
**Why:** Ephemerust **0.6.0+** is published by the same maintainer, so the D-005 rationale (tight
local co-development before publication) no longer applies. A published pin simplifies CI, makes
third-party builds work with a plain `git clone && cargo test`, and is the honest showcase story —
the gateway consumes its maintainer's published crate. Local co-development remains possible at any
time with a temporary `[patch.crates-io]` entry.
**Reproducibility:** still a `0.x` crate: the `0.7` pin accepts compatible patches only (Cargo
treats 0.x minors as breaking); bump the minor deliberately and record it here.

### D-038 — CV-4 eclipse upgraded to conical umbra/penumbra (amends D-021)
**Decision:** `nadir_sun_illumination_cos` now uses **Ephemerust 0.7.0's `eclipse` module** —
`sun_vector_km` (geocentric Sun position with distance) + `shadow_state_from_vectors` (conical
apparent-disk-overlap test, Vallado §5.3) — instead of the in-house RA/Dec direction + ray–sphere
occultation toy, which is deleted. Mapping: **Sunlit** → full nadir-cosine factor, **Penumbra** →
factor × 0.5 (first-order partial solar disk; LEO crossings last seconds), **Umbra** → 0. The
nadir-fixed panel-cosine model and the D-021 linear voltage/thermal maps, tolerances, and flag bits
are unchanged.
**Why:** Upgrades the shadow check from proxy to physics with *less* gateway code, and keeps the
gateway and `chronus-hil-sim` self-consistent automatically — the HIL simulator calls the same
gateway function, so both sides of CV-4 moved together.
**Tested by:** existing `propagator::nadir_sun_illumination_cos_is_deterministic` and
`validate::hil_cv4_*` suites (no tolerance changes needed); Ephemerust's own eclipse unit,
integration, and doctest coverage backs the geometry.

### D-039 — Rust edition 2024 + MSRV-aware resolver + dependency currency pass
**Decision:** Migrate the workspace from **edition 2021** to **edition 2024** (matching
Ephemerust) and switch `[workspace] resolver` from `"2"` to `"3"` (MSRV-aware — Cargo selects
dependency versions compatible with `rust-version`). **MSRV stays 1.89** (D-006 rationale
unchanged; already above Ephemerust's 1.88 floor and edition 2024's 1.85 minimum), so the CI
toolchain pins, the Dockerfile base image, and consumer expectations are untouched. In the same
pass, bring dependencies to current majors: **axum 0.8** (`ws::Message::Text` now carries
`Utf8Bytes`), **tower 0.5**, **tower-http 0.7**, **base64 0.23**, **thiserror 2**, **toml 1**,
**spacepackets 0.18**, and dev-deps **criterion 0.8** (`std::hint::black_box` replaces the
deprecated re-export) and **tokio-tungstenite 0.30**, plus a full `cargo update` lockfile refresh.
**Why:** Keeps the gateway on the same edition as its astrodynamics backend and on maintained
dependency lines ahead of first publish; edition 2024's let-chains replaced four nested-`if`
pyramids in `validate`/`config` with flat `if let … && …` chains (clippy `collapsible_if` fixes),
and resolver v3 makes the declared MSRV an enforced contract instead of a hope.
**Migration evidence:** `cargo fix --edition` produced **zero** source changes (code was already
edition-2024 clean); rustfmt style edition 2024 applied. Verified green on stable (1.97.1):
`cargo test --workspace`, `cargo clippy --all-targets -- -D warnings`, `cargo bench --no-run`;
MSRV floor proven with `cargo +1.89 check --workspace --all-targets` on the real 1.89 toolchain.

### D-040 — Tranche R mission-readiness review: findings + accepted exceptions
**Decision:** Tranche R (Rust-strengths audit, `PROJECT_FINALIZATION_PLAN.md`) executed 2026-07-29.
Deliverable: [`docs/RUST_MISSION_READY.md`](docs/RUST_MISSION_READY.md) — the strength → code map
plus findings **F-1…F-5** (all hardening/observability polish, no design flaws; fixes scheduled
into Tranche C). Measured facts recorded here so later claims stay honest:
**zero `unsafe`** in all three crates; hot-path panic surface limited to two invariant-backed
sites (F-1 mutex-poison `expect`, F-5 constructor-guaranteed slice index); the single `Mutex`
(`TrackingProvider` throttle cache) holds only `Copy` reads/writes and computes SGP4 outside the
lock — **accepted** as a justified lock pending F-1 poison-tolerance; Criterion baseline
`chronus-0.1.x-2026-07-29` saved (`parse_telemetry` ≈ 17 ns, `apply_physics_validation` ≈ 15 ns).
**Why:** The review gate exists to convert "Rust is mission-ready" from marketing into an
auditable inventory with dispositions — same honesty bar as the tolerance register.
**Gate:** Owner sign-off pending on findings + Tranche C scheduling.

---

## Open decisions (to resolve as milestones land)

- **OD-E — Multi-node / rack-scale co-simulation.** Backlog beyond the M7 laptop scope.

---

## Attribution

External works this project builds on or is inspired by. **Keep this table aligned** with
`[workspace.dependencies]` in the root `Cargo.toml`, `README.md` § Acknowledgements, and crate
docs when dependencies change (finalization **B.2**, **D-035**).

| Work | Role here | Source / License |
|------|-----------|------------------|
| **Ephemerust** (owner) | SGP4 propagation, look-angles, range-rate, Sun vector + conical umbra/penumbra eclipse model (**CV-4** illumination; **D-038**) | [crates.io `ephemerust`](https://crates.io/crates/ephemerust) `0.7` pin (**D-037**), MIT ([GitHub](https://github.com/IsomorphicAlgo/Ephemerust)) |
| **Rusty_Server** (owner) | Architectural inspiration (async/Axum/config patterns) | Maintainer sibling project; **D-002** |
| [`sgp4`](https://crates.io/crates/sgp4) | SGP4/SDP4 numerics (via Ephemerust) | crates.io, MIT/Apache-2.0 |
| [`spacepackets`](https://crates.io/crates/spacepackets) ([us-irs](https://github.com/us-irs/spacepackets)) | CCSDS Space Packet parsing (**M2**, **D-010**) | crates.io, Apache-2.0/MIT |
| [`nexosim`](https://crates.io/crates/nexosim) ([asynchronics](https://github.com/asynchronics/nexosim)) | HIL discrete-event sim (`chronus-hil-sim`, **M7**) | crates.io, MIT OR Apache-2.0 |
| [`tokio`](https://crates.io/crates/tokio), [`tokio-util`](https://crates.io/crates/tokio-util) | Async runtime, UDP ingest, graceful shutdown | crates.io, MIT |
| [`axum`](https://crates.io/crates/axum), [`tower`](https://crates.io/crates/tower), [`tower-http`](https://crates.io/crates/tower-http) | HTTP + WebSocket (**M5**, **D-013**) | crates.io, MIT |
| [`tracing`](https://crates.io/crates/tracing), [`tracing-subscriber`](https://crates.io/crates/tracing-subscriber) | Structured logging | crates.io, MIT |
| [`serde`](https://crates.io/crates/serde), [`serde_json`](https://crates.io/crates/serde_json), [`chrono`](https://crates.io/crates/chrono) | Serialization + timestamps | crates.io, MIT/Apache-2.0 |
| [`toml`](https://crates.io/crates/toml) | Gateway config file parsing (**M8**) | crates.io, MIT/Apache-2.0 |
| [`anyhow`](https://crates.io/crates/anyhow), [`thiserror`](https://crates.io/crates/thiserror) | Error handling | crates.io, MIT/Apache-2.0 |
| [`clap`](https://crates.io/crates/clap) | CLI (`chronus-replay`, `chronus-hil-sim`; **S3**, **D-028**) | crates.io, MIT/Apache-2.0 |
| [`base64`](https://crates.io/crates/base64), [`futures-util`](https://crates.io/crates/futures-util) | WebSocket JSON encoding | crates.io, MIT/Apache-2.0 |
| [`criterion`](https://crates.io/crates/criterion), [`proptest`](https://crates.io/crates/proptest) | Benchmarks + parser property tests (**M6**, **D-030**) | crates.io, MIT/Apache-2.0 |
| [`tokio-tungstenite`](https://crates.io/crates/tokio-tungstenite), [`tempfile`](https://crates.io/crates/tempfile) | Dev-only: WS integration tests; TOML config tests | crates.io, MIT |
| [`cargo-audit`](https://crates.io/crates/cargo-audit), [`cargo-deny`](https://crates.io/crates/cargo-deny) | CI supply-chain gates | crates.io |
| Vite | Demo dashboard bundler (Showcase **S2**) | [vitejs.dev](https://vitejs.dev/), MIT |
| [`cargo-mutants`](https://mutants.rs/), [`cargo-hack`](https://github.com/taiki-e/cargo-hack) | Optional secondary testing (**D-029**) | MIT |
| **CCSDS** Blue Books | Wire formats and protocols | [public.ccsds.org](https://public.ccsds.org/) |
| **NASA Open MCT** | Distribution UX reference | [nasa.github.io/openmct](https://nasa.github.io/openmct/), Apache-2.0 |
| `sat-rs`, `nyx-space` | Design analysis only (not dependencies) | GitHub; see **D-001** |
| [gr-satellites](https://github.com/daniestevez/gr-satellites) | Pedagogical CCSDS reference (**S4** fixtures) | GPL-3.0 (cited, not vendored) |

---

*Last updated: 2026-06-19.*
