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
//!
//! **CV-4:** [`TrackingState::nadir_sun_illum_cos`] is a **toy** nadir-fixed solar-array
//! illumination factor in \([0, 1]\) from geocentric TEME geometry + Ephemerust’s low-precision
//! Sun vector ([`nadir_sun_illumination_cos`]). Non-physics backends should set this to `NaN` so
//! subsystem checks are skipped.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::{DateTime, Utc};
use ephemerust::celestial::{calculate_position, CelestialObject};
use ephemerust::{look_angles, propagate, ObserverLocation, Tle};

use crate::config::StationConfig;

/// WGS84 equatorial Earth radius (km); occultation uses a **spherical** Earth (CV-4 toy).
const EARTH_EQUATORIAL_RADIUS_KM: f64 = 6378.137;

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
    /// Toy nadir-array Sun illumination in \([0, 1]\); `NaN` = unavailable (**CV-4**).
    pub nadir_sun_illum_cos: f64,
}

/// Decoupling boundary between the gateway and any astrodynamics backend.
///
/// Implementors must be `Send + Sync` so a single propagator can be shared (behind an `Arc`)
/// across the Tokio worker threads that service concurrent WebSocket clients.
pub trait OrbitalPropagator: Send + Sync {
    /// Computes the station-relative tracking state at `time`.
    fn tracking_state(&self, time: DateTime<Utc>) -> Result<TrackingState>;
}

/// Unit vector in **geocentric equatorial** coordinates (TEME-as-inertial) from right ascension
/// (hours) and declination (degrees), matching Ephemerust’s Sun pipeline conventions.
fn equatorial_dir_from_ra_dec_hours(ra_hours: f64, dec_deg: f64) -> Option<[f64; 3]> {
    if !(ra_hours.is_finite() && dec_deg.is_finite()) {
        return None;
    }
    let ra_rad = ra_hours * (std::f64::consts::PI / 12.0);
    let dec_rad = dec_deg.to_radians();
    let cd = dec_rad.cos();
    let sd = dec_rad.sin();
    let (sr, cr) = ra_rad.sin_cos();
    let x = cd * cr;
    let y = cd * sr;
    let z = sd;
    if x.is_finite() && y.is_finite() && z.is_finite() {
        Some([x, y, z])
    } else {
        None
    }
}

/// `true` if the half-line **sat → Sun** (direction `u_sun`, geocentric, unit) first intersects the
/// solid Earth sphere before reaching open space — toy cylindrical / ray–sphere umbra (**CV-4**).
fn sun_ray_intersects_earth_sphere(r_sat_km: [f64; 3], u_sun: [f64; 3]) -> bool {
    let rd = r_sat_km[0] * u_sun[0] + r_sat_km[1] * u_sun[1] + r_sat_km[2] * u_sun[2];
    let r2 = r_sat_km[0] * r_sat_km[0] + r_sat_km[1] * r_sat_km[1] + r_sat_km[2] * r_sat_km[2];
    if r2 < EARTH_EQUATORIAL_RADIUS_KM * EARTH_EQUATORIAL_RADIUS_KM * 0.99 {
        // Inside or grazing the Earth interior — treat as no Sun for the toy model.
        return true;
    }
    let c = r2 - EARTH_EQUATORIAL_RADIUS_KM * EARTH_EQUATORIAL_RADIUS_KM;
    let disc = rd * rd - c;
    if disc <= 0.0 {
        return false;
    }
    let s = disc.sqrt();
    let t0 = -rd - s;
    let t1 = -rd + s;
    let mut t_hit = f64::INFINITY;
    for t in [t0, t1] {
        if t > 1e-6 {
            t_hit = t_hit.min(t);
        }
    }
    t_hit.is_finite() && t_hit < f64::INFINITY
}

