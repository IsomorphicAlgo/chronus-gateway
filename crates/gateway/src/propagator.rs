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
//!
//! ## Station model (D-042, D-043)
//!
//! Both backends share a [`StationModel`]: the station's WGS84 location, its **Earth
//! orientation parameters** (UT1−UTC and polar motion, so the Earth-fixed frame is where the
//! Earth actually is that day), and optional standard-atmosphere **refraction**, which yields
//! [`TrackingState::apparent_elevation_deg`] — where a real antenna points.
//!
//! ## TLE-age Doppler margin (D-044)
//!
//! A TLE's along-track error grows with age; a time-like error δt = δs / |v| shifts the
//! predicted range rate by ρ̈·δt. When enabled, [`EphemerustPropagator`] reports that bound as
//! [`TrackingState::range_rate_uncertainty_km_s`] and the Doppler check widens its tolerance by the
//! matching frequency (see [`crate::validate`]).

use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::{DateTime, Utc};
use ephemerust::eclipse::{ShadowState, shadow_state_from_vectors};
use ephemerust::frames::{InertialFrame, itrf_to_inertial};
use ephemerust::{EarthOrientation, LookAngles, ObserverLocation, Propagator, Tle, sun_vector_km};

use crate::config::StationConfig;

/// Topocentric tracking state of a spacecraft relative to a fixed ground station.
///
/// `range_rate_km_s` is the line-of-sight velocity (negative while approaching) and is the
/// term consumed by the Doppler co-validation check.
///
/// The struct is `#[non_exhaustive]` so new physics fields can be added without breaking custom
/// [`OrbitalPropagator`] backends: build it with [`TrackingState::new`] and the `with_*` methods.
///
/// ```
/// use chronus_gateway::propagator::TrackingState;
///
/// let s = TrackingState::new(120.0, 30.0, 900.0, -3.2).with_apparent_elevation(30.03);
/// assert_eq!(s.pointing_elevation_deg(), 30.03);
/// assert!(s.nadir_sun_illum_cos.is_nan()); // unavailable unless a backend sets it
/// assert_eq!(s.range_rate_uncertainty_km_s, 0.0);
/// ```
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[non_exhaustive]
pub struct TrackingState {
    /// Azimuth in degrees, clockwise from true north.
    pub azimuth_deg: f64,
    /// **Geometric** elevation above the local horizon in degrees (no refraction). Used by the
    /// horizon check (bit 1).
    pub elevation_deg: f64,
    /// Slant range from station to spacecraft in kilometres.
    pub range_km: f64,
    /// Line-of-sight range rate in km/s (negative = approaching).
    pub range_rate_km_s: f64,
    /// Toy nadir-array Sun illumination in \([0, 1]\); `NaN` = unavailable (**CV-4**).
    pub nadir_sun_illum_cos: f64,
    /// **Apparent** elevation (geometric + atmospheric refraction) in degrees — where an antenna
    /// tracking the signal actually points. `NaN` when refraction is not modelled; the pointing
    /// check (bit 3) then uses [`Self::elevation_deg`].
    pub apparent_elevation_deg: f64,
    /// One-sided bound on the error of [`Self::range_rate_km_s`] caused by the ephemeris (km/s),
    /// e.g. TLE age (D-044). `0` when not modelled. The Doppler check widens its tolerance by the
    /// corresponding frequency.
    pub range_rate_uncertainty_km_s: f64,
}

impl TrackingState {
    /// Geometric tracking state; optional fields start unavailable (`NaN`) or zero.
    #[must_use]
    pub fn new(azimuth_deg: f64, elevation_deg: f64, range_km: f64, range_rate_km_s: f64) -> Self {
        Self {
            azimuth_deg,
            elevation_deg,
            range_km,
            range_rate_km_s,
            nadir_sun_illum_cos: f64::NAN,
            apparent_elevation_deg: f64::NAN,
            range_rate_uncertainty_km_s: 0.0,
        }
    }

    /// Sets the CV-4 nadir-array Sun illumination factor.
    #[must_use]
    pub fn with_sun_illumination(mut self, nadir_sun_illum_cos: f64) -> Self {
        self.nadir_sun_illum_cos = nadir_sun_illum_cos;
        self
    }

    /// Sets the apparent (refracted) elevation.
    #[must_use]
    pub fn with_apparent_elevation(mut self, apparent_elevation_deg: f64) -> Self {
        self.apparent_elevation_deg = apparent_elevation_deg;
        self
    }

