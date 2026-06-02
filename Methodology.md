# Methodology — ChronusGateway-RS

A living record of **why** the project is built the way it is: major decisions, frameworks,
trade-offs, and the reasoning behind them. Append new entries as decisions are made; do not
silently rewrite history (mark superseded entries). Required reading + maintenance per
`AGENTS.md`.

> Status: **M1-M4 complete**. UDP ingestion, CCSDS telemetry parsing, station-configured
> tracking, and Physics-Telemetry Co-Validation are implemented and tested. Open MCT WebSocket
> fan-out remains Milestone 5.

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
  tests, `ctrl_c` in `main`) without a `tokio-util` `CancellationToken` dependency.
**Tested by:** `tests/ingest.rs` (order, shutdown, oversized, backpressure).

### D-010 — CCSDS parsing crate: `spacepackets` (resolves OD-A)
**Decision:** Use **`spacepackets` 0.17** (us-irs) for CCSDS Space Packet parsing, wrapped behind
the `ccsds` module so the rest of the gateway depends on our `TelemetryFrame`, not on the crate.
**Why:** It supports the full primary header plus secondary-header/PUS handling we will need for
real telemetry, is actively maintained, and parses with a clean `from_be_bytes` returning the
header and remaining slice. `space-packet` is Kani-verified but primary-header-only; an in-house
parser would duplicate well-tested work and own the correctness burden (against AGENTS security
posture). Keeping it behind the module boundary preserves the option to swap later.
**Frame representation:** `TelemetryFrame` retains the original `Arc<[u8]>` datagram and exposes
the packet data field via a zero-copy `payload()` borrow (no `bytes` crate needed — extends D-009).
**Scope note:** M2 decodes and validates the CCSDS primary header, preserves
`has_secondary_header`, and exposes the packet data field. It does **not** decode secondary-header
contents yet; that remains future distribution/telemetry-schema work.
**Validation:** length → decode → declared-vs-available → TM/TC; recoverable `CcsdsError` per case,
no panics or unbounded allocation on untrusted input.
**Tested by:** inline unit tests in `ccsds.rs` (golden bytes, round-trip, truncation, garbage, routing).

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
- **Bit 2:** reserved for RSSI / link budget (`FLAG_RSSI_RESERVED`); not set in this milestone.
- **`RfMetadata::measured_carrier_hz == None`:** Doppler check skipped (no bit 0); production SDR
  wiring comes with M5 or a side channel.
**Why OD-C is closed:** Ephemerust documents `range_rate_km_s` to ~0.25 km/s vs a 1 s central
difference; at L-band (~437 MHz) that maps to sub-kHz frequency uncertainty from propagation math
alone. The ±150 Hz band is therefore dominated by atmosphere, receiver chain, and clock effects,
not SGP4 truncation at the teaching-grade arcminute level (D-004).
**`TelemetryFrame`:** `raw` and `payload_len` are `pub(crate)` so `validate` unit tests can build
minimal frames without exposing internals on the public API.
**Tested by:** nine `validate` unit tests (in/out-of-band Doppler, horizon, combined flags, NaN-safe
skip, formula identity).

### D-013 — Binary wiring: implemented M1-M4 pipeline before external config/API
**Decision:** `crates/gateway/src/main.rs` runs a development pipeline using `Default`
configuration: bind UDP, broadcast raw frames, parse CCSDS telemetry, obtain a throttled tracking
state, and apply physics validation before logging the frame. It intentionally has no CLI/env
configuration and no Open MCT/WebSocket API yet.
**Why:** This keeps the current milestone path runnable end to end while M5 defines the external
distribution contract. The public library API remains the stable surface for tests and downstream
integration.
**Runtime limit:** The binary passes `RfMetadata::default()` until SDR/front-end measured-carrier
metadata is wired, so Doppler bit 0 is skipped in `cargo run`; the elevation gate still runs when
tracking succeeds. Unit tests cover Doppler behavior directly.

---

## Open decisions (to resolve as milestones land)
- **OD-B — Web/distribution stack.** Axum (mirrors Rusty_Server) for the WebSocket + HTTP API to
  Open MCT. Confirm the Open MCT telemetry dictionary + JSON format contract.
- **OD-D — HIL simulation (NeXosim).** Optional Milestone 7; scope a single simulated spacecraft
  on a laptop before any multi-node/rack topology.

---

## Attribution
External works this project builds on or is inspired by (keep current per `AGENTS.md` rule 2):

| Work | Role here | Source / License |
|------|-----------|------------------|
| **Ephemerust** (owner) | SGP4 propagation, look-angles, range-rate | local sibling crate, MIT |
| `sgp4` crate | Underlying SGP4/SDP4 numerics (via Ephemerust) | crates.io |
| `spacepackets` (us-irs) | CCSDS Space Packet parsing (M2) | crates.io, Apache-2.0/MIT |
| **Rusty_Server** (owner) | Architectural inspiration (async/Axum/config patterns) | sibling repo |
| Tokio, Axum, Tracing, Serde, Chrono, Anyhow, Thiserror | Runtime/infra crates | crates.io, MIT/Apache-2.0 |
| CCSDS standards | TMTC framing/packet specifications | open international standards |
| NASA Open MCT | Target mission-control dashboard | open source (NASA) |

---

*Last updated: 2026-06-02.*
