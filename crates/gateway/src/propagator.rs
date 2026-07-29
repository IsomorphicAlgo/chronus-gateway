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
//! Since Ephemerust 0.6.0, [`EphemerustPropagator`] holds an initialized
//! [`ephemerust::Propagator`]: the SGP4 element parsing and constants derivation happen
//! **once at construction**, and each per-frame [`OrbitalPropagator::tracking_state`] call is
//! a cheap propagation step (previously every call re-initialized SGP4 — twice, counting the
//! CV-4 illumination path). `ephemerust::Propagator` is `Send + Sync` with `&self` methods,
//! so this backend shares across Tokio workers without locking, as the trait requires.
//!
//! **CV-4:** [`TrackingState::nadir_sun_illum_cos`] is a nadir-fixed solar-array illumination
//! factor in \([0, 1]\) ([`nadir_sun_illumination_cos`]). Since Ephemerust 0.7.0 the eclipse
//! part is **real physics** — the conical umbra/penumbra model in [`ephemerust::eclipse`]
//! (apparent-disk overlap, Vallado §5.3) — replacing the in-house ray–sphere shadow toy; the
//! nadir-fixed panel-normal cosine remains a deliberately simple demo model (D-021/D-038).
//! Non-physics backends should set this to `NaN` so subsystem checks are skipped.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::{DateTime, Utc};
use ephemerust::eclipse::{ShadowState, shadow_state_from_vectors};
use ephemerust::{ObserverLocation, Propagator, Tle, sun_vector_km};

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

/// Nadir-fixed solar-array illumination factor for **subsystem co-validation (CV-4)**.
///
/// Uses the SGP4 position in **TEME** (km) and Ephemerust's geocentric Sun position vector
/// ([`sun_vector_km`], direction **and** distance). Combines:
///
/// 1. **Terminator / panel normal (toy):** `max(0, −û_sat · û_sun)` with `û_sat` the geocentric satellite
///    radial (nadir normal ≈ **−**`û_sat` for a nadir-pointing panel).
/// 2. **Eclipse (real physics since Ephemerust 0.7.0):** the conical umbra/penumbra model
///    ([`shadow_state_from_vectors`], apparent-disk-overlap test). **Umbra** zeroes the
///    factor; **penumbra** halves it, a first-order stand-in for the partially covered
///    solar disk during the seconds-long LEO crossing.
///
/// The panel model is still **not** flight-array fidelity; it exists for deterministic HIL
/// cross-checks. See `Methodology.md` **D-038** (amending **D-021**).
///
/// One-shot convenience: initializes an [`ephemerust::Propagator`] for a single evaluation.
/// Loops (like [`EphemerustPropagator`]) should build the propagator once and call
/// [`nadir_sun_illumination_cos_from`] instead.
#[must_use]
pub fn nadir_sun_illumination_cos(tle: &Tle, time: DateTime<Utc>) -> Option<f64> {
    let prop = Propagator::new(tle).ok()?;
    nadir_sun_illumination_cos_from(&prop, time)
}

/// Like [`nadir_sun_illumination_cos`], but reuses an already-initialized
/// [`ephemerust::Propagator`] so per-frame callers pay only a propagation step.
#[must_use]
pub fn nadir_sun_illumination_cos_from(prop: &Propagator, time: DateTime<Utc>) -> Option<f64> {
    let state = prop.propagate(time).ok()?;
    let r = state.position_km;
    let rn = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt();
    if !(rn.is_finite() && rn > 1.0) {
        return None;
    }
    let sun = sun_vector_km(time).ok()?;
    let sn = (sun[0] * sun[0] + sun[1] * sun[1] + sun[2] * sun[2]).sqrt();
    if !(sn.is_finite() && sn > 1.0) {
        return None;
    }

    // Toy nadir-panel cosine against the Sun direction.
    let u_sat = [r[0] / rn, r[1] / rn, r[2] / rn];
    let u_sun = [sun[0] / sn, sun[1] / sn, sun[2] / sn];
    let dot = u_sat[0] * u_sun[0] + u_sat[1] * u_sun[1] + u_sat[2] * u_sun[2];
    let cos_factor = (-dot).clamp(0.0, 1.0);

    // Real conical shadow geometry from Ephemerust's eclipse module.
    let illum = match shadow_state_from_vectors(r, sun) {
        ShadowState::Sunlit => cos_factor,
        ShadowState::Penumbra => 0.5 * cos_factor,
        ShadowState::Umbra => 0.0,
    };
    Some(illum)
}

/// Default SGP4 backend, driven by the `ephemerust` crate.
///
/// Holds an initialized [`ephemerust::Propagator`]: element parsing and SGP4 constants
/// derivation happen once in [`EphemerustPropagator::new`], so every subsequent
/// [`OrbitalPropagator::tracking_state`] call is a cheap propagation step. (Ephemerust's
/// benchmarks put initialization at ~72% of a one-shot propagation call — see its
/// `docs/rust-idioms.md` §1.)
pub struct EphemerustPropagator {
    propagator: Propagator,
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
        // Pay SGP4 initialization once, here; per-frame calls are then propagation steps.
        let propagator = Propagator::new(&tle)?;
        Ok(Self {
            propagator,
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
            if let Some((cached_at, state)) = cache.as_ref()
                && (time - *cached_at).num_milliseconds().abs() < self.min_interval_ms
            {
                return Ok(*state);
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
        let la = self.propagator.look_angles(time, observer)?;
        let nadir_sun_illum_cos = nadir_sun_illumination_cos_from(&self.propagator, time)
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
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

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
