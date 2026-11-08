# Requirements and tolerance traceability

This is the in-repository **tolerance register** and the map from each requirement to the tests
and evidence that verify it (Tranche 5.4, `Methodology.md` D-057). It replaces the register that
used to live in the maintainer's local `TEST_PLAN.md`: every tolerance ID the code mentions
(`T-DOPPLER`, `T-POINT`, …) is defined here, and every alarm bit has a row.

`tools/check_traceability.py` runs in CI and fails when:

- a test named here (written as path-to-file.rs::test_name) no longer exists, or a file named here is gone;
- an alarm constant (`FLAG_*` in `crates/gateway/src/validate.rs`) has no row in the register;
- the code uses a tolerance ID (`T-XXX`) that the register does not define.

So a renamed test, a new alarm bit or a new tolerance cannot land without this page following.

## 1. Tolerance register

One row per check. *Alarm* is where the result appears: a bit of `physics_flags` /
`physics_flags_v2` in every realtime message (`openmct.realtime.v1`), or a counter on
`/metrics`. All defaults are configurable; `gateway.example.toml` lists the keys.

| ID | Check | Alarm | Default | Config key | Basis | Verified by |
|---|---|---|---|---|---|---|
| **T-DOPPLER** | Measured carrier vs `f₀·(1 − ṙ/c)` from the propagated range rate | `physics_flags` bit 0, `FLAG_DOPPLER_ANOMALY` | ±150 Hz (+ TLE-age margin when enabled) | `station.doppler_tolerance_hz`, `station.doppler_tle_age_margin` | D-012, D-041, D-044; budget: TLE along-track error ≈ 25 Hz/km at UHF, then ionosphere, receiver, oscillator | `crates/gateway/src/validate.rs::doppler_in_band_no_flag_t_doppler_150hz`, `crates/gateway/src/validate.rs::doppler_out_of_band_sets_bit0`, `crates/gateway/src/validate.rs::expected_carrier_matches_non_relativistic_formula`, `crates/gateway/src/validate.rs::tle_age_uncertainty_widens_the_doppler_band`, `crates/gateway/src/validate.rs::no_measured_carrier_skips_doppler_even_if_would_be_bad`, `crates/gateway/src/validate.rs::nan_measured_skips_doppler_no_panic`, `crates/gateway/tests/live_rf_e2e.rs::rf_side_channel_drives_doppler_link_budget_and_pointing_bits`; real RF: [RF_VALIDATION.md](RF_VALIDATION.md) |
| **T-RANGERATE** | Propagator range rate vs its own d(range)/dt (model accuracy; no alarm) | — (keeps T-DOPPLER's budget for real errors) | < 1 m/s (≈ 1.5 Hz at 437 MHz) | — | D-041; Ephemerust 1.1 agrees with an IAU 2000 reference to 0.41 m/s | `crates/gateway/src/propagator.rs::range_rate_matches_numerical_derivative_of_range`, `crates/gateway/src/propagator.rs::tle_age_uncertainty_matches_an_along_track_shift` |
| **T-HORIZON** | Geometric elevation below the station mask | `physics_flags` bit 1, `FLAG_BELOW_HORIZON` | 0° | `station.minimum_elevation_deg` | M4; geometric (not refracted) by design, D-042 | `crates/gateway/src/validate.rs::below_horizon_sets_bit1`, `crates/gateway/src/validate.rs::elevation_at_horizon_not_flagged_when_minimum_is_zero`, `crates/chronus-hil-sim/tests/hil_rf_pipeline.rs::a_run_below_the_horizon_raises_bit_1_on_every_frame` |
| **T-RSSI** | Received power vs free-space (Friis) prediction | `physics_flags` bit 2, `FLAG_LINK_BUDGET_ANOMALY` | ±3 dB | `station.link_budget_tolerance_db` (+ `tx_power_dbm`, `tx_gain_dbi`, `rx_gain_dbi`) | CV-1; free space only in v1 (no atmosphere or cable loss) | `crates/gateway/src/validate.rs::free_space_path_loss_matches_manual`, `crates/gateway/src/validate.rs::link_budget_in_band_no_flag_t_rssi`, `crates/gateway/src/validate.rs::link_budget_out_of_band_sets_bit2`, `crates/gateway/src/validate.rs::nan_measured_rx_skips_link_no_panic`, `crates/gateway/src/validate.rs::zero_range_skips_link_budget_no_flag` |
| **T-POINT** | Great-circle angle between measured and computed (az, el), apparent elevation when refraction is modelled | `physics_flags` bit 3, `FLAG_POINTING_ANOMALY` | 0.25° | `station.pointing_tolerance_deg` | CV-2, D-042 | `crates/gateway/src/validate.rs::pointing_within_t_point_no_bit3`, `crates/gateway/src/validate.rs::pointing_exceeds_t_point_sets_bit3`, `crates/gateway/src/validate.rs::pointing_uses_apparent_elevation_when_refraction_is_modelled`, `crates/gateway/src/validate.rs::pointing_only_azimuth_skips_no_bit3`, `crates/gateway/src/validate.rs::non_finite_pointing_tolerance_skips_pointing` |
| **T-EPS** | HIL bus voltage vs sun-angle toy model (`hil` feature) | `physics_flags` bit 4, `FLAG_EPS_SUBSYSTEM_ANOMALY` | ±10 % relative | `station.hil_eps_relative_tolerance` | CV-4 (synthetic model, D-022) | `crates/gateway/src/validate.rs::hil_cv4_voltage_and_thermal_in_band`, `crates/gateway/src/validate.rs::hil_cv4_bad_voltage_sets_bit4`, `crates/gateway/src/validate.rs::hil_cv4_skips_when_illum_non_finite` |
| **T-THERMAL** | HIL panel temperature vs sun-angle band (`hil` feature) | `physics_flags` bit 5, `FLAG_THERMAL_SUBSYSTEM_ANOMALY` | ±10 K | `station.hil_thermal_absolute_tolerance_k` | CV-4 | `crates/gateway/src/validate.rs::hil_cv4_bad_thermal_sets_bit5`, `crates/chronus-hil-sim/tests/hil_rf_pipeline.rs::subsystem_faults_raise_bits_4_5_6_on_exactly_the_scripted_frames` |
| **T-BODYRATE** | HIL body-rate magnitude ceiling (`hil` feature) | `physics_flags` bit 6, `FLAG_ADCS_BODY_RATE_ANOMALY` | 5 °/s | `station.hil_body_rate_max_abs_deg_s` | CV-5 | `crates/gateway/src/validate.rs::hil_cv5_body_rate_within_ceiling_no_bit6`, `crates/gateway/src/validate.rs::hil_cv5_body_rate_exceeds_ceiling_sets_bit6`, `crates/gateway/src/validate.rs::hil_cv5_skips_when_body_rate_non_finite`, `crates/gateway/src/validate.rs::hil_cv5_skips_when_max_not_positive` |
| **T-SKEW** | Onboard time tag vs receive time; physics falls back to receive time when exceeded | `physics_flags_v2` bit 0, `FLAG_V2_TIME_TAG_SKEW` | 5 s (`0` = lab mode, check off) | `time.max_skew_s` | D-047 | `crates/gateway/src/http.rs::onboard_time_drives_physics_and_skew_falls_back_to_receive_time`, `crates/gateway/tests/live_rf_e2e.rs::untrusted_or_unreadable_time_falls_back_to_receive_time_and_flags_v2` |
| **T-TIMECODE** | Secondary-header time code (CDS / CUC) decodes to a valid instant | `physics_flags_v2` bit 1, `FLAG_V2_TIME_CODE_INVALID` | exact (field layout) | `time.format` | D-047; CCSDS 301.0-B | `crates/gateway/src/time.rs::short_field_is_a_typed_error`, `crates/gateway/src/time.rs::cds_ms_of_day_out_of_range_is_rejected`, `crates/gateway/src/time.rs::cds_round_trips_to_the_millisecond`, `crates/gateway/src/time.rs::cuc_round_trips_within_fine_resolution`, `crates/gateway/tests/live_rf_e2e.rs::cuc_time_codes_are_decoded_in_lab_mode` |
| **T-OEM** | OEM backend station-relative state vs an independent (astropy) ITRF reference (model accuracy; no alarm) | — | < 0.15 m position | — | D-045 | `crates/gateway/tests/oem_backend.rs::every_frame_and_time_system_reproduces_astropy_itrf`, `crates/gateway/tests/oem_backend.rs::oem_backend_tracks_the_same_orbit_as_the_tle_backend` |
| **T-EOP** | IERS finals2000A rows parsed and interpolated; both published layouts agree (model input; no alarm) | `chronus_data_reload_errors_total` on a bad file | exact to the published digits | `freshness.eop_file` | D-052 | `crates/gateway/src/freshness.rs::real_iers_csv_parses_with_the_published_values`, `crates/gateway/src/freshness.rs::real_csv_and_fixed_width_forms_agree`, `crates/gateway/src/freshness.rs::leap_second_is_not_interpolated_through` |
| **T-FECF** | TM Transfer Frame CRC-16 (FECF); failing frames are dropped and counted | `chronus_framing_frames_crc_error_total` | exact | `framing.fecf` | CCSDS 132.0-B, D-048 | `crates/gateway/src/frames.rs::crc_matches_the_standard_check_value_and_detects_a_flip`, `crates/gateway/src/frames.rs::parse_rejects_crc_version_sync_and_short_frames` |

Bit 7 of `physics_flags` and bits 2–7 of `physics_flags_v2` are reserved (D-016): assigning one
needs a new row here, a JSON Schema update and a Methodology entry.

All seven `physics_flags` bits are exercised together through the real binary by
`crates/gateway/tests/live_rf_e2e.rs::all_seven_physics_bits_fire_through_the_real_binary`, and each
fault is shown to raise only its own bit by
`crates/chronus-demo-server/src/sandbox.rs::faults_are_isolated_to_their_own_bits`.

## 2. Requirements → verification

| ID | Requirement | Verified by |
|---|---|---|
| **R-ING-1** | Receive CCSDS packets over UDP without ever blocking on slow consumers; bound datagram size | `crates/gateway/tests/ingest.rs::receives_all_datagrams_in_order`, `crates/gateway/tests/ingest.rs::oversized_datagram_does_not_break_loop`, `crates/gateway/tests/ingest.rs::lagging_subscriber_never_blocks_socket`, `crates/gateway/src/pipeline.rs::lag_is_counted_and_resets_the_deframer` |
| **R-ING-2** | Accept telemetry and RF datagrams only from configured source networks | `crates/gateway/src/ingest.rs::filtered_run_drops_senders_outside_the_allow_list`, `crates/gateway/src/security.rs::source_filter_counts_rejections_per_port` |
| **R-PKT-1** | Parse the Space Packet primary header; reject telecommand, short and truncated packets; never panic on any input | `crates/gateway/src/ccsds.rs::parses_known_golden_bytes`, `crates/gateway/src/ccsds.rs::telecommand_is_rejected`, `crates/gateway/src/ccsds.rs::truncated_payload_is_rejected`, `crates/gateway/src/ccsds.rs::parse_random_bytes_never_panics`; fuzz: `fuzz/fuzz_targets/ccsds_packet.rs` |
| **R-PKT-2** | Decode CDS and CUC time codes; physics runs at onboard time when it is trusted | T-SKEW, T-TIMECODE rows; `crates/gateway/src/time.rs::arbitrary_bytes_never_panic` |
| **R-TMF-1** | Recover packets from TM Transfer Frames (ASM, derandomization, FECF, virtual channels, spanning packets) with bounded memory and counted loss | `crates/gateway/src/frames.rs::packets_spanning_many_frames_and_sharing_frames_are_recovered`, `crates/gateway/src/frames.rs::a_lost_frame_drops_only_the_packet_it_cut`, `crates/gateway/src/frames.rs::garbage_never_panics_and_memory_stays_bounded`, `crates/gateway/tests/tm_frames_fixture.rs::rust_deframer_recovers_exactly_what_the_python_encoder_sent`; fuzz: `fuzz/fuzz_targets/tm_deframer.rs` |
| **R-PHY-1** | Co-validate every packet against orbital physics and publish the result as alarm bits | Section 1; `crates/gateway/tests/live_rf_e2e.rs::all_seven_physics_bits_fire_through_the_real_binary`, `crates/chronus-hil-sim/tests/hil_rf_pipeline.rs::nominal_hil_and_rf_raise_nothing` |
| **R-PHY-2** | Doppler residuals on a real pass stay inside T-DOPPLER once receiver offset is removed | `crates/chronus-evidence/src/doppler.rs::offset_receiver_passes_once_the_offset_is_fitted`, `crates/chronus-evidence/src/doppler.rs::wrong_physics_is_caught_by_the_shape_gain`; evidence: [RF_VALIDATION.md](RF_VALIDATION.md) |
| **R-EPH-1** | Track from a TLE (SGP4) or a CCSDS OEM, with Earth orientation applied | `crates/gateway/src/propagator.rs::earth_orientation_shifts_tracking_slightly`, `crates/gateway/src/oem.rs::refuses_to_extrapolate`, T-OEM and T-EOP rows; fuzz: `fuzz/fuzz_targets/oem_text.rs`, `fuzz/fuzz_targets/eop_finals.rs` |
| **R-RF-1** | Accept receiver measurements on the `chronus.rf.v1` side channel, bounded, matched by time | `crates/gateway/src/rf.rs::lookup_picks_nearest_inside_window`, `crates/gateway/src/rf.rs::capacity_evicts_oldest_first`, `crates/gateway/src/rf.rs::rejects_foreign_schema_garbage_and_missing_schema`, `crates/gateway/src/rf.rs::long_decimals_parse_to_the_nearest_double_and_round_trip`; fuzz: `fuzz/fuzz_targets/rf_datagram.rs` |
| **R-HIL-1** | Decode the synthetic HIL TM v1 payload and reject anything else | `crates/gateway/src/hil_tm.rs::golden_encode_then_decode_round_trip`, `crates/gateway/src/hil_tm.rs::wrong_magic_rejected`, `crates/chronus-hil-sim/tests/hil_ingest.rs::nexosim_smoke_reaches_ingest_and_parse`; fuzz: `fuzz/fuzz_targets/hil_tm_v1.rs` |
| **R-DST-1** | Stream every validated packet as versioned JSON over WebSocket; the published JSON Schemas match the output | `crates/gateway/tests/distribution.rs::websocket_receives_openmct_json`, `crates/gateway/tests/schemas.rs::realtime_messages_validate_with_and_without_physics_and_time`, `crates/gateway/tests/schemas.rs::every_published_schema_is_a_valid_json_schema` |
| **R-DST-2** | Keep a bounded, optionally persistent history and serve it to Open MCT | `crates/gateway/src/history.rs::ring_is_bounded_and_evicts_oldest`, `crates/gateway/src/history.rs::persisted_history_survives_a_restart_and_is_compacted`, `crates/gateway/tests/ground_segment_e2e.rs::recorded_frame_stream_decodes_end_to_end_and_history_survives_a_restart`, `crates/gateway/src/http.rs::point_history_returns_openmct_datums` |
| **R-SEC-1** | API tokens on every route but `/health`; Origin check; WebSocket client cap; refuse unauthenticated remote binds | `crates/gateway/src/http.rs::tokens_guard_every_route_but_health`, `crates/gateway/src/security.rs::bind_policy_refuses_only_unauthenticated_remote`, `crates/gateway/tests/security_ops_e2e.rs::tokens_origins_client_cap_and_udp_allow_list_are_enforced` |
| **R-OPS-1** | Explain every configuration error with a code, cause and fix; `--check-config` refuses unsafe setups | `crates/gateway/src/diagnose.rs::every_config_error_explanation_is_an_error_with_a_fix`, `crates/gateway/tests/security_ops_e2e.rs::check_config_refuses_unsafe_configurations_with_explanations`; fuzz: `fuzz/fuzz_targets/config_toml.rs` |
| **R-OPS-2** | Expose counters, data age and process memory in Prometheus format | `crates/gateway/src/prometheus.rs::exposition_is_well_formed_and_names_are_unique`, `crates/gateway/src/http.rs::prometheus_endpoint_serves_text_exposition` |
| **R-OPS-3** | Reload TLE / OEM / EOP files while running and report their age | `crates/gateway/src/freshness.rs::refresher_applies_todays_eop_and_reloads_changed_files`, `crates/gateway/tests/security_ops_e2e.rs::data_files_are_reloaded_and_their_age_is_reported` |
| **R-ROB-1** | Every parser of untrusted input survives coverage-guided fuzzing (24 h per target for 1.0) | `fuzz/fuzz_targets/packet_pipeline.rs` and the seven targets above; workflow `.github/workflows/fuzz.yml`; ledger: [FUZZING.md](FUZZING.md) |
| **R-PERF-1** | Sustain the design load with bounded memory and a stated p99 end-to-end latency | `crates/chronus-evidence/tests/envelope.rs::gateway_output_carries_the_probe`, `crates/chronus-evidence/src/soak.rs::rss_slope_flags_growth_and_ignores_warmup`; evidence: [SOAK.md](SOAK.md) |

## 3. Evidence status for 1.0.0

| Evidence | Where | Status |
|---|---|---|
| Fuzzing, 24 h per target | [FUZZING.md](FUZZING.md) | 600 s clean per target so far; one bug found and fixed; nightly workflow accumulates the rest |
| Soak / load | [SOAK.md](SOAK.md) | 10 min at the design load: zero loss, p99 13 ms, flat memory; 24 h run on the release host pending |
| Real-RF Doppler | [RF_VALIDATION.md](RF_VALIDATION.md) | tooling and synthetic rehearsal done; owner's own capture pending |
| One week on the public demo, no API change | `CHANGELOG.md` 1.0.0-rc.1 | starts when the rc is deployed |
