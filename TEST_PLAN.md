# ChronusGateway-RS — Iterative Test Plan

The testing companion to [`docs/BUILD_PLAN.md`](docs/BUILD_PLAN.md). It encodes the **Ephemerust testing standard**
required by the project's stage-gate protocol: layered tests, documented physics tolerances, deterministic and
offline, enforced at every stage gate.

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
- [x] Binary smoke run produces a finite `TrackingState` (az/el/range/range-rate) from the ISS TLE.
- [x] Doctest on `EphemerustPropagator::new` showing construction + a bounded assertion.
- [x] Unit tests: finite tracking state near epoch; invalid TLE rejected.

### M1 — Ingestion
- [x] **Integration:** bind to an ephemeral loopback port, send `N` datagrams, assert `N` frames
      observed on the channel in order (`receives_all_datagrams_in_order`).
- [x] **Shutdown:** cancellation stops the loop promptly with no leaked task/panic
      (`shutdown_stops_loop_promptly`).
- [x] **Security:** oversized datagram handled safely; loop keeps delivering valid frames; buffer
      fixed at `max_datagram_size` (`oversized_datagram_does_not_break_loop`).
- [x] **Backpressure:** a slow subscriber observes `Lagged` while the socket loop receives all
      datagrams (`lagging_subscriber_never_blocks_socket`).

### M2 — CCSDS parsing
- [x] **Unit:** parse a golden primary header → correct type/APID/seq/length
      (`parses_valid_tm_packet`, `parses_known_golden_bytes`).
- [x] **Round-trip:** canonical bytes → parse equals original fields, incl. min/max APID & seq
      (`round_trip_preserves_fields`).
- [x] **Robustness:** short header, truncated payload, and all-`0xFF` garbage return structured
      errors, never a panic (`short_datagram_is_rejected`, `truncated_payload_is_rejected`,
      `garbage_does_not_panic`).
- [x] **Routing:** TM accepted; TC rejected with `NotTelemetry` (`telecommand_is_rejected`).

### M3 — Propagator integration
- [x] **Unit:** config validation (valid default; invalid lat/lon/freq/altitude; empty inline TLE;
      missing TLE file) with specific errors (`default_station_is_valid`, `rejects_out_of_range_fields`,
      `resolves_inline_tle_and_rejects_empty`, `missing_tle_file_is_reported`).
- [x] **Deterministic:** fixed TLE + fixed instant → stable `TrackingState`, baseline-locked within
      tolerance (`from_station_is_deterministic_and_in_tolerance`).
- [x] **Trait swap + throttle:** a counting mock propagator proves the seam and the
      recompute-throttle cache (`provider_uses_mock_and_throttles_recompute`).

### M4 — Co-validation
- [x] **Doppler in-band:** measured carrier within **T-DOPPLER ±150 Hz** of `expected_carrier_hz` →
      no bit 0 (`doppler_in_band_no_flag_t_doppler_150hz`).
- [x] **Doppler out-of-band:** deviation beyond bound → bit 0 (`doppler_out_of_band_sets_bit0`).
- [x] **Look-angle / elevation:** predicted elevation strictly below `minimum_elevation_deg` → bit 1
      (`below_horizon_sets_bit1`); at-threshold edge (`elevation_at_horizon_not_flagged_when_minimum_is_zero`).
- [x] **Bitfield:** independent anomalies set independent bits; clean frame = `0`
      (`combined_anomalies_set_both_bits`, `independent_bits_doppler_only`, `no_measured_carrier_skips_doppler_even_if_would_be_bad`).
- [x] **Non-finite RF:** NaN measured carrier skips Doppler without panic (`nan_measured_skips_doppler_no_panic`).
- [x] **Formula:** non-relativistic Doppler identity locked by unit test (`expected_carrier_matches_non_relativistic_formula`).

### M5 — Distribution
- [x] **End-to-end:** in-process `ingest → parse → validate → WebSocket`; a connected client
      receives well-formed Open MCT JSON including `physics_flags`.
- [x] **Health:** `GET /health` returns `200`.
- [x] **Lifecycle:** client disconnect is handled without affecting other clients or the loop.

### M6 — Hardening
- [x] **Benchmarks:** `criterion` parse + validate hot paths (`cargo bench -p chronus-gateway`).
- [x] **Fuzz/property:** randomized byte streams never panic the parser (`ccsds` proptest).
- [x] **Supply chain:** `cargo audit` / `cargo deny` in CI (`deny.toml`).

### M7 — HIL simulation
- [x] **Smoke:** NeXosim sim → gateway delivers validated frames to a client (`chronus-hil-sim` +
      real `ingest::run` on loopback).
