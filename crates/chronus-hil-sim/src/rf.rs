//! Synthetic receiver chain for the HIL sim (Tranche 2, item 2.4).
//!
//! Produces `chronus.rf.v1` samples consistent with the gateway's own physics model at the
//! simulation's mission time, so the Doppler (bit 0), link-budget (bit 2) and pointing (bit 3)
//! checks can run — and be driven to alarm — through the real UDP binary.

use chrono::{DateTime, Duration, Utc};
use chronus_gateway::config::StationConfig;
use chronus_gateway::propagator::{OrbitalPropagator, propagator_from_station};
use chronus_gateway::rf::RfSampleV1;
use chronus_gateway::validate::ValidationContext;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Which receiver measurement is forced out of band (bits **0**, **2**, **3**).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RfFaultKind {
    /// Carrier offset of +2 kHz (T-DOPPLER default is ±150 Hz) → bit 0.
    Doppler,
    /// Received power +20 dB over the free-space prediction (default tolerance 3 dB) → bit 2.
    LinkBudget,
    /// Antenna azimuth off by 5° (default T-POINT is 0.25°) → bit 3.
    Pointing,
}

/// Inject an RF fault for frames **`start_frame` .. `start_frame + duration_frames`**.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RfScriptedAnomaly {
    /// Which measurement to corrupt.
    pub kind: RfFaultKind,
    /// First frame index (inclusive).
    pub start_frame: u32,
    /// Number of consecutive frames; `0` disables injection.
    pub duration_frames: u32,
}

/// In-place mutation when `frame_index` lies in the window. Returns `true` if `sample` changed.
pub fn apply_rf_anomaly(
    sample: &mut RfSampleV1,
    frame_index: u32,
    script: &RfScriptedAnomaly,
) -> bool {
    if script.duration_frames == 0 {
        return false;
    }
    let end = script.start_frame.saturating_add(script.duration_frames);
    if frame_index < script.start_frame || frame_index >= end {
        return false;
    }
    match script.kind {
        RfFaultKind::Doppler => {
            if let Some(c) = sample.carrier_hz.as_mut() {
                *c += 2_000.0;
            }
        }
        RfFaultKind::LinkBudget => {
            if let Some(p) = sample.rx_power_dbm.as_mut() {
                *p += 20.0;
            }
        }
        RfFaultKind::Pointing => {
            if let Some(a) = sample.az_deg.as_mut() {
                *a = (*a + 5.0).rem_euclid(360.0);
            }
        }
    }
    true
}

/// The simulated receiver: same station + propagator the gateway builds from its default config.
pub struct RfModel {
    propagator: Arc<dyn OrbitalPropagator>,
    ctx: ValidationContext,
}

impl RfModel {
    /// Builds the model from `station` (the gateway's defaults unless the operator overrides them).
    ///
    /// # Errors
    ///
    /// If the station's ephemeris cannot be loaded.
    pub fn from_station(station: &StationConfig) -> anyhow::Result<Self> {
        Ok(Self {
            propagator: propagator_from_station(station)?,
            ctx: ValidationContext::from_station(station),
        })
    }

    /// The consistent ("perfect receiver") sample at mission time `utc`, or `None` if the
    /// propagator cannot produce a state there.
    pub fn nominal(&self, utc: DateTime<Utc>) -> Option<RfSampleV1> {
        let state = self.propagator.tracking_state(utc).ok()?;
        Some(RfSampleV1::nominal_for(&self.ctx, &state))
    }

    /// Geometric elevation (degrees) at `utc`, if computable.
    pub fn elevation_deg(&self, utc: DateTime<Utc>) -> Option<f64> {
        self.propagator
            .tracking_state(utc)
            .ok()
            .map(|s| s.elevation_deg)
    }

    /// First instant at or after `from` (scanning up to `within`) when the spacecraft is at least
    /// `min_el_deg` above the horizon, in 30 s steps. Lets a demo start on a real pass.
    pub fn next_visible(
        &self,
        from: DateTime<Utc>,
        within: Duration,
        min_el_deg: f64,
    ) -> Option<DateTime<Utc>> {
        let mut t = from;
        let end = from + within;
        while t <= end {
            if self.elevation_deg(t).is_some_and(|e| e >= min_el_deg) {
                return Some(t);
            }
            t += Duration::seconds(30);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_hil_epoch_utc;

    fn sample() -> RfSampleV1 {
        RfSampleV1::new()
            .with_carrier_hz(437_500_000.0)
            .with_rx_power_dbm(-110.0)
            .with_pointing_deg(359.0, 20.0)
    }

    #[test]
    fn each_fault_touches_only_its_field_and_only_in_window() {
        for (kind, check) in [
            (
                RfFaultKind::Doppler,
                (|a: &RfSampleV1, b: &RfSampleV1| a.carrier_hz != b.carrier_hz)
                    as fn(&RfSampleV1, &RfSampleV1) -> bool,
            ),
            (RfFaultKind::LinkBudget, |a, b| {
                a.rx_power_dbm != b.rx_power_dbm
            }),
            (RfFaultKind::Pointing, |a, b| a.az_deg != b.az_deg),
        ] {
            let script = RfScriptedAnomaly {
                kind,
                start_frame: 2,
                duration_frames: 2,
            };
            let base = sample();
            for (i, expect) in [(1, false), (2, true), (3, true), (4, false)] {
                let mut s = base;
                assert_eq!(
                    apply_rf_anomaly(&mut s, i, &script),
                    expect,
                    "{kind:?} @{i}"
                );
                assert_eq!(check(&s, &base), expect, "{kind:?} @{i}");
            }
        }
    }

    #[test]
    fn pointing_fault_wraps_azimuth() {
        let mut s = sample();
        let script = RfScriptedAnomaly {
            kind: RfFaultKind::Pointing,
            start_frame: 0,
            duration_frames: 1,
        };
        apply_rf_anomaly(&mut s, 0, &script);
        assert!((s.az_deg.unwrap() - 4.0).abs() < 1e-9);
    }

    #[test]
    fn zero_duration_never_injects() {
        let mut s = sample();
        let script = RfScriptedAnomaly {
            kind: RfFaultKind::Doppler,
            start_frame: 0,
            duration_frames: 0,
        };
        assert!(!apply_rf_anomaly(&mut s, 0, &script));
        assert_eq!(s, sample());
    }

    #[test]
    fn model_is_self_consistent_and_finds_a_pass() {
        let station = StationConfig::default();
        let model = RfModel::from_station(&station).unwrap();
        let t = model
            .next_visible(default_hil_epoch_utc(), Duration::days(3), 10.0)
            .expect("ISS is visible from the default site within three days");
        let s = model.nominal(t).unwrap();
        assert!(s.carrier_hz.is_some() && s.rx_power_dbm.is_some() && s.az_deg.is_some());
        // Doppler at 437.5 MHz for an LEO pass stays within ±12 kHz.
        assert!((s.carrier_hz.unwrap() - 437_500_000.0).abs() < 12_000.0);
        assert!(model.elevation_deg(t).unwrap() >= 10.0);
    }
}