    /// Sets the ephemeris-induced range-rate uncertainty (km/s).
    #[must_use]
    pub fn with_range_rate_uncertainty(mut self, km_s: f64) -> Self {
        self.range_rate_uncertainty_km_s = km_s;
        self
    }

    /// Elevation an antenna should point at: apparent if refraction is modelled, else geometric.
    #[must_use]
    pub fn pointing_elevation_deg(&self) -> f64 {
        if self.apparent_elevation_deg.is_finite() {
            self.apparent_elevation_deg
        } else {
            self.elevation_deg
        }
    }
}

/// Ground-station model shared by every backend: location, Earth orientation and refraction.
///
/// ```
/// use chronus_gateway::config::StationConfig;
/// use chronus_gateway::propagator::StationModel;
///
/// let m = StationModel::from_station(&StationConfig::default()).unwrap();
/// // Refraction is on by default: ~0.48° lift at the horizon, none at the zenith.
/// assert!((m.apparent_elevation_deg(0.0) - 0.48).abs() < 0.01);
/// assert!((m.apparent_elevation_deg(90.0) - 90.0).abs() < 1e-6);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct StationModel {
    /// Geodetic latitude, degrees north.
    pub latitude_deg: f64,
    /// Geodetic longitude, degrees east.
    pub longitude_deg: f64,
    /// Height above the WGS84 ellipsoid, metres.
    pub altitude_m: f64,
    /// Earth orientation parameters for the ITRF rotation.
    pub earth_orientation: EarthOrientation,
    /// Refraction conditions `(pressure_hpa, temperature_c)`; `None` = geometric only.
    pub refraction: Option<(f64, f64)>,
}

impl StationModel {
    /// Geometric station (no Earth orientation, no refraction): the 0.1 behaviour.
    #[must_use]
    pub fn geometric(latitude_deg: f64, longitude_deg: f64, altitude_m: f64) -> Self {
        Self {
            latitude_deg,
            longitude_deg,
            altitude_m,
            earth_orientation: EarthOrientation::default(),
            refraction: None,
        }
    }

    /// Station model from a validated [`StationConfig`].
    ///
    /// # Errors
    ///
    /// Propagates [`StationConfig::validate`] failures.
    pub fn from_station(config: &StationConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            latitude_deg: config.latitude_deg,
            longitude_deg: config.longitude_deg,
            altitude_m: config.altitude_m,
            earth_orientation: config.earth_orientation()?,
            refraction: config.refraction.then_some((
                config.refraction_pressure_hpa,
                config.refraction_temperature_c,
            )),
        })
    }

    /// Ephemerust observer for look-angle calls.
    #[must_use]
    pub fn observer(&self) -> ObserverLocation {
        ObserverLocation {
            latitude: self.latitude_deg,
            longitude: self.longitude_deg,
            elevation: self.altitude_m,
        }
    }

    /// Geometric elevation → apparent elevation (identity when refraction is off).
    #[must_use]
    pub fn apparent_elevation_deg(&self, geometric_deg: f64) -> f64 {
        match self.refraction {
            Some((p, t)) => {
                geometric_deg
                    + ephemerust::coordinates::refraction_deg_with_conditions(geometric_deg, p, t)
            }
            None => geometric_deg,
        }
    }

    /// Builds a [`TrackingState`] from look angles, adding apparent elevation when modelled.
    #[must_use]
    pub fn tracking_state(&self, la: &LookAngles) -> TrackingState {
        let mut s = TrackingState::new(
            la.azimuth_deg,
            la.elevation_deg,
            la.range_km,
            la.range_rate_km_s,
        );
        if self.refraction.is_some() {
            s = s.with_apparent_elevation(self.apparent_elevation_deg(la.elevation_deg));
        }
        s
    }
}

/// TLE along-track error model for the Doppler margin (D-044): `δs = at_epoch + growth × |age|`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TleErrorModel {
    /// Along-track error at the TLE epoch, km.
    pub km_at_epoch: f64,
    /// Growth of along-track error per day of TLE age, km/day.
    pub growth_km_per_day: f64,
}

impl TleErrorModel {
    /// Along-track error (km) for a TLE `age_days` old (either direction).
    #[must_use]
    pub fn along_track_error_km(&self, age_days: f64) -> f64 {
        self.km_at_epoch + self.growth_km_per_day * age_days.abs()
    }
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
    nadir_sun_illumination_cos_teme(state.position_km, time)
}