/// Nadir-fixed solar-array illumination factor for **subsystem co-validation (CV-4)**.
///
/// Uses SGP4 [`propagate`] position in **TEME** (km) and Ephemerust’s low-precision geocentric Sun
/// direction from [`calculate_position`](ephemerust::celestial::calculate_position) (see
/// `Methodology.md` **D-021**). Combines:
///
/// 1. **Terminator / panel normal:** `max(0, −û_sat · û_sun)` with `û_sat` the geocentric satellite
///    radial (nadir normal ≈ **−**`û_sat` for a nadir-pointing panel).
/// 2. **Toy eclipse:** zero illumination if the Sun ray from the satellite intersects the Earth
///    sphere (spherical WGS84 equatorial radius).
///
/// This is **not** flight-array or umbra-penumbra fidelity; it exists for deterministic HIL
/// cross-checks only.
#[must_use]
pub fn nadir_sun_illumination_cos(tle: &Tle, time: DateTime<Utc>) -> Option<f64> {
    let state = propagate(tle, time).ok()?;
    let r = state.position_km;
    let r2 = r[0] * r[0] + r[1] * r[1] + r[2] * r[2];
    let rn = r2.sqrt();
    if !(rn.is_finite() && rn > 1.0) {
        return None;
    }
    let ra_dec = calculate_position(CelestialObject::Sun, time).ok()?;
    let u_sun = equatorial_dir_from_ra_dec_hours(ra_dec.ra, ra_dec.dec)?;
    let u_sat = [r[0] / rn, r[1] / rn, r[2] / rn];
    let dot = u_sat[0] * u_sun[0] + u_sat[1] * u_sun[1] + u_sat[2] * u_sun[2];
    let mut illum = (-dot).clamp(0.0, 1.0);
    if sun_ray_intersects_earth_sphere(r, u_sun) {
        illum = 0.0;
    }
    Some(illum)
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
    pub fn new(
        tle_text: &str,
        latitude_deg: f64,
        longitude_deg: f64,
        altitude_m: f64,
    ) -> Result<Self> {
        let tle = Tle::parse(tle_text)?;
        Ok(Self {
            tle,
            latitude_deg,
            longitude_deg,
            altitude_m,
        })
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
        let nadir_sun_illum_cos = nadir_sun_illumination_cos(&self.tle, time)
            .filter(|x| x.is_finite())
            .unwrap_or(f64::NAN);
        Ok(TrackingState {
            azimuth_deg: la.azimuth_deg,
            elevation_deg: la.elevation_deg,
            range_km: la.range_km,
            range_rate_km_s: la.range_rate_km_s,
            nadir_sun_illum_cos,
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
        Utc.with_ymd_and_hms(2020, 7, 12, 21, 0, 0)
            .single()
            .unwrap()
    }

    #[test]
    fn tracking_state_is_finite_near_epoch() {
        let prop = EphemerustPropagator::new(ISS_TLE, 35.0, -116.0, 1000.0).expect("valid TLE");
        let s = prop.tracking_state(epoch()).expect("propagation succeeds");

        assert!(
            s.nadir_sun_illum_cos.is_finite()
                && s.nadir_sun_illum_cos >= 0.0
                && s.nadir_sun_illum_cos <= 1.0,
            "illum = {}",
            s.nadir_sun_illum_cos
        );
        assert!(
            s.range_km.is_finite() && s.range_km > 0.0,
            "range_km = {}",
            s.range_km
        );
        assert!(
            (0.0..=360.0).contains(&s.azimuth_deg),
            "azimuth = {}",
            s.azimuth_deg
        );
        assert!(
            (-90.0..=90.0).contains(&s.elevation_deg),
            "elevation = {}",
            s.elevation_deg
        );
        assert!(
            s.range_rate_km_s.is_finite(),
            "range_rate = {}",
            s.range_rate_km_s
        );
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
        assert!(
            (a.range_km - 9134.98).abs() < 1.0,
            "range_km = {}",
            a.range_km
        );
        assert!(
            (a.elevation_deg - (-42.07)).abs() < 0.5,
            "elevation = {}",
            a.elevation_deg
        );
        assert!(
            (a.azimuth_deg - 141.70).abs() < 0.5,
            "azimuth = {}",
            a.azimuth_deg
        );
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
            nadir_sun_illum_cos: f64::NAN,
        };
        let counting = Arc::new(CountingPropagator {
            calls: AtomicU64::new(0),
            state: scripted,
        });
        let provider = TrackingProvider::new(counting.clone(), 100); // 100 ms throttle

        let t0 = epoch();
        let first = provider.tracking_state(t0).expect("first");
        assert_eq!(
            first.range_km, scripted.range_km,
            "provider returns the backend's state"
        );

        // Within the throttle window → served from cache, no extra propagation.
        provider
            .tracking_state(t0 + chrono::Duration::milliseconds(50))
            .expect("cached");
        assert_eq!(counting.calls.load(Ordering::Relaxed), 1);

        // Beyond the window → recompute.
        provider
            .tracking_state(t0 + chrono::Duration::milliseconds(200))
            .expect("recompute");
        assert_eq!(counting.calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn nadir_sun_illumination_cos_is_deterministic() {
        let tle = Tle::parse(
            "1 25544U 98067A   20194.88612269 -.00002218  00000-0 -31515-4 0  9992\n\
             2 25544  51.6461 221.2784 0001413  89.1723 280.4612 15.49507896236008",
        )
        .expect("parse");
        let t = epoch();
        let a = nadir_sun_illumination_cos(&tle, t).expect("illum");
        let b = nadir_sun_illumination_cos(&tle, t).expect("illum again");
        assert_eq!(a, b);
        assert!((0.0..=1.0).contains(&a), "a={a}");
    }
}
