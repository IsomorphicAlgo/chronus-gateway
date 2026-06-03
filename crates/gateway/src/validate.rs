//! Physics–Telemetry Co-Validation (Milestone 4).
//!
//! Cross-checks each [`TelemetryFrame`] against the propagator-derived [`TrackingState`] and sets
//! [`TelemetryFrame::physics_flags`] for downstream consumers (e.g. Open MCT alarm coloring).
//!
//! ## Doppler model
//!
//! Non-relativistic line-of-sight Doppler using the signed range rate from Ephemerust
//! (`range_rate_km_s`, **positive = receding**, **negative = approaching**):
//!
//! `f_expected = f_nominal − f_nominal × (v_m/s / c)` where `v_m/s = range_rate_km_s × 1000`.
//!
//! This matches the project design document convention (Δf from relative range rate). The
//! Ephemerust range-rate magnitude is validated to ~0.25 km/s against a central difference in
//! that crate; at 437.5 MHz that leaves sub-kHz uncertainty from propagation math — the **±150 Hz**
//! acceptance band is dominated by atmospheric / ionospheric drift and receiver chain tolerance
//! (see `TEST_PLAN.md` T-DOPPLER and `Methodology.md` D-012).
//!
//! ## `physics_flags` bitfield (stable contract)
//!
//! | Bit | Mask | Meaning |
//! |-----|------|---------|
//! | 0 | `0x01` | Doppler anomaly — measured carrier differs from expected beyond tolerance. |
//! | 1 | `0x02` | Horizon / elevation — spacecraft is below the configured minimum elevation. |
//! | 2 | `0x04` | **Reserved** — link budget / RSSI (not implemented in this milestone). |
//!
//! When [`RfMetadata::measured_carrier_hz`] is `None`, the Doppler check is **skipped** (no bit 0);
//! lab and integration tests pass `Some`. Production wiring from SDR metadata arrives with the
//! distribution layer or a dedicated ingest side-channel.

use crate::ccsds::TelemetryFrame;
use crate::propagator::TrackingState;

/// Speed of light in vacuum (m/s); CODATA-compatible constant for Doppler arithmetic.
pub const SPEED_OF_LIGHT_M_S: f64 = 299_792_458.0;

/// Bit 0 — measured carrier frequency inconsistent with range-rate Doppler.
pub const FLAG_DOPPLER_ANOMALY: u8 = 0x01;
/// Bit 1 — spacecraft below the configured minimum elevation (e.g. below local horizon).
pub const FLAG_BELOW_HORIZON: u8 = 0x02;
/// Bit 2 — reserved for RSSI / link-budget co-validation (not set by this module yet).
pub const FLAG_RSSI_RESERVED: u8 = 0x04;

/// Optional RF measurements accompanying a frame (typically from the SDR / front-end).
#[derive(Debug, Clone, Copy, Default)]
pub struct RfMetadata {
    /// Measured downlink carrier frequency (Hz). `None` skips the Doppler check.
    pub measured_carrier_hz: Option<f64>,
}

/// Expected received carrier (Hz) from nominal transmit frequency and line-of-sight range rate.
///
/// `range_rate_km_s` follows Ephemerust: positive receding, negative approaching.
///
/// # Examples
///
/// ```
/// use chronus_gateway::expected_carrier_hz;
///
/// let nominal = 437_500_000.0;
/// let approaching = expected_carrier_hz(nominal, -1.0);
/// let receding = expected_carrier_hz(nominal, 1.0);
///
/// assert!(approaching > nominal);
/// assert!(receding < nominal);
/// ```
#[must_use]
pub fn expected_carrier_hz(nominal_hz: f64, range_rate_km_s: f64) -> f64 {
    let v_m_s = range_rate_km_s * 1000.0;
    nominal_hz - nominal_hz * (v_m_s / SPEED_OF_LIGHT_M_S)
}