/// The CV-4 illumination model for a satellite **TEME** position (km) — the backend-independent
/// core of [`nadir_sun_illumination_cos_from`], also used by the OEM backend after rotating its
/// state to TEME.
#[must_use]
pub fn nadir_sun_illumination_cos_teme(r: [f64; 3], time: DateTime<Utc>) -> Option<f64> {
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
    tle_epoch: DateTime<Utc>,
    station: StationModel,
    tle_error: Option<TleErrorModel>,
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
            tle_epoch: tle.epoch,
            station: StationModel::geometric(latitude_deg, longitude_deg, altitude_m),
            tle_error: None,
        })
    }

    /// Builds a propagator from a validated [`StationConfig`], resolving its TLE source and
    /// applying its Earth orientation, refraction and TLE-age Doppler settings.
    pub fn from_station(config: &StationConfig) -> Result<Self> {
        let station = StationModel::from_station(config)?;
        let tle_text = config.resolve_tle_text()?;
        let mut prop = Self::new(
            &tle_text,
            config.latitude_deg,
            config.longitude_deg,
            config.altitude_m,
        )?
        .with_station_model(station);
        if config.doppler_tle_age_margin {
            prop = prop.with_tle_error_model(TleErrorModel {
                km_at_epoch: config.tle_error_km_at_epoch,
                growth_km_per_day: config.tle_error_growth_km_per_day,
            });
        }
        Ok(prop)
    }

    /// Replaces the station model (location, Earth orientation, refraction).
    #[must_use]
    pub fn with_station_model(mut self, station: StationModel) -> Self {
        self.station = station;
        self
    }

    /// Enables the TLE-age Doppler margin (D-044) with the given along-track error model.
    #[must_use]
    pub fn with_tle_error_model(mut self, model: TleErrorModel) -> Self {
        self.tle_error = Some(model);
        self
    }

    /// Epoch of the loaded TLE.
    #[must_use]
    pub fn tle_epoch(&self) -> DateTime<Utc> {
        self.tle_epoch
    }

    fn look_angles(&self, time: DateTime<Utc>) -> Result<LookAngles> {
        Ok(self.propagator.look_angles_with_eop(
            time,
            self.station.observer(),
            &self.station.earth_orientation,
        )?)
    }

    /// Range-rate error (km/s) implied by along-track TLE error: δt = δs / |v| shifts the
    /// predicted range rate by ρ̈·δt. ρ̈ comes from a ±1 s central difference of range rate.
    fn range_rate_uncertainty_km_s(
        &self,
        time: DateTime<Utc>,
        model: &TleErrorModel,
    ) -> Result<f64> {
        let h = chrono::Duration::seconds(1);
        let rr_plus = self.look_angles(time + h)?.range_rate_km_s;
        let rr_minus = self.look_angles(time - h)?.range_rate_km_s;
        let range_accel_km_s2 = (rr_plus - rr_minus) / 2.0;
        let v = self.propagator.propagate(time)?.velocity_km_s;
        let speed_km_s = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        let age_days = (time - self.tle_epoch).num_milliseconds() as f64 / 86_400_000.0;
        let ds_km = model.along_track_error_km(age_days);
        Ok(range_accel_km_s2.abs() * ds_km / speed_km_s.max(1e-3))
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
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use chrono::{Duration, TimeZone, Utc};
    /// use chronus_gateway::config::DEFAULT_ISS_TLE;
    /// use chronus_gateway::propagator::{EphemerustPropagator, TrackingProvider};
    ///
    /// let prop = EphemerustPropagator::new(DEFAULT_ISS_TLE, 35.0, -116.0, 1000.0).unwrap();
    /// let provider = TrackingProvider::new(Arc::new(prop), 10);
    ///
    /// let t = Utc.with_ymd_and_hms(2020, 7, 12, 21, 0, 0).unwrap();
    /// let a = provider.tracking_state(t).unwrap();
    /// // 5 ms later is inside the 10 ms window: served from cache, identical result.
    /// let b = provider.tracking_state(t + Duration::milliseconds(5)).unwrap();
    /// assert_eq!(a.range_km, b.range_km);
    /// ```
    pub fn new(propagator: Arc<dyn OrbitalPropagator>, min_interval_ms: u64) -> Self {
        Self {
            propagator,
            min_interval_ms: min_interval_ms as i64,
            last: Mutex::new(None),
        }
    }

    /// Returns the tracking state at `time`, served from cache when within the throttle window.
    ///
    /// Poison-tolerant locking (**F-1**, `docs/RUST_MISSION_READY.md`): both critical sections
    /// only read/write a `Copy` value, so a panic while holding the lock is unreachable — but if
    /// one ever occurred, the cache content would still be a valid (at worst stale) state, so the
    /// poison flag is safely ignored rather than panicking the telemetry hot path.
    pub fn tracking_state(&self, time: DateTime<Utc>) -> Result<TrackingState> {
        {
            let cache = self
                .last
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((cached_at, state)) = cache.as_ref()
                && (time - *cached_at).num_milliseconds().abs() < self.min_interval_ms
            {
                return Ok(*state);
            }
        }
        // Compute outside the lock so SGP4 work never serializes other callers.
        let state = self.propagator.tracking_state(time)?;
        let mut cache = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *cache = Some((time, state));
        Ok(state)
    }
}

