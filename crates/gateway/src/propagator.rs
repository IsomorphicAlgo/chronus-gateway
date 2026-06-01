//! Astrodynamics abstraction for the Physics-Telemetry Co-Validation engine.
//!
//! The gateway never talks to a propagator library directly. Instead it depends on the
//! [`OrbitalPropagator`] trait, which yields the topocentric [`TrackingState`] (azimuth,
//! elevation, slant range, and **range rate**) needed to derive expected look-angles and the
//! Doppler-shifted carrier frequency for an incoming RF frame.
//!
//! Today the trait is backed by [`EphemerustPropagator`] (SGP4 via the `ephemerust` crate).
//! Keeping the network and validation pipelines behind this seam is what lets a future
//! high-fidelity backend (e.g. `nyx-space`) drop in without a rewrite. See `Methodology.md`
//! → "Trait-based astrodynamics (Ephemerust now, nyx-space later)".

use anyhow::Result;
use chrono::{DateTime, Utc};
use ephemerust::{look_angles, ObserverLocation, Tle};

/// Topocentric tracking state of a spacecraft relative to a fixed ground station.
///
/// `range_rate_km_s` is the line-of-sight velocity (negative while approaching) and is the
/// term consumed by the Doppler co-validation check.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct TrackingState {
    /// Azimuth in degrees, clockwise from true north.
    pub azimuth_deg: f64,
    /// Elevation above the local horizon in degrees.
    pub elevation_deg: f64,
    /// Slant range from station to spacecraft in kilometres.
    pub range_km: f64,
    /// Line-of-sight range rate in km/s (negative = approaching).
    pub range_rate_km_s: f64,
}

/// Decoupling boundary between the gateway and any astrodynamics backend.
///
/// Implementors must be `Send + Sync` so a single propagator can be shared (behind an `Arc`)
/// across the Tokio worker threads that service concurrent WebSocket clients.
pub trait OrbitalPropagator: Send + Sync {
    /// Computes the station-relative tracking state at `time`.
    fn tracking_state(&self, time: DateTime<Utc>) -> Result<TrackingState>;
}

/// Default SGP4 backend, driven by the `ephemerust` crate.
pub struct EphemerustPropagator {
    tle: Tle,
    latitude_deg: f64,
    longitude_deg: f64,
    altitude_m: f64,
}

impl EphemerustPropagator {
    /// Builds a propagator from a 2- or 3-line TLE and a fixed ground-station location.
    ///
    /// `latitude_deg`/`longitude_deg` are geodetic degrees (north/east positive);
    /// `altitude_m` is height above the WGS84 ellipsoid in metres.
    ///
    /// # Examples
    ///
    /// ```
    /// use chronus_gateway::propagator::{EphemerustPropagator, OrbitalPropagator};
    /// use chrono::{TimeZone, Utc};
    ///
    /// let iss = "ISS (ZARYA)\n\
    ///     1 25544U 98067A   20194.88612269 -.00002218  00000-0 -31515-4 0  9992\n\
    ///     2 25544  51.6461 221.2784 0001413  89.1723 280.4612 15.49507896236008";
    /// let prop = EphemerustPropagator::new(iss, 35.0, -116.0, 1000.0).unwrap();
    ///
    /// let t = Utc.with_ymd_and_hms(2020, 7, 12, 21, 0, 0).unwrap();
    /// let state = prop.tracking_state(t).unwrap();
    /// assert!(state.range_km > 0.0 && state.range_km.is_finite());
    /// ```
    pub fn new(tle_text: &str, latitude_deg: f64, longitude_deg: f64, altitude_m: f64) -> Result<Self> {
        let tle = Tle::parse(tle_text)?;
        Ok(Self { tle, latitude_deg, longitude_deg, altitude_m })
    }
}

impl OrbitalPropagator for EphemerustPropagator {
    fn tracking_state(&self, time: DateTime<Utc>) -> Result<TrackingState> {
        let observer = ObserverLocation {
            latitude: self.latitude_deg,
            longitude: self.longitude_deg,
            elevation: self.altitude_m,
        };
        let la = look_angles(&self.tle, time, observer)?;
        Ok(TrackingState {
            azimuth_deg: la.azimuth_deg,
            elevation_deg: la.elevation_deg,
            range_km: la.range_km,
            range_rate_km_s: la.range_rate_km_s,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    const ISS_TLE: &str = "ISS (ZARYA)\n\
        1 25544U 98067A   20194.88612269 -.00002218  00000-0 -31515-4 0  9992\n\
        2 25544  51.6461 221.2784 0001413  89.1723 280.4612 15.49507896236008";

    fn epoch() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2020, 7, 12, 21, 0, 0).single().unwrap()
    }

    #[test]
    fn tracking_state_is_finite_near_epoch() {
        let prop = EphemerustPropagator::new(ISS_TLE, 35.0, -116.0, 1000.0).expect("valid TLE");
        let s = prop.tracking_state(epoch()).expect("propagation succeeds");

        assert!(s.range_km.is_finite() && s.range_km > 0.0, "range_km = {}", s.range_km);
        assert!((0.0..=360.0).contains(&s.azimuth_deg), "azimuth = {}", s.azimuth_deg);
        assert!((-90.0..=90.0).contains(&s.elevation_deg), "elevation = {}", s.elevation_deg);
        assert!(s.range_rate_km_s.is_finite(), "range_rate = {}", s.range_rate_km_s);
    }

    #[test]
    fn invalid_tle_is_rejected() {
        let result = EphemerustPropagator::new("definitely not a TLE", 0.0, 0.0, 0.0);
        assert!(result.is_err(), "garbage TLE text must not parse");
    }
}