/// Clears then sets [`TelemetryFrame::physics_flags`] from physics checks.
///
/// Always resets flags to `0` first so repeated validation does not OR stale bits.
/// Non-finite `TrackingState` fields skip checks that depend on them (no panic).
pub fn apply_physics_validation(
    frame: &mut TelemetryFrame,
    state: &TrackingState,
    nominal_carrier_hz: f64,
    rf: RfMetadata,
    doppler_tolerance_hz: f64,
    minimum_elevation_deg: f64,
) {
    frame.physics_flags = 0;

    if state.elevation_deg.is_finite() && state.elevation_deg < minimum_elevation_deg {
        frame.physics_flags |= FLAG_BELOW_HORIZON;
    }

    if let Some(measured) = rf.measured_carrier_hz {
        if measured.is_finite()
            && nominal_carrier_hz.is_finite()
            && state.range_rate_km_s.is_finite()
        {
            let expected = expected_carrier_hz(nominal_carrier_hz, state.range_rate_km_s);
            if (measured - expected).abs() > doppler_tolerance_hz {
                frame.physics_flags |= FLAG_DOPPLER_ANOMALY;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use chrono::Utc;

    fn dummy_tm() -> TelemetryFrame {
        TelemetryFrame {
            raw: vec![0u8; 12].into(),
            payload_len: 6,
            apid: 1,
            seq_count: 0,
            has_secondary_header: false,
            received_at: Utc::now(),
            source: SocketAddr::from(([127, 0, 0, 1], 1)),
            physics_flags: 0,
        }
    }

    fn state(el_deg: f64, range_rate_km_s: f64) -> TrackingState {
        TrackingState {
            azimuth_deg: 90.0,
            elevation_deg: el_deg,
            range_km: 4000.0,
            range_rate_km_s,
        }
    }

    #[test]
    fn expected_carrier_matches_non_relativistic_formula() {
        let f0 = 1e9;
        let v_m_s = 3000.0;
        let v_km_s = v_m_s / 1000.0;
        let exp = expected_carrier_hz(f0, v_km_s);
        let manual = f0 - f0 * (v_m_s / SPEED_OF_LIGHT_M_S);
        assert!((exp - manual).abs() < 1e-6, "exp={exp} manual={manual}");
    }

    #[test]
    fn doppler_in_band_no_flag_t_doppler_150hz() {
        // T-DOPPLER: ±150 Hz — measured within band of expected.
        let mut tm = dummy_tm();
        let s = state(45.0, 0.2648964327533433); // ISS-like range rate from propagator tests
        let f0 = 437_500_000.0;
        let expected = expected_carrier_hz(f0, s.range_rate_km_s);
        apply_physics_validation(
            &mut tm,
            &s,
            f0,
            RfMetadata {
                measured_carrier_hz: Some(expected + 100.0), // +100 Hz < 150
            },
            150.0,
            0.0,
        );
        assert_eq!(tm.physics_flags & FLAG_DOPPLER_ANOMALY, 0);
        assert_eq!(tm.physics_flags & FLAG_BELOW_HORIZON, 0);
    }

    #[test]
    fn doppler_out_of_band_sets_bit0() {
        let mut tm = dummy_tm();
        let s = state(45.0, 0.0);
        let f0 = 437_500_000.0;
        let expected = expected_carrier_hz(f0, 0.0);
        apply_physics_validation(
            &mut tm,
            &s,
            f0,
            RfMetadata {
                measured_carrier_hz: Some(expected + 200.0), // > 150 Hz
            },
            150.0,
            0.0,
        );
        assert!(tm.physics_flags & FLAG_DOPPLER_ANOMALY != 0);
    }

    #[test]
    fn below_horizon_sets_bit1() {
        let mut tm = dummy_tm();
        let s = state(-5.0, 0.0);
        apply_physics_validation(
            &mut tm,
            &s,
            437_500_000.0,
            RfMetadata::default(),
            150.0,
            0.0,
        );
        assert!(tm.physics_flags & FLAG_BELOW_HORIZON != 0);
        assert_eq!(tm.physics_flags & FLAG_DOPPLER_ANOMALY, 0);
    }

    #[test]
    fn elevation_at_horizon_not_flagged_when_minimum_is_zero() {
        let mut tm = dummy_tm();
        let s = state(0.0, 0.0);
        apply_physics_validation(&mut tm, &s, 437e6, RfMetadata::default(), 150.0, 0.0);
        assert_eq!(tm.physics_flags & FLAG_BELOW_HORIZON, 0);
    }

    #[test]
    fn combined_anomalies_set_both_bits() {
        let mut tm = dummy_tm();
        let s = state(-10.0, 0.0);
        let f0 = 100e6;
        apply_physics_validation(
            &mut tm,
            &s,
            f0,
            RfMetadata {
                measured_carrier_hz: Some(f0 + 500.0),
            },
            150.0,
            0.0,
        );
        assert_eq!(tm.physics_flags, FLAG_DOPPLER_ANOMALY | FLAG_BELOW_HORIZON);
    }

    #[test]
    fn no_measured_carrier_skips_doppler_even_if_would_be_bad() {
        let mut tm = dummy_tm();
        let s = state(30.0, 0.0);
        apply_physics_validation(&mut tm, &s, 100e6, RfMetadata::default(), 150.0, 0.0);
        assert_eq!(tm.physics_flags, 0);
    }

    #[test]
    fn independent_bits_doppler_only() {
        let mut tm = dummy_tm();
        let s = state(30.0, 0.0);
        let f0 = 100e6;
        apply_physics_validation(
            &mut tm,
            &s,
            f0,
            RfMetadata {
                measured_carrier_hz: Some(f0 + 500.0),
            },
            150.0,
            0.0,
        );
        assert_eq!(tm.physics_flags, FLAG_DOPPLER_ANOMALY);
    }

    #[test]
    fn nan_measured_skips_doppler_no_panic() {
        let mut tm = dummy_tm();
        let s = state(10.0, 1.0);
        apply_physics_validation(
            &mut tm,
            &s,
            437e6,
            RfMetadata {
                measured_carrier_hz: Some(f64::NAN),
            },
            150.0,
            0.0,
        );
        assert_eq!(tm.physics_flags, 0);
    }
}
