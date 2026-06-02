# ChronusGateway-RS — Iterative Test Plan

The testing companion to `BUILD_PLAN.md`. It encodes the **Ephemerust testing standard**
required by `AGENTS.md` rule 4: layered tests, documented physics tolerances, deterministic and
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

**`physics_flags` contract**

| Bit | Mask | Set by | Notes |
|-----|------|--------|-------|
| 0 | `0x01` | Doppler anomaly | Requires `RfMetadata::measured_carrier_hz = Some(finite)`. `None` and non-finite values skip the check. |
| 1 | `0x02` | Elevation below threshold | Strict comparison: `elevation_deg < minimum_elevation_deg`. |
| 2 | `0x04` | Reserved RSSI/link-budget anomaly | Documented for forward compatibility; not set in M4. |

M4 coverage is unit-level with deterministic mock `TrackingState`s. Full
`ingest -> parse -> validate -> WebSocket` end-to-end coverage is deferred to M5 because the
distribution layer is not implemented yet.

### M5 — Distribution
- [ ] **End-to-end:** in-process `ingest → parse → validate → WebSocket`; a connected client
      receives well-formed Open MCT JSON including `physics_flags`.
- [ ] **Health:** `GET /health` returns `200`.
- [ ] **Lifecycle:** client disconnect is handled without affecting other clients or the loop.

### M6 — Hardening
- [ ] **Benchmarks:** `criterion` parse + validate hot paths produce reported latency numbers.
- [ ] **Fuzz/property:** randomized byte streams never panic the parser.
- [ ] **Supply chain:** `cargo audit` / `cargo deny` clean.

### M7 — HIL simulation
- [ ] **Smoke:** NeXosim sim → gateway delivers validated frames to a client.
- [ ] **Soak:** sustained-rate run for a fixed duration with bounded drops and no leaks.

---

## Tolerance Register (justify every number)
Populate as engines land; keep rationale next to the value (Ephemerust style).

| ID | Quantity | Tolerance | Rationale / source |
|----|----------|-----------|--------------------|
| T-DOPPLER | Carrier Δf deviation | ±150 Hz | **Locked (M4 / OD-C).** PDF atmospheric/ionospheric drift band; Ephemerust `range_rate_km_s` is validated to ~0.25 km/s vs central difference — at 437.5 MHz that is sub-kHz from propagation math, so ±150 Hz is conservative for physics-only error. |
| T-ELEVATION | Minimum elevation for valid TM | Configurable (`minimum_elevation_deg`, default **0°**) | Flag when `elevation_deg < threshold` (strict inequality). Default: at or above mathematical horizon passes; use negative threshold for refraction margin. |
| T-RANGERATE | Range-rate vs numerical | 0.25 km/s | Matches Ephemerust's central-difference check (reused convention). |
| T-RSSI | Received power | ±3 dB (provisional) | PDF link-budget margin; revisit when/if implemented. |

---

## Status / counts (keep current)
| Layer | Count | Notes |
|-------|-------|-------|
| Unit tests | 24 | `ccsds` (7) + `config` (4) + `propagator` (4) + `validate` (9). |
| Integration tests | 4 | `tests/ingest.rs` (order, shutdown, oversized, backpressure). |
| Doctests | 1 | `EphemerustPropagator::new`. |

*Last updated: 2026-06-02.*
