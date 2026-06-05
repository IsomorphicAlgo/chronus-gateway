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
**CI:** `.github/workflows/ci.yml` always clones **`IsomorphicAlgo/Ephemerust`** (not `github.repository_owner`) into a sibling directory so fork pull requests still resolve `../Ephemerust`; `actions/checkout@v5` avoids deprecated Node 20 runners for the checkout action.

### D-006 — MSRV 1.89 (advisory), no forced toolchain pin yet
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

### D-021 — Subsystem toy co-validation vs Sun proxy (**CV-4**)
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

---

## Open decisions (to resolve as milestones land)

- **OD-E — Multi-node / rack-scale co-simulation.** Backlog beyond the M7 laptop scope.

---

## Attribution
External works this project builds on or is inspired by (keep current; attribute in this table and at point of use):

| Work | Role here | Source / License |
|------|-----------|------------------|
| **Ephemerust** (owner) | SGP4 propagation, look-angles, range-rate, low-precision Sun position (**CV-4** illumination) | local sibling crate, MIT |
| `sgp4` crate | Underlying SGP4/SDP4 numerics (via Ephemerust) | crates.io |
| `spacepackets` (us-irs) | CCSDS Space Packet parsing (M2) | crates.io, Apache-2.0/MIT |
| **Rusty_Server** (owner) | Architectural inspiration (async/Axum/config patterns) | sibling repo |
| Tokio, Axum, Tower, Tower-HTTP, Serde, Chrono, Anyhow, Thiserror, Base64, Futures-util | Runtime + HTTP/WS + serialization | crates.io, MIT/Apache-2.0 |
| `criterion`, `proptest` | Benchmarks + parser robustness property tests (M6) | crates.io, MIT/Apache-2.0 |
| `toml` | Gateway config file parsing (M8) | crates.io, MIT/Apache-2.0 |
| NASA Open MCT | Target mission-control dashboard | open source (NASA) |

---

*Last updated: 2026-06-03.*
