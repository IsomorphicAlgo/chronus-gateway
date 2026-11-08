# Changelog

ChronusGateway-RS follows [Semantic Versioning](https://semver.org/). Versions cover the
published workspace crates (`chronus-gateway`, `chronus-hil-sim`, `chronus-replay`), which share
one version via `[workspace.package]`.

## Stability policy for 1.x

From `1.0.0` on, a **major** version is required to break any of the following; everything else
can change in a minor or patch release.

| Covered by the 1.x promise | How it is kept |
|---|---|
| **Library API** of `chronus-gateway`: the `ccsds`, `config`, `diagnose`, `freshness`, `frames`, `history`, `http`, `ingest`, `metrics`, `oem`, `openmct`, `pipeline`, `propagator`, `rf`, `security`, `state`, `time` and `validate` modules and the crate-root re-exports | `cargo-semver-checks` in CI against the last release tag; types that will grow are `#[non_exhaustive]` with constructors (`Methodology.md` D-046, D-058) |
| **Wire formats:** `openmct.realtime.v1`, `chronus.rf.v1`, `chronus.history.v1`, `chronus.openmct.dictionary.v1` (published JSON Schemas) and the `physics_flags` / `physics_flags_v2` bit assignments | New fields are additive; a field is never removed or repurposed; a reserved bit is assigned only with a schema update and a tolerance-register row (`docs/TRACEABILITY.md`) |
| **HTTP routes** under `/api/v1/`, `/telemetry/openmct`, `/metrics` and `/health` | New routes and fields only; `/api/v2/` if a route must change |
| **Configuration file** keys and their meaning, and the CLI flags | New keys get defaults that keep today's behaviour; a renamed key keeps its old name as an alias for all of 1.x |
| **Diagnostic codes** (`CG-1xx` startup, `CG-5xx` runtime) | A code keeps its meaning; new codes are added |

**Not covered:** anything behind the `hil` feature (lab models), `chronus-hil-sim` internals, the
demo crates (`chronus-demo-server`, `chronus-evidence`, never published), log message text, the
exact wording of diagnostics, Prometheus metric names other than those listed in
`docs/DEPLOYMENT.md`, and the minimum supported Rust version (raised only in a minor release, with
a CHANGELOG note). Default tolerances may be tuned in a minor release when evidence justifies it,
with the change and the evidence stated here.

During `0.x` (up to 0.6.0) the **minor** version marked breaking changes.

## Unreleased

## 1.0.0-rc.1 — 2026-11-07

Evidence and release candidate (Tranche 5 of the 1.0 plan, **D-054** to **D-058**). The API is
frozen from here: if the rc runs a week on the public demo without an API change, it becomes
`1.0.0` unchanged.

**Compatibility**
- **Library API: one deliberate break**, the last before 1.0 (D-058). The structs D-046 left
  open are now `#[non_exhaustive]`: `IngestConfig`, `RawFrame`, `TelemetryFrame`,
  `LinkBudgetStationParams`, `StationModel`, `OemSegment`, `Diagnosis`, `HealthReport`,
  `HealthInputs`, `IngestStats` and `GatewayMetrics`. Build them with the new constructors
  (`RawFrame::new`, `LinkBudgetStationParams::new`, `HealthInputs::new` + `with_addrs`,
  `StationModel::with_earth_orientation` / `with_refraction`) or from `Default` and assign
  fields. `TleErrorModel` and `DecodedHilTmV1` stay plain: they are complete by definition (a
  linear model; a fixed v1 wire layout).
- **Everything else is additive:** `config::load_gateway_from_str`;
  `prometheus::resident_memory_bytes` and the `process_resident_memory_bytes` gauge on `/metrics`
  (Linux).
- Wire formats, routes, configuration and CLI are unchanged.

**Fixed**
- `chronus.rf.v1` numbers with long decimal mantissas were parsed up to one ULP off, so a sample
  re-encoded by a relay could differ from the one received. Found by the new `rf_datagram` fuzz
  target: `serde_json` now has `float_roundtrip` enabled for every build. It was previously on
  only in test builds, through a dev-dependency.

**Fuzzing** (`fuzz/`, D-054)
- Eight `cargo-fuzz` targets cover every parser of untrusted input: the CCSDS packet parser and
  time codes, the TM frame deframer, the whole packet pipeline, `chronus.hil.tm.v1`,
  `chronus.rf.v1` and its store, OEM text, IERS finals and the TOML loader. Each asserts
  properties, not just "no panic" (round trips, bounds, declared lengths).
- Seeds are generated from the repository's fixtures (`fuzz/make_seeds.py`).
- `.github/workflows/fuzz.yml`: a 30 s smoke run of each target on pull requests that touch the
  parsers, a nightly run per target that resumes from a cached corpus, and manual runs of up to
  5.8 h. The ledger toward 24 h per target is in `docs/FUZZING.md`.

**Soak and load** (`chronus-evidence` crate, `chronus-soak`, D-055)
- A load driver: UDP probe packets at a fixed rate, N WebSocket clients, end-to-end latency
  percentiles from a log-linear histogram, loss, and the memory trend with a least-squares
  growth rate. Pass/fail thresholds for CI or the release host.
- At the design load (10 000 pkt/s, 50 clients) for 10 minutes on a 2-vCPU VM: 300 million
  messages with zero loss; p99 13 ms end to end; memory flat at 24 MiB. The 24 h run on the
  release host is pending. Results: `docs/SOAK.md`.

**Real-RF Doppler validation** (`chronus-doppler`, `tools/rf/`, D-056)
- `tools/rf/iq_carrier.py` turns an SDR recording (raw IQ, SigMF or gqrx files) into a
  `chronus.rf.v1` carrier track. `chronus-doppler` predicts each sample with the gateway's own
  propagator and judges the residuals against T-DOPPLER. First it removes the receiver's
  oscillator offset and drift, which it reports in ppm, and it checks a shape gain that no offset
  can fake.
- A synthetic-IQ rehearsal of the whole chain runs in CI. The procedure for a real pass is in
  `docs/RF_VALIDATION.md`; the owner's own capture is pending.

**Traceability** (D-057)
- `docs/TRACEABILITY.md` is the in-repository tolerance register (13 rows: every alarm bit, the
  time checks, the model accuracies) and maps 18 requirements to 91 named tests.
  `tools/check_traceability.py` fails CI when a referenced test disappears, an alarm bit has no row,
  or the code uses an undefined tolerance ID.

## 0.6.0 — 2026-10-24

Security and operations (Tranche 4 of the 1.0 plan, **D-051**, **D-052**, **D-053**).

**Compatibility**
- **Library API:** additive against 0.5.0 (`cargo semver-checks` passes as a minor release).
- **One operator-visible change:** serving the API on a non-loopback address with no API token
  now **refuses to start** (`CG-137`). Add a token, or set
  `security.allow_unauthenticated_remote = true` if a proxy in front of the gateway already
  authenticates.
- **Everything else is opt-in.** Defaults stay loopback-only with no token.

**Access control** (new `security` module, `[security]` table)
- **API tokens.** Bearer tokens on every route except `GET /health`; browser WebSockets can use
  `?access_token=`. Tokens come from `api_tokens_file` or `CHRONUS_API_TOKENS`, must be at least
  32 characters, are compared in constant time, and are never logged.
- **WebSocket limits.** An `Origin` check (pages from this computer, plus `allowed_origins`) and a
  client cap (`max_ws_clients`, default 64).
- **UDP allow-lists.** `udp_allowed_sources` / `rf_allowed_sources` (CIDR) for the telemetry and
  receiver ports, through the new `ingest::run_filtered` and `rf::run_listener_filtered`.
- **TLS** is left to a reverse proxy; Caddy and nginx configurations are in
  `docs/DEPLOYMENT.md`.

**Data freshness** (new `freshness` module, `[freshness]` table)
- **Reload while running.** `tle_file`, `oem_file` and the new `eop_file` are reloaded when they
  change. The propagator is swapped without dropping frames (`TrackingProvider::replace`), and a
  broken file keeps the old data and is reported.
- **Earth orientation from the IERS.** `eop_file` is an IERS `finals2000A` file: fixed-width
  (`finals.daily`) or the IERS data centre's CSV (`finals2000A.daily.csv`). Today's UT1−UTC and
  polar motion are interpolated from it and re-applied every UTC day. It is checked against a real
  IERS file (`demo/fixtures/iers/`).
- **Optional fetching.** The `refresh` cargo feature (off by default; it pulls in an HTTPS stack)
  lets the gateway fetch `tle_url` / `eop_url` itself: at most every 2 h, no retries, downloads
  validated before they replace a file. Without it, `deploy/fetch-data.sh` and a systemd timer do
  the same.
- **Data age everywhere.** `/status` and `/api/v1/chronus/metrics` gain a `data` object: TLE
  epoch and age, OEM coverage, Earth-orientation source, values and coverage, reload and fetch
  results.

**Operations**
- **A real command line** (`clap`): `--help`, `--version`, `--config`, `--check-config` (dry run:
  exit 0 ready, 2 must fix), `--generate-token`, `--log-format text|json`.
- **Graceful stop.** SIGTERM now stops the gateway cleanly, so `docker stop` and `systemctl stop`
  flush the history file.
- **`GET /metrics`** in the Prometheus text format: every counter, `chronus_health_status`, and
  the data ages in seconds.
- **Packaging** in `deploy/`:
  - a distroless non-root container image that refuses to run without a token;
  - a hardened systemd unit (`systemd-analyze security` 1.1, "OK") with a sysusers file, a
    fetch timer and a production config;
  - `.github/workflows/release.yml`, which builds Linux, Windows and macOS archives with SHA-256
    sums, creates a GitHub Release, and pushes the image to GHCR.
- **CI** gains the `refresh` feature tests and clippy run, `shellcheck`, and an image build.
  `deny.toml` now checks all features.
- **Demos:** `demo/openmct` reads `CHRONUS_TOKEN` and its proxy adds the token. The Docker demo
  publishes on loopback and opts out of the bind policy.

**Diagnostics**
- **Startup:** `CG-137` (unauthenticated network bind), `CG-138` (unusable tokens), `CG-139`
  (unusable Earth orientation file), `CG-140` (fetch URLs in a build without `refresh`),
  `CG-141` (`allowed_origins = ["*"]`), `CG-142` (`--check-config` found errors).
- **Runtime:** `CG-515` (refused tokens, origins, clients or senders), `CG-516` (reload or fetch
  failed), `CG-517` (Earth orientation file ran out). The TLE-age and OEM-coverage checks now run
  live, not only at startup.

**Docs:** new `docs/DEPLOYMENT.md` (the hardened-deployment page).

**Tests:** 376 → 413 workspace tests (315 unit, 43 integration, 55 doctests); 355 for the gateway
without `hil`, 357 with `refresh`. New dependency: `getrandom` (already in the tree via axum).

## 0.5.0 — 2026-10-24

Ground-segment completeness (Tranche 3 of the 1.0 plan, **D-048**, **D-049**, **D-050**). The
public API is **additive** against 0.4.0 (`cargo semver-checks` passes even as a patch). The minor
bump marks one **behaviour change for embedders** (below) and the new feature surface. Operators
upgrading from 0.4.0 need no config changes: the defaults are one Space Packet per datagram, as
before, and an in-memory history.

- **CCSDS TM Transfer Frames (new `frames` module, `[framing]` table).** `mode = "tm_frames"` decodes
  132.0-B-3 frames: sync-marker (`1ACFFC1D`) search across datagrams, the 131.0-B-4
  pseudo-randomizer, CRC-16 FECF, spacecraft and virtual-channel filters, master/VC frame-counter
  gap detection, and packet reassembly across frame boundaries via the First Header Pointer.
  Idle frames and idle packets are dropped. `TmFrameBuilder` is the inverse (used by tools and
  tests). Messages gain an additive `vcid` field.
- **One processing pipeline (new `pipeline` module).** Each packet is parsed and validated
  **once** and the result is published to history and every WebSocket client. Before, each client
  did its own processing. Physics now runs once per packet instead of once per client, telemetry
  is processed with no dashboard connected, and `telemetry_frames_emitted` / `anomaly_frames` / the
  RF match counters count packets, not packets × clients. **Behaviour change:** a server built from
  `http::router` must also run `pipeline::spawn(state, shutdown)`; the binary does this.
- **History (new `history` module, `[history]` table).** A bounded ring of recent messages (default
  10 000), optionally backed by a JSON-lines file that is reloaded at startup, compacted to stay
  bounded, and written from its own thread. `GET /api/v1/chronus/history?start=&end=&apid=&limit=`
  (`chronus.history.v1`) replaces the stub.
- **Open MCT (new `openmct` module).** `GET /api/v1/chronus/openmct/dictionary` is now generated
  from one table of telemetry points (replacing the stub), and
  `GET /api/v1/chronus/openmct/history/{key}` serves each point's history as Open MCT datums. New
  `demo/openmct/`: NASA Open MCT 4.3.1 with a Chronus plugin (object tree, history, one shared
  WebSocket, alarm limits) and a headless reload check (`verify_history.py`).
- **Published JSON Schemas** for `openmct.realtime.v1`, `chronus.rf.v1`, `chronus.history.v1` and
  the dictionary, in `crates/gateway/schemas/` and served at `/api/v1/chronus/schemas/{name}`.
- **Telecommand scoped out of 1.x** (`docs/TELECOMMAND.md`). The crate description no longer says
  "TMTC gateway".
- **Operator surface.** `/api/v1/chronus/metrics` gains `framing`, `history` and `pipeline`
  objects. `/status` gains `CG-513` (transfer frames failing their checks, or frame gaps) and
  `CG-514` (history file problems). New startup codes: `CG-134` (bad `[framing]` / `[history]`
  setting), `CG-135` (history file can't be opened) and the `CG-136` warning (frames larger than a
  datagram). `CG-502` and the first-invalid-packet log point at `[framing]` when the data looks
  like CADUs. `CG-509` no longer says nothing is stored.
- **`chronus-replay`:** `--tm-frames` (with `--frame-length`, `--scid`, `--vcid`, `--asm`,
  `--randomize`, `--no-fecf`). New fixtures: `tm_frames_lab.hex` (a lab CADU stream produced by the
  independent Python encoder `gen_tm_frames.py`, with its `expected.json`) and `iss_pass_lab.jsonl`
  (a whole pass with a Doppler fault).
- **Exit criteria met:** the recorded frame stream decodes end to end through the real binary
  (`crates/gateway/tests/ground_segment_e2e.rs`, also cross-checked against the encoder in
  `tm_frames_fixture.rs`). History survives a `kill -9` and restart, and Open MCT shows it after a
  page reload (`demo/openmct/verify_history.py`; screenshots in `docs/assets/`).
- **Tests:** 316 → 376 workspace tests (286 unit, 40 integration, 50 doctests); 318 when the
  gateway is tested without `hil`. New dev-dependency: `jsonschema` (MIT, no default features).

## 0.4.0 — 2026-10-11

Live receiver data and spacecraft time (Tranche 2 of the 1.0 plan, **D-047**). **Purely additive**
against 0.3.0 (`cargo semver-checks` clean) and **off by default**: with no `[rf]` or `[time]`
table the gateway behaves as in 0.3.0. The minor bump marks the new feature surface.

- **`chronus.rf.v1` receiver side channel (new `rf` module).** A second, optional UDP port
  (`[rf] bind_addr`) takes one JSON datagram per measurement (carrier Hz, received power dBm,
  antenna az/el, optional timestamp). Each frame is matched to the sample nearest its receive time
  within `rf.window_ms`; a bounded store (`rf.capacity`) holds recent samples. This makes the
  Doppler (bit 0), link-budget (bit 2) and pointing (bit 3) checks work in the shipped binary,
  where before only embedders could drive them. `RfSampleV1` builds and parses the datagram, and
  `RfSampleV1::nominal_for` is the "perfect receiver" for simulators and tests.
- **Spacecraft time (new `time` module).** `[time] format = "cds" | "cuc"` decodes the 6-byte time
  code at the start of secondary-header packets. Physics runs at the **onboard time**, falling back
  to receive time. `max_skew_s = 0` is lab/replay mode (recorded or simulated mission time is
  trusted). The time field is excluded from `TelemetryFrame::user_data()`.
- **`physics_flags_v2`** (new field): bit 0 time-tag skew, bit 1 time code invalid
  (`FLAG_V2_TIME_TAG_SKEW`, `FLAG_V2_TIME_CODE_INVALID`). The Open MCT message also gains
  `onboard_time`, `physics_time` and `rf_matched` (all additive; schema stays `openmct.realtime.v1`).
- **Operator surface.** `/api/v1/chronus/metrics` gains an `rf` object; `/status` gains `CG-511`
  (no or unmatched samples) and `CG-512` (invalid datagrams); `CG-129` for bad `[rf]` / `[time]`
  values; `Listener::Rf` for bind errors. New docs: `docs/RF_SIDE_CHANNEL.md`.
- **`chronus-replay`:** per-line `rf` objects with `--rf-dest`; described packets (`apid`, `seq`,
  `data_hex`, `onboard_time` or `"now"`) encoded with a CDS/CUC time field (`--time-format`). New
  fixture `demo/replay/fixtures/rf_lab.jsonl`.
- **`chronus-hil-sim`:** `--time-code`, `--epoch`, `--epoch-visible`, a simulated receiver
  (`--rf-dest`) and scripted receiver faults (`--rf-anomaly doppler|link-budget|pointing`).
  `run_nexosim_hil(HilRunOptions)` is the new entry point; the old functions are unchanged.
- **Exit criterion met:** the real `chronus-gateway` binary, fed over UDP, raises all seven bits
  (`0x7F` in one frame with the `hil` feature), asserted in `crates/gateway/tests/live_rf_e2e.rs`
  and, through the NeXosim simulator, in `crates/chronus-hil-sim/tests/hil_rf_pipeline.rs`.
- **Tests:** 253 → 316 workspace tests (241 unit, 33 integration, 42 doctests); 262 when the
  gateway is tested without `hil`.

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