impl OrbitalPropagator for EphemerustPropagator {
    fn tracking_state(&self, time: DateTime<Utc>) -> Result<TrackingState> {
        let la = self.look_angles(time)?;
        let nadir_sun_illum_cos = nadir_sun_illumination_cos_from(&self.propagator, time)
            .filter(|x| x.is_finite())
            .unwrap_or(f64::NAN);
        let mut state = self
            .station
            .tracking_state(&la)
            .with_sun_illumination(nadir_sun_illum_cos);
        if let Some(model) = &self.tle_error {
            state =
                state.with_range_rate_uncertainty(self.range_rate_uncertainty_km_s(time, model)?);
        }
        Ok(state)
    }
}

/// Builds the tracking backend a [`StationConfig`] asks for: the CCSDS **OEM** backend when
/// `oem_file` is set ([`crate::oem::OemPropagator`]), otherwise SGP4 from the TLE
/// ([`EphemerustPropagator`]).
///
/// # Errors
///
/// Configuration, TLE or OEM loading failures, explainable with
/// [`crate::diagnose::explain_tracking_error`].
pub fn propagator_from_station(config: &StationConfig) -> Result<Arc<dyn OrbitalPropagator>> {
    if config.oem_file.is_some() {
        Ok(Arc::new(crate::oem::OemPropagator::from_station(config)?))
    } else {
        Ok(Arc::new(EphemerustPropagator::from_station(config)?))
    }
}

