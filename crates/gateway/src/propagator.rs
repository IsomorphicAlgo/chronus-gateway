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

use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::{DateTime, Utc};
use ephemerust::{look_angles, ObserverLocation, Tle};

use crate::config::StationConfig;

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

    /// Builds a propagator from a validated [`StationConfig`], resolving its TLE source.
    pub fn from_station(config: &StationConfig) -> Result<Self> {
        config.validate()?;
        let tle_text = config.resolve_tle_text()?;
        Self::new(
            &tle_text,
            config.latitude_deg,
            config.longitude_deg,
            config.altitude_m,
        )
    }
}

/// A shareable, throttled front-end over an [`OrbitalPropagator`].
///
/// Caches the most recent `(time, state)` and reuses it for any request within
/// `min_interval_ms` of the cached instant, so a burst of frames does not trigger redundant SGP4
/// propagations (the look-angle recompute throttle, e.g. 100 Hz). Safe to share across the Tokio
/// worker threads that service concurrent clients.
pub struct TrackingProvider {
    propagator: Arc<dyn OrbitalPropagator>,
    min_interval_ms: i64,
    last: Mutex<Option<(DateTime<Utc>, TrackingState)>>,
}

impl TrackingProvider {
    /// Wraps `propagator`, reusing a cached state for requests within `min_interval_ms`
    /// (`0` disables caching).
    pub fn new(propagator: Arc<dyn OrbitalPropagator>, min_interval_ms: u64) -> Self {
        Self {
            propagator,
            min_interval_ms: min_interval_ms as i64,
            last: Mutex::new(None),
        }
    }

    /// Returns the tracking state at `time`, served from cache when within the throttle window.
    pub fn tracking_state(&self, time: DateTime<Utc>) -> Result<TrackingState> {
        {
            let cache = self.last.lock().expect("tracking cache mutex poisoned");
            if let Some((cached_at, state)) = cache.as_ref() {
                if (time - *cached_at).num_milliseconds().abs() < self.min_interval_ms {
                    return Ok(*state);
                }
            }
        }
        // Compute outside the lock so SGP4 work never serializes other callers.
        let state = self.propagator.tracking_state(time)?;
        let mut cache = self.last.lock().expect("tracking cache mutex poisoned");
        *cache = Some((time, state));
        Ok(state)
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

    #[test]
    fn from_station_is_deterministic_and_in_tolerance() {
        use crate::config::{StationConfig, TleSource};

        let station = StationConfig {
            latitude_deg: 35.0,
            longitude_deg: -116.0,
            altitude_m: 1000.0,
            nominal_carrier_hz: 437_500_000.0,
            tle: TleSource::Inline(ISS_TLE.to_string()),
            min_recompute_interval_ms: 0,
            ..Default::default()
        };
        let prop = EphemerustPropagator::from_station(&station).expect("build from station");

        let a = prop.tracking_state(epoch()).expect("state");
        let b = prop.tracking_state(epoch()).expect("state again");
        assert_eq!(a.range_km, b.range_km, "propagation must be deterministic");

        // Baseline locked from the foundation smoke run (same TLE/station/epoch).
        assert!((a.range_km - 9134.98).abs() < 1.0, "range_km = {}", a.range_km);
        assert!((a.elevation_deg - (-42.07)).abs() < 0.5, "elevation = {}", a.elevation_deg);
        assert!((a.azimuth_deg - 141.70).abs() < 0.5, "azimuth = {}", a.azimuth_deg);
    }

    #[test]
    fn provider_uses_mock_and_throttles_recompute() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;

        // A scripted, counting propagator proves the trait seam and lets us observe caching.
        struct CountingPropagator {
            calls: AtomicU64,
            state: TrackingState,
        }
        impl OrbitalPropagator for CountingPropagator {
            fn tracking_state(&self, _time: chrono::DateTime<Utc>) -> Result<TrackingState> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                Ok(self.state)
            }
        }

        let scripted = TrackingState {
            azimuth_deg: 10.0,
            elevation_deg: 20.0,
            range_km: 30.0,
            range_rate_km_s: 0.5,
        };
        let counting = Arc::new(CountingPropagator { calls: AtomicU64::new(0), state: scripted });
        let provider = TrackingProvider::new(counting.clone(), 100); // 100 ms throttle

        let t0 = epoch();
        let first = provider.tracking_state(t0).expect("first");
        assert_eq!(first.range_km, scripted.range_km, "provider returns the backend's state");

        // Within the throttle window → served from cache, no extra propagation.
        provider.tracking_state(t0 + chrono::Duration::milliseconds(50)).expect("cached");
        assert_eq!(counting.calls.load(Ordering::Relaxed), 1);

        // Beyond the window → recompute.
        provider.tracking_state(t0 + chrono::Duration::milliseconds(200)).expect("recompute");
        assert_eq!(counting.calls.load(Ordering::Relaxed), 2);
    }
}