- [x] **Soak:** sustained simulated-rate run (400 frames) with `recv_errors == 0` and full parse.

### M8 — File configuration
- [x] **TOML:** valid file → merged `IngestConfig` + `StationConfig`; startup runs `validate()` and
      TLE resolution (inline or readable file).
- [x] **Errors:** ambiguous/missing TLE keys, bad `SocketAddr`, unknown top-level keys, missing TLE
      file → structured `ConfigLoadError` / `ConfigError` (unit tests in `config/file.rs`).

### CV-0 — Extended co-validation charter (documentation only)
Charter lives in **`docs/EXTENDED_COVALIDATION_PLAN.md`** and **`Methodology.md` D-016**. No code
behavior change beyond documenting contracts.

- [x] **`physics_flags` bit map** — bits 2–5 assigned to CV-1 / CV-2 / CV-4; bits 6–7 reserved;
  JSON evolution policy if \(>8\) alarms (`physics_flags_v2`).
- [x] **RfMetadata vs payload** — ground-chain measurements on `RfMetadata`; spacecraft scalars via
  versioned synthetic CCSDS payload decode after **CV-3**.
- [x] **Tolerance register** — **T-RSSI**, **T-POINT**, **T-EPS**, **T-THERMAL** rows below (charter
  values; rebaseline when models land in CV-1 / CV-2 / CV-4).
- [x] **Explicit deferrals** — listed under D-016 (atmosphere, full PUS, SPICE-grade ephemeris,
  uncalibrated hardware RSSI).

**Gate CV-0:** `[x]` **Owner approval** obtained — **CV-1** implementation may proceed (see extension plan).

---

## Tolerance Register (justify every number)
Populate as engines land; keep rationale next to the value (Ephemerust style).

| ID | Quantity | Tolerance | Rationale / source |
|----|----------|-----------|--------------------|
| T-DOPPLER | Carrier Δf deviation | ±150 Hz | **Locked (M4 / OD-C).** PDF atmospheric/ionospheric drift band; Ephemerust `range_rate_km_s` is validated to ~0.25 km/s vs central difference — at 437.5 MHz that is sub-kHz from propagation math, so ±150 Hz is conservative for physics-only error. |
| T-ELEVATION | Minimum elevation for valid TM | Configurable (`minimum_elevation_deg`, default **0°**) | Flag when `elevation_deg < threshold` (strict inequality). Default: at or above mathematical horizon passes; use negative threshold for refraction margin. |
| T-RANGERATE | Range-rate vs numerical | 0.25 km/s | Matches Ephemerust's central-difference check (reused convention). |
| T-RSSI | \(\|P_{rx,\mathrm{meas}} - P_{rx,\mathrm{pred}}\|\) on **free-space** link budget | **±3 dB** | **Charter (CV-0 / D-016).** Matches design-paper margin for a **synthetic** dBm contract. **Caveat:** v1 prediction is **free-space only** (no rain, ionosphere, cable, or pointing loss folded into \(P_{rx,\mathrm{pred}}\)); measured values must use the same calibration fiction in tests/HIL. Revisit after CV-1 if a richer budget is justified. |
| T-POINT | Great-circle angular separation between measured \((Az,El)\) and computed boresight | **0.25°** | **Charter (CV-0 / D-016).** Design-paper encoder vs computed residual; implementation in CV-2 uses spherical geometry. Revisit if station mount flex or refraction dominates in a given demo. |
| T-EPS | Array current vs toy \(I_{\max}\cos\theta\) model (illumination + optional eclipse clamp) | **±10%** of \(I_{\max}\) near full sun | **Provisional (CV-0).** Placeholder for **CV-4** toy EPS check; rebaseline when sun model + decoded TM layout are fixed (Ephemerust-style numeric cross-check in tests). |
| T-THERMAL | Panel or bus temperature vs crude band tied to sun-angle proxy | **±10 K** vs model midpoint | **Provisional (CV-0).** Placeholder for **CV-4** demo thermal band — **not** flight thermal analysis; tighten or replace when HIL emits self-consistent synthetic physics. |

---

## Status / counts (keep current)
| Layer | Count | Notes |
|-------|-------|-------|
| Unit tests | 33 | `ccsds` (8 incl. proptest) + `config` (12) + `propagator` (4) + `validate` (9). |
| Integration tests | 9 | `crates/gateway/tests/*.rs` (7) + `crates/chronus-hil-sim/tests/hil_ingest.rs` (2). |
| Doctests | 1 | `EphemerustPropagator::new`. |

*Last updated: 2026-06-03.*