/// Rotates an Earth-fixed (ITRF) position to TEME, for the CV-4 illumination model.
pub(crate) fn itrf_to_teme_position(
    r_itrf_km: [f64; 3],
    time: DateTime<Utc>,
    eop: &EarthOrientation,
) -> [f64; 3] {
    itrf_to_inertial(InertialFrame::Teme, time, r_itrf_km, [0.0; 3], eop).0
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

    /// **T-RANGERATE** (`Methodology.md` D-041): the Doppler input must equal the time
    /// derivative of slant range. A 0.5 s central difference over three hours of ISS geometry
    /// (above and below the horizon) bounds the disagreement at 1 m/s, about 1.5 Hz at
    /// 437.5 MHz. Ephemerust 0.7 failed this by up to ~0.3 km/s (an Earth-rotation sign error
    /// fixed in Ephemerust 1.0).
    #[test]
    fn range_rate_matches_numerical_derivative_of_range() {
        let prop = EphemerustPropagator::new(ISS_TLE, 47.9088, -122.2503, 0.0).expect("valid TLE");
        let half = chrono::Duration::milliseconds(500);
        let mut worst_m_s = 0.0_f64;
        for k in 0..90 {
            let t = epoch() + chrono::Duration::minutes(2 * k);
            let s = prop.tracking_state(t).expect("state");
            let r_plus = prop
                .tracking_state(t + half)
                .expect("state at t+0.5 s")
                .range_km;
            let r_minus = prop
                .tracking_state(t - half)
                .expect("state at t-0.5 s")
                .range_km;
            let numeric_km_s = r_plus - r_minus; // over a 1 s baseline
            worst_m_s = worst_m_s.max((numeric_km_s - s.range_rate_km_s).abs() * 1000.0);
        }
        assert!(
            worst_m_s < 1.0,
            "range_rate vs d(range)/dt differs by {worst_m_s:.3} m/s (T-RANGERATE bound 1 m/s)"
        );
    }

    /// D-044: the reported range-rate uncertainty must match the change in range rate that an
    /// along-track error actually causes. Shift the satellite by δs along track (a time shift
    /// δt = δs/|v|) and compare |Δρ̇| with the model, around a real pass.
    #[test]
    fn tle_age_uncertainty_matches_an_along_track_shift() {
        let model = TleErrorModel {
            km_at_epoch: 5.0,
            growth_km_per_day: 0.0,
        };
        let prop = EphemerustPropagator::new(ISS_TLE, 47.9088, -122.2503, 0.0)
            .unwrap()
            .with_tle_error_model(model);
        let plain = EphemerustPropagator::new(ISS_TLE, 47.9088, -122.2503, 0.0).unwrap();
        let mut checked = 0;
        // Sample a full day at 20 s so several passes over Everett are included.
        for k in 0..4320 {
            let t = epoch() + chrono::Duration::seconds(20 * k);
            let s = prop.tracking_state(t).unwrap();
            if s.elevation_deg < 10.0 {
                continue;
            }
            let v = prop.propagator.propagate(t).unwrap().velocity_km_s;
            let speed = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            let dt = chrono::Duration::milliseconds((5.0 / speed * 1000.0) as i64);
            let actual =
                (plain.tracking_state(t + dt).unwrap().range_rate_km_s - s.range_rate_km_s).abs();
            let predicted = s.range_rate_uncertainty_km_s;
            // First-order model: agree within 10 % (+1 m/s floor for near-zero ρ̈).
            assert!(
                (actual - predicted).abs() < 0.1 * predicted + 0.001,
                "t+{}s: actual {actual:.5} km/s vs predicted {predicted:.5} km/s",
                20 * k
            );
            checked += 1;
        }
        assert!(
            checked > 20,
            "pass must be sampled (got {checked} points above 10°)"
        );
    }

    #[test]
    fn tle_age_uncertainty_grows_with_age_and_is_off_by_default() {
        let model = TleErrorModel {
            km_at_epoch: 1.0,
            growth_km_per_day: 2.0,
        };
        assert_eq!(model.along_track_error_km(0.0), 1.0);
        assert_eq!(model.along_track_error_km(-3.0), 7.0);
        let plain = EphemerustPropagator::new(ISS_TLE, 47.9088, -122.2503, 0.0).unwrap();
        assert_eq!(
            plain
                .tracking_state(epoch())
                .unwrap()
                .range_rate_uncertainty_km_s,
            0.0
        );
        let aged = EphemerustPropagator::new(ISS_TLE, 47.9088, -122.2503, 0.0)
            .unwrap()
            .with_tle_error_model(model);
        let near = aged
            .tracking_state(epoch())
            .unwrap()
            .range_rate_uncertainty_km_s;
        let far = aged
            .tracking_state(epoch() + chrono::Duration::days(3) + chrono::Duration::seconds(0))
            .unwrap()
            .range_rate_uncertainty_km_s;
        assert!(near > 0.0 && far.is_finite());
    }

    /// D-043: Earth orientation is applied. UT1-UTC of -0.227 s rotates the Earth by ~3.4″, which
    /// moves a LEO pass by tens of metres in range but well under 0.01° in direction.
    #[test]
    fn earth_orientation_shifts_tracking_slightly() {
        let base = EphemerustPropagator::new(ISS_TLE, 47.9088, -122.2503, 0.0).unwrap();
        let eop_station = StationModel {
            earth_orientation: EarthOrientation::new(-0.2273, 0.1875, 0.4171).unwrap(),
            ..StationModel::geometric(47.9088, -122.2503, 0.0)
        };
        let with_eop = EphemerustPropagator::new(ISS_TLE, 47.9088, -122.2503, 0.0)
            .unwrap()
            .with_station_model(eop_station);
        let t = epoch() + chrono::Duration::minutes(25);
        let (a, b) = (
            base.tracking_state(t).unwrap(),
            with_eop.tracking_state(t).unwrap(),
        );
        let d_range_m = (a.range_km - b.range_km).abs() * 1000.0;
        assert!(
            d_range_m > 1.0 && d_range_m < 500.0,
            "Δrange = {d_range_m} m"
        );
        assert!((a.elevation_deg - b.elevation_deg).abs() < 0.01);
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

        let scripted = TrackingState::new(10.0, 20.0, 30.0, 0.5);
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
