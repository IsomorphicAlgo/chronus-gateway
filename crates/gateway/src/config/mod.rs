//! Gateway configuration.
//!
//! Holds ingestion settings ([`IngestConfig`]) and ground-station / propagator settings
//! ([`StationConfig`]). HTTP/WebSocket bind is part of [`IngestConfig::http_bind`] (Milestone 5).
//! Optional TOML file loading lives in [`file`](mod@file) (Milestone 8).

pub mod file;

pub use file::{
    ConfigLoadError, load_effective_gateway_config, load_gateway_from_path, resolve_config_path,
};

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::hil_tm::{CCSDS_APID_MAX, DEFAULT_HIL_TM_V1_APID_MAX, DEFAULT_HIL_TM_V1_APID_MIN};
use crate::time::TimeFormat;

/// Configuration for the asynchronous UDP ingestion loop.
#[derive(Debug, Clone)]
pub struct IngestConfig {
    /// Local address the UDP socket binds to.
    ///
    /// Defaults to loopback to avoid firewall prompts during development; production deployments
    /// typically bind `0.0.0.0` (or a specific NIC) to receive from the SDR/front-end host.
    pub bind_addr: SocketAddr,

    /// Capacity of the internal broadcast channel, in frames.
    ///
    /// The channel is intentionally **lossy**: when full, the oldest frames are dropped and slow
    /// subscribers observe a lag rather than blocking the receive loop (see [`crate::ingest`]).
    pub channel_capacity: usize,

    /// Maximum accepted datagram size, in bytes. This fixes the receive-buffer size so the loop
    /// never allocates based on attacker-controlled input. Datagrams larger than this are dropped
    /// (Windows) or truncated by the OS (Unix); either way the loop stays in sync.
    pub max_datagram_size: usize,

    /// HTTP + WebSocket bind address (Milestone 5). Open MCT or other dashboards connect here.
    pub http_bind: SocketAddr,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            // 7301 is an arbitrary unprivileged default for the TMTC downlink port.
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 7301)),
            channel_capacity: 1024,
            // CCSDS space packets are at most 65536 + 6 bytes; a 64 KiB ceiling covers any single
            // packet while bounding per-datagram memory. Tunable per deployment.
            max_datagram_size: 65_542,
            http_bind: SocketAddr::from(([127, 0, 0, 1], 8080)),
        }
    }
}

/// Smallest datagram buffer that can hold a CCSDS primary header (6 bytes).
pub const MIN_DATAGRAM_SIZE: usize = 6;
/// Upper bound on [`IngestConfig::max_datagram_size`] (1 MiB). Larger values only waste memory:
/// a single UDP datagram can never exceed ~64 KiB.
pub const MAX_DATAGRAM_SIZE_LIMIT: usize = 1 << 20;

impl IngestConfig {
    /// Checks the ingest settings before any socket or channel is created.
    ///
    /// This catches values that would otherwise crash the gateway at startup (a zero-capacity
    /// broadcast channel panics inside Tokio) or silently drop every packet (a receive buffer
    /// smaller than a CCSDS header).
    ///
    /// # Examples
    ///
    /// ```
    /// use chronus_gateway::config::{ConfigError, IngestConfig};
    ///
    /// assert!(IngestConfig::default().validate().is_ok());
    ///
    /// let bad = IngestConfig { channel_capacity: 0, ..IngestConfig::default() };
    /// assert!(matches!(bad.validate(), Err(ConfigError::InvalidChannelCapacity(0))));
    /// ```
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.channel_capacity == 0 {
            return Err(ConfigError::InvalidChannelCapacity(self.channel_capacity));
        }
        if self.max_datagram_size < MIN_DATAGRAM_SIZE
            || self.max_datagram_size > MAX_DATAGRAM_SIZE_LIMIT
        {
            return Err(ConfigError::InvalidMaxDatagramSize(self.max_datagram_size));
        }
        Ok(())
    }
}

/// Reference ISS (ZARYA) element set used as the default TLE (valid checksums; epoch ~2020-07-12).
/// Public reference data only — synthetic/public data per project compliance posture (README).
pub const DEFAULT_ISS_TLE: &str = "ISS (ZARYA)\n\
    1 25544U 98067A   20194.88612269 -.00002218  00000-0 -31515-4 0  9992\n\
    2 25544  51.6461 221.2784 0001413  89.1723 280.4612 15.49507896236008";

/// Where the tracked spacecraft's ephemeris comes from.
///
/// One variant is active at a time; `#[non_exhaustive]` so more sources (for example a live
/// CelesTrak fetch) can be added in 1.x.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum EphemerisSource {
    /// TLE text provided inline (2- or 3-line element set), propagated with SGP4.
    TleInline(String),
    /// TLE read from a file on disk, propagated with SGP4.
    TleFile(PathBuf),
    /// Operator-supplied **CCSDS OEM** ephemeris file (KVN), used instead of a TLE
    /// (see [`crate::oem`] and `Methodology.md` D-045).
    OemFile(PathBuf),
}

impl Default for EphemerisSource {
    fn default() -> Self {
        Self::TleInline(DEFAULT_ISS_TLE.to_string())
    }
}

/// Ground-station and tracked-spacecraft configuration for the co-validation engine.
///
/// Grouped by concern so each part can grow independently:
///
/// | Group | Type | Holds |
/// |---|---|---|
/// | [`site`](Self::site) | [`SiteConfig`] | where the antenna is and the nominal carrier |
/// | [`tracking`](Self::tracking) | [`TrackingConfig`] | ephemeris source, Earth orientation, refraction, TLE-age margin |
/// | [`tolerances`](Self::tolerances) | [`ToleranceConfig`] | check thresholds and link-budget model |
/// | [`hil`](Self::hil) | [`HilConfig`] | synthetic HIL telemetry demo models (lab use) |
/// | [`rf`](Self::rf) | [`RfConfig`] | the `chronus.rf.v1` receiver side channel (Tranche 2) |
/// | [`time`](Self::time) | [`TimeConfig`] | spacecraft time-code decoding and skew policy (Tranche 2) |
///
/// All config structs are `#[non_exhaustive]`: start from [`Default`] and assign the fields you
/// need, so new fields in 1.x do not break callers.
///
/// ```
/// use chronus_gateway::config::{EphemerisSource, StationConfig};
///
/// let mut cfg = StationConfig::default();
/// cfg.site.latitude_deg = 47.9;
/// cfg.site.longitude_deg = -122.25;
/// cfg.tracking.ephemeris = EphemerisSource::TleFile("iss.tle".into());
/// cfg.tolerances.doppler_tolerance_hz = 200.0;
/// assert!(cfg.validate().is_ok());
/// ```
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct StationConfig {
    /// Antenna location and nominal downlink carrier.
    pub site: SiteConfig,
    /// What is tracked and how its position is computed.
    pub tracking: TrackingConfig,
    /// Thresholds and model parameters for the physics co-validation checks.
    pub tolerances: ToleranceConfig,
    /// Synthetic hardware-in-the-loop demo models (CV-3…CV-5).
    pub hil: HilConfig,
    /// Receiver-metadata side channel (`chronus.rf.v1`); disabled unless a bind address is set.
    pub rf: RfConfig,
    /// Spacecraft time-code decoding; disabled unless a format is chosen.
    pub time: TimeConfig,
}

/// Where the ground station is and the carrier it listens on.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SiteConfig {
    /// Geodetic latitude in degrees, north positive (`[-90, 90]`).
    pub latitude_deg: f64,
    /// Geodetic longitude in degrees, east positive (`[-180, 180]`).
    pub longitude_deg: f64,
    /// Station height above the WGS84 ellipsoid, in metres.
    pub altitude_m: f64,
    /// Nominal (un-shifted) downlink carrier frequency, in Hz. Used by the Doppler check (M4).
    pub nominal_carrier_hz: f64,
}

impl Default for SiteConfig {
    fn default() -> Self {
        Self {
            latitude_deg: 35.0,
            longitude_deg: -116.0,
            altitude_m: 1000.0,
            nominal_carrier_hz: 437_500_000.0,
        }
    }
}

/// What is tracked and how: ephemeris source, Earth orientation, refraction and TLE-age model.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TrackingConfig {
    /// Source of the tracked spacecraft's ephemeris (TLE inline, TLE file, or CCSDS OEM file).
    pub ephemeris: EphemerisSource,
    /// Minimum interval between propagator recomputations, in milliseconds. Frames arriving within
    /// this window reuse the last tracking state (throttle; `0` disables caching). 10 ms ≈ 100 Hz.
    pub min_recompute_interval_ms: u64,

    /// **Earth orientation:** UT1−UTC in seconds (IERS Bulletin A; within ±0.9 s). Default `0`.
    pub ut1_minus_utc_s: f64,
    /// **Earth orientation:** polar motion *x*ₚ in arcseconds (IERS Bulletin A). Default `0`.
    pub polar_motion_x_arcsec: f64,
    /// **Earth orientation:** polar motion *y*ₚ in arcseconds (IERS Bulletin A). Default `0`.
    pub polar_motion_y_arcsec: f64,

    /// Apply standard atmospheric **refraction** to the computed elevation before the pointing
    /// check (**CV-2**): a real antenna points at the *apparent* position, ~0.5° above the
    /// geometric one at the horizon. Default `true` (`Methodology.md` D-042).
    pub refraction: bool,
    /// Station air pressure for the refraction model, hPa. Default **1010**.
    pub refraction_pressure_hpa: f64,
    /// Station air temperature for the refraction model, °C. Default **10**.
    pub refraction_temperature_c: f64,

    /// Widen the Doppler tolerance by the range-rate error a TLE of this age is expected to cause
    /// (**T-DOPPLER**, `Methodology.md` D-044). Default `false` (fixed tolerance).
    pub doppler_tle_age_margin: bool,
    /// Assumed TLE along-track error at its epoch, km (default **1.0**).
    pub tle_error_km_at_epoch: f64,
    /// Assumed growth of TLE along-track error per day of age, km/day (default **2.0**).
    pub tle_error_growth_km_per_day: f64,
}

impl Default for TrackingConfig {
    fn default() -> Self {
        Self {
            ephemeris: EphemerisSource::default(),
            min_recompute_interval_ms: 10,
            ut1_minus_utc_s: 0.0,
            polar_motion_x_arcsec: 0.0,
            polar_motion_y_arcsec: 0.0,
            refraction: true,
            refraction_pressure_hpa: 1010.0,
            refraction_temperature_c: 10.0,
            doppler_tle_age_margin: false,
            tle_error_km_at_epoch: 1.0,
            tle_error_growth_km_per_day: 2.0,
        }
    }
}

/// Thresholds and model parameters for the physics checks (Doppler, horizon, link budget,
/// pointing).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ToleranceConfig {
    /// Maximum allowed |measured − expected| carrier deviation for the Doppler check (Hz).
    /// Default **150 Hz** — see `TEST_PLAN.md` (T-DOPPLER) and `Methodology.md` (D-012).
    pub doppler_tolerance_hz: f64,
    /// Minimum elevation (degrees) for accepting telemetry as geometrically plausible at the
    /// ground station. Frames with predicted elevation **strictly below** this value set
    /// [`FLAG_BELOW_HORIZON`](crate::validate::FLAG_BELOW_HORIZON). Default `0` = at or above the
    /// mathematical horizon is OK; negative values allow a refraction / mask margin.
    pub minimum_elevation_deg: f64,

    /// Synthetic **spacecraft transmit** power at the feed before the transmit antenna (dBm).
    /// Used only for the free-space **link-budget** check (CV-1 / **T-RSSI**); not calibrated to
    /// any real mission.
    pub tx_power_dbm: f64,
    /// Transmit antenna gain (dBi), synthetic default.
    pub tx_gain_dbi: f64,
    /// Receive antenna gain at the ground station (dBi), synthetic default.
    pub rx_gain_dbi: f64,
    /// Maximum allowed |measured − predicted| received power for the link-budget check (dB).
    /// Default **3 dB** — see `TEST_PLAN.md` (**T-RSSI**) and `Methodology.md` D-016.
    pub link_budget_tolerance_db: f64,
    /// Great-circle angular tolerance (degrees) for measured vs computed azimuth/elevation (**CV-2** / **T-POINT**).
    /// Default **0.25°** — see `TEST_PLAN.md` and `Methodology.md` D-016.
    pub pointing_tolerance_deg: f64,
}

impl Default for ToleranceConfig {
    fn default() -> Self {
        Self {
            doppler_tolerance_hz: 150.0,
            minimum_elevation_deg: 0.0,
            // Synthetic demo link: ~1 W transmit + modest gains — not a real spacecraft EIRP.
            tx_power_dbm: 30.0,
            tx_gain_dbi: 2.0,
            rx_gain_dbi: 5.0,
            link_budget_tolerance_db: 3.0,
            pointing_tolerance_deg: 0.25,
        }
    }
}

/// Synthetic hardware-in-the-loop (HIL) demo models: the `chronus.hil.tm.v1` payload band
/// (**CV-3**) and the toy EPS / thermal / body-rate cross-checks (**CV-4** / **CV-5**).
///
/// These describe lab models, not flight-grade physics. The values are always parsed and
/// validated so a config file stays portable, but the checks that use them only run when the
/// crate is built with the **`hil`** cargo feature.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HilConfig {
    /// Inclusive lower bound of CCSDS APIDs carrying **chronus.hil.tm.v1** synthetic payloads (**CV-3**).
    /// Default [`crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MIN`].
    pub tm_v1_apid_min: u16,
    /// Inclusive upper bound (must be ≥ `tm_v1_apid_min` and ≤ **0x7FF**).
    pub tm_v1_apid_max: u16,
    /// **CV-4:** model bus voltage at full toy Sun illumination, volts (synthetic).
    pub eps_voltage_full_sun_v: f64,
    /// **CV-4:** model bus voltage at zero illumination, volts.
    pub eps_voltage_eclipse_v: f64,
    /// **CV-4:** model panel temperature at full illumination, °C.
    pub thermal_c_full_sun: f64,
    /// **CV-4:** model panel temperature at eclipse, °C.
    pub thermal_c_eclipse: f64,
    /// **T-EPS:** relative tolerance on the voltage span (default **0.1** = ±10 %).
    pub eps_relative_tolerance: f64,
    /// **T-THERMAL:** absolute temperature band, kelvin (default **10**).
    pub thermal_absolute_tolerance_k: f64,
    /// **CV-5 / T-BODYRATE:** maximum allowed magnitude of HIL v1 `body_rate_deg_s` (deg/s).
    pub body_rate_max_abs_deg_s: f64,
}

impl Default for HilConfig {
    fn default() -> Self {
        Self {
            tm_v1_apid_min: DEFAULT_HIL_TM_V1_APID_MIN,
            tm_v1_apid_max: DEFAULT_HIL_TM_V1_APID_MAX,
            eps_voltage_full_sun_v: 28.0,
            eps_voltage_eclipse_v: 24.0,
            thermal_c_full_sun: 32.0,
            thermal_c_eclipse: 12.0,
            eps_relative_tolerance: 0.1,
            thermal_absolute_tolerance_k: 10.0,
            body_rate_max_abs_deg_s: 5.0,
        }
    }
}

/// Largest accepted [`RfConfig::capacity`].
pub const RF_CAPACITY_LIMIT: usize = 65_536;
/// Largest accepted [`RfConfig::window_ms`] (one minute).
pub const RF_WINDOW_MS_LIMIT: u64 = 60_000;

/// The `chronus.rf.v1` receiver-metadata side channel (see [`crate::rf`]).
///
/// Off by default: with no [`bind_addr`](Self::bind_addr) the gateway opens no second socket and
/// the Doppler, link-budget and pointing checks stay dormant unless a caller supplies
/// [`RfMetadata`](crate::validate::RfMetadata) directly.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RfConfig {
    /// UDP address for `chronus.rf.v1` datagrams. `None` disables the side channel.
    pub bind_addr: Option<SocketAddr>,
    /// A frame is matched to the RF sample nearest in time, but only within this many ms.
    pub window_ms: u64,
    /// Maximum RF samples kept (oldest evicted first).
    pub capacity: usize,
}

impl Default for RfConfig {
    fn default() -> Self {
        Self {
            bind_addr: None,
            window_ms: 250,
            capacity: 1024,
        }
    }
}

/// Spacecraft time-code policy (see [`crate::time`]).
///
/// With [`format`](Self::format) set, secondary-header packets are expected to start with a time
/// field. Physics is then evaluated at that **onboard** time; the receive time is the fallback when
/// the packet carries no usable time or the time disagrees with the gateway clock.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TimeConfig {
    /// Which time code leads the packet data field. [`TimeFormat::None`] disables decoding.
    pub format: TimeFormat,
    /// Largest accepted |onboard − receive| difference in seconds before the frame is flagged
    /// (`physics_flags_v2` bit 0) and physics falls back to receive time. **`0` disables the
    /// check (lab / replay mode):** onboard time is trusted as-is, which is what a recorded or
    /// simulated stream needs.
    pub max_skew_s: f64,
    /// TAI−UTC in seconds, used to decode CUC (TAI-based) time codes. 37 since 2017.
    pub cuc_tai_minus_utc_s: i32,
}

impl Default for TimeConfig {
    fn default() -> Self {
        Self {
            format: TimeFormat::None,
            max_skew_s: 5.0,
            cuc_tai_minus_utc_s: 37,
        }
    }
}

/// Errors produced while validating or resolving [`StationConfig`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// Latitude is non-finite or outside `[-90, 90]`.
    #[error("latitude {0}° is invalid (expected a finite value in [-90, 90])")]
    InvalidLatitude(f64),
    /// Longitude is non-finite or outside `[-180, 180]`.
    #[error("longitude {0}° is invalid (expected a finite value in [-180, 180])")]
    InvalidLongitude(f64),
    /// Altitude is non-finite.
    #[error("altitude {0} m is invalid (expected a finite value)")]
    InvalidAltitude(f64),
    /// Carrier frequency is non-finite or not positive.
    #[error("nominal carrier frequency {0} Hz is invalid (expected a finite, positive value)")]
    InvalidFrequency(f64),
    /// The inline TLE text was empty.
    #[error("TLE source is empty")]
    EmptyTle,
    /// Doppler tolerance is not a finite positive value.
    #[error("doppler tolerance {0} Hz is invalid (expected a finite value > 0)")]
    InvalidDopplerTolerance(f64),
    /// Minimum elevation threshold is non-finite or outside `[-90, 90]`.
    #[error("minimum elevation {0}° is invalid (expected a finite value in [-90, 90])")]
    InvalidMinimumElevation(f64),
    /// Link-budget tolerance (dB) is not a finite value greater than zero.
    #[error("link budget tolerance {0} dB is invalid (expected a finite value > 0)")]
    InvalidLinkBudgetTolerance(f64),
    /// Pointing tolerance (degrees) is not a finite value greater than zero (**T-POINT** / CV-2).
    #[error("pointing tolerance {0}° is invalid (expected a finite value > 0)")]
    InvalidPointingTolerance(f64),
    /// A link-budget numeric field is non-finite (**CV-1**).
    #[error("link budget field `{field}` is invalid (expected a finite value)")]
    InvalidLinkBudgetField {
        /// Which field failed validation.
        field: &'static str,
        /// The offending value.
        value: f64,
    },
    /// HIL TM v1 synthetic APID range is empty or outside the 11-bit CCSDS APID space.
    #[error(
        "HIL TM v1 APID range {min:#x}..={max:#x} is invalid (expected min <= max and both <= {apid_max:#x})"
    )]
    InvalidHilTmV1ApidRange {
        /// Configured lower bound.
        min: u16,
        /// Configured upper bound.
        max: u16,
        /// Largest legal APID (`0x7FF`).
        apid_max: u16,
    },
    /// HIL CV-4 EPS relative tolerance is not finite or not in `(0, 1]`.
    #[error("HIL EPS relative tolerance {0} is invalid (expected a finite value in (0, 1])")]
    InvalidHilEpsRelativeTolerance(f64),
    /// HIL CV-4 thermal absolute tolerance is not a finite positive value.
    #[error("HIL thermal absolute tolerance {0} K is invalid (expected a finite value > 0)")]
    InvalidHilThermalTolerance(f64),
    /// A HIL CV-4 voltage or temperature endpoint is non-finite.
    #[error("HIL CV-4 field `{field}` is invalid (expected a finite value)")]
    InvalidHilSubsystemField {
        /// Which field failed validation.
        field: &'static str,
        /// The offending value.
        value: f64,
    },
    /// HIL CV-5 body-rate ceiling (deg/s) is not a finite positive value.
    #[error("HIL body-rate ceiling {0} deg/s is invalid (expected a finite value > 0)")]
    InvalidHilBodyRateMaxAbs(f64),
    /// `ingest.channel_capacity` is zero (Tokio's broadcast channel requires at least 1 slot).
    #[error("channel capacity {0} is invalid (expected at least 1)")]
    InvalidChannelCapacity(usize),
    /// `ingest.max_datagram_size` is smaller than a CCSDS header or unreasonably large.
    #[error(
        "max datagram size {0} bytes is invalid (expected {MIN_DATAGRAM_SIZE}..={MAX_DATAGRAM_SIZE_LIMIT})"
    )]
    InvalidMaxDatagramSize(usize),
    /// Earth orientation parameters are out of range (usually a unit mistake).
    #[error("earth orientation parameters are invalid: {0}")]
    InvalidEarthOrientation(String),
    /// A refraction-model field is non-finite or physically implausible.
    #[error("refraction field `{field}` = {value} is invalid")]
    InvalidRefraction {
        /// Which field failed validation.
        field: &'static str,
        /// The offending value.
        value: f64,
    },
    /// A TLE error-growth model field is non-finite or negative.
    #[error("TLE error model field `{field}` = {value} is invalid (expected a finite value >= 0)")]
    InvalidTleErrorModel {
        /// Which field failed validation.
        field: &'static str,
        /// The offending value.
        value: f64,
    },
    /// A `[rf]` or `[time]` setting is out of range.
    #[error("setting `{field}` = {value} is invalid ({expected})")]
    InvalidRfTime {
        /// Dotted setting name, e.g. `rf.window_ms`.
        field: &'static str,
        /// The offending value, rendered.
        value: String,
        /// What would be accepted.
        expected: &'static str,
    },
    /// An OEM ephemeris file could not be read.
    #[error("failed to read OEM file {path}: {source}")]
    OemRead {
        /// The path that failed to read.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The configured ephemeris is an OEM file, but TLE text was requested.
    #[error("the configured ephemeris source is an OEM file, not a TLE")]
    NotATleSource,
    /// A TLE file could not be read.
    #[error("failed to read TLE file {path}: {source}")]
    TleRead {
        /// The path that failed to read.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

impl StationConfig {
    /// Validates the numeric fields and (for inline TLEs) that text is present.
    ///
    /// File-based TLEs are only checked for readability by [`StationConfig::resolve_tle_text`].
    ///
    /// # Examples
    ///
    /// ```
    /// use chronus_gateway::config::{ConfigError, StationConfig};
    /// use chronus_gateway::diagnose::Explain;
    ///
    /// assert!(StationConfig::default().validate().is_ok());
    ///
    /// let mut swapped = StationConfig::default();
    /// swapped.site.latitude_deg = -122.3;
    /// swapped.site.longitude_deg = 47.6;
    /// let err = swapped.validate().unwrap_err();
    /// assert!(matches!(err, ConfigError::InvalidLatitude(_)));
    /// // Every config error can explain itself in plain English.
    /// assert!(err.explain().cause.contains("swapped"));
    /// ```
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.site.latitude_deg.is_finite() || !(-90.0..=90.0).contains(&self.site.latitude_deg)
        {
            return Err(ConfigError::InvalidLatitude(self.site.latitude_deg));
        }
        if !self.site.longitude_deg.is_finite()
            || !(-180.0..=180.0).contains(&self.site.longitude_deg)
        {
            return Err(ConfigError::InvalidLongitude(self.site.longitude_deg));
        }
        if !self.site.altitude_m.is_finite() {
            return Err(ConfigError::InvalidAltitude(self.site.altitude_m));
        }
        if !self.site.nominal_carrier_hz.is_finite() || self.site.nominal_carrier_hz <= 0.0 {
            return Err(ConfigError::InvalidFrequency(self.site.nominal_carrier_hz));
        }
        if !self.tolerances.doppler_tolerance_hz.is_finite()
            || self.tolerances.doppler_tolerance_hz <= 0.0
        {
            return Err(ConfigError::InvalidDopplerTolerance(
                self.tolerances.doppler_tolerance_hz,
            ));
        }
        if !self.tolerances.minimum_elevation_deg.is_finite()
            || !(-90.0..=90.0).contains(&self.tolerances.minimum_elevation_deg)
        {
            return Err(ConfigError::InvalidMinimumElevation(
                self.tolerances.minimum_elevation_deg,
            ));
        }
        if let EphemerisSource::TleInline(text) = &self.tracking.ephemeris
            && text.trim().is_empty()
        {
            return Err(ConfigError::EmptyTle);
        }
        self.earth_orientation()?;
        if !self.tracking.refraction_pressure_hpa.is_finite()
            || !(1.0..=1100.0).contains(&self.tracking.refraction_pressure_hpa)
        {
            return Err(ConfigError::InvalidRefraction {
                field: "refraction_pressure_hpa",
                value: self.tracking.refraction_pressure_hpa,
            });
        }
        if !self.tracking.refraction_temperature_c.is_finite()
            || !(-90.0..=60.0).contains(&self.tracking.refraction_temperature_c)
        {
            return Err(ConfigError::InvalidRefraction {
                field: "refraction_temperature_c",
                value: self.tracking.refraction_temperature_c,
            });
        }
        for (field, v) in [
            ("tle_error_km_at_epoch", self.tracking.tle_error_km_at_epoch),
            (
                "tle_error_growth_km_per_day",
                self.tracking.tle_error_growth_km_per_day,
            ),
        ] {
            if !v.is_finite() || v < 0.0 {
                return Err(ConfigError::InvalidTleErrorModel { field, value: v });
            }
        }
        if !self.tolerances.tx_power_dbm.is_finite() {
            return Err(ConfigError::InvalidLinkBudgetField {
                field: "tx_power_dbm",
                value: self.tolerances.tx_power_dbm,
            });
        }
        if !self.tolerances.tx_gain_dbi.is_finite() {
            return Err(ConfigError::InvalidLinkBudgetField {
                field: "tx_gain_dbi",
                value: self.tolerances.tx_gain_dbi,
            });
        }
        if !self.tolerances.rx_gain_dbi.is_finite() {
            return Err(ConfigError::InvalidLinkBudgetField {
                field: "rx_gain_dbi",
                value: self.tolerances.rx_gain_dbi,
            });
        }
        if !self.tolerances.link_budget_tolerance_db.is_finite()
            || self.tolerances.link_budget_tolerance_db <= 0.0
        {
            return Err(ConfigError::InvalidLinkBudgetTolerance(
                self.tolerances.link_budget_tolerance_db,
            ));
        }
        if !self.tolerances.pointing_tolerance_deg.is_finite()
            || self.tolerances.pointing_tolerance_deg <= 0.0
        {
            return Err(ConfigError::InvalidPointingTolerance(
                self.tolerances.pointing_tolerance_deg,
            ));
        }
        if self.hil.tm_v1_apid_min > self.hil.tm_v1_apid_max
            || self.hil.tm_v1_apid_min > CCSDS_APID_MAX
            || self.hil.tm_v1_apid_max > CCSDS_APID_MAX
        {
            return Err(ConfigError::InvalidHilTmV1ApidRange {
                min: self.hil.tm_v1_apid_min,
                max: self.hil.tm_v1_apid_max,
                apid_max: CCSDS_APID_MAX,
            });
        }
        for (field, v) in [
            (
                "hil_eps_voltage_full_sun_v",
                self.hil.eps_voltage_full_sun_v,
            ),
            ("hil_eps_voltage_eclipse_v", self.hil.eps_voltage_eclipse_v),
            ("hil_thermal_c_full_sun", self.hil.thermal_c_full_sun),
            ("hil_thermal_c_eclipse", self.hil.thermal_c_eclipse),
        ] {
            if !v.is_finite() {
                return Err(ConfigError::InvalidHilSubsystemField { field, value: v });
            }
        }
        if !self.hil.eps_relative_tolerance.is_finite()
            || self.hil.eps_relative_tolerance <= 0.0
            || self.hil.eps_relative_tolerance > 1.0
        {
            return Err(ConfigError::InvalidHilEpsRelativeTolerance(
                self.hil.eps_relative_tolerance,
            ));
        }
        if !self.hil.thermal_absolute_tolerance_k.is_finite()
            || self.hil.thermal_absolute_tolerance_k <= 0.0
        {
            return Err(ConfigError::InvalidHilThermalTolerance(
                self.hil.thermal_absolute_tolerance_k,
            ));
        }
        if !self.hil.body_rate_max_abs_deg_s.is_finite() || self.hil.body_rate_max_abs_deg_s <= 0.0
        {
            return Err(ConfigError::InvalidHilBodyRateMaxAbs(
                self.hil.body_rate_max_abs_deg_s,
            ));
        }
        if self.rf.window_ms == 0 || self.rf.window_ms > RF_WINDOW_MS_LIMIT {
            return Err(ConfigError::InvalidRfTime {
                field: "rf.window_ms",
                value: self.rf.window_ms.to_string(),
                expected: "1..=60000 milliseconds",
            });
        }
        if self.rf.capacity == 0 || self.rf.capacity > RF_CAPACITY_LIMIT {
            return Err(ConfigError::InvalidRfTime {
                field: "rf.capacity",
                value: self.rf.capacity.to_string(),
                expected: "1..=65536 samples",
            });
        }
        if !self.time.max_skew_s.is_finite() || self.time.max_skew_s < 0.0 {
            return Err(ConfigError::InvalidRfTime {
                field: "time.max_skew_s",
                value: self.time.max_skew_s.to_string(),
                expected: "a finite number of seconds >= 0 (0 disables the check)",
            });
        }
        if !(0..=100).contains(&self.time.cuc_tai_minus_utc_s) {
            return Err(ConfigError::InvalidRfTime {
                field: "time.cuc_tai_minus_utc_s",
                value: self.time.cuc_tai_minus_utc_s.to_string(),
                expected: "0..=100 seconds (37 today)",
            });
        }
        Ok(())
    }

    /// The configured Earth orientation parameters, validated.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidEarthOrientation`] when UT1−UTC exceeds ±1 s or a pole coordinate
    /// exceeds ±1″ (typically milliseconds or milliarcseconds entered by mistake).
    ///
    /// # Examples
    ///
    /// ```
    /// use chronus_gateway::config::StationConfig;
    ///
    /// let mut cfg = StationConfig::default();
    /// cfg.tracking.ut1_minus_utc_s = -0.2273;
    /// assert!(cfg.earth_orientation().is_ok());
    /// cfg.tracking.ut1_minus_utc_s = -227.3; // milliseconds entered by mistake
    /// assert!(cfg.earth_orientation().is_err());
    /// ```
    pub fn earth_orientation(&self) -> Result<ephemerust::EarthOrientation, ConfigError> {
        ephemerust::EarthOrientation::new(
            self.tracking.ut1_minus_utc_s,
            self.tracking.polar_motion_x_arcsec,
            self.tracking.polar_motion_y_arcsec,
        )
        .map_err(|e| ConfigError::InvalidEarthOrientation(e.to_string()))
    }

    /// Returns `true` if `apid` lies in the configured inclusive **HIL TM v1** synthetic band (**CV-3**).
    ///
    /// # Examples
    ///
    /// ```
    /// use chronus_gateway::config::StationConfig;
    ///
    /// let mut s = StationConfig::default();
    /// s.hil.tm_v1_apid_min = 0x100;
    /// s.hil.tm_v1_apid_max = 0x10F;
    /// assert!(s.apid_allows_hil_tm_v1(0x100));
    /// assert!(s.apid_allows_hil_tm_v1(0x10F));
    /// assert!(!s.apid_allows_hil_tm_v1(0x110));
    /// ```
    #[must_use]
    pub fn apid_allows_hil_tm_v1(&self, apid: u16) -> bool {
        (self.hil.tm_v1_apid_min..=self.hil.tm_v1_apid_max).contains(&apid)
    }

    /// Resolves the configured [`EphemerisSource`] to TLE text (reading the file if necessary).
    ///
    /// # Errors
    ///
    /// [`ConfigError::EmptyTle`], [`ConfigError::TleRead`], or
    /// [`ConfigError::NotATleSource`] when the source is an OEM file.
    ///
    /// # Examples
    ///
    /// ```
    /// use chronus_gateway::config::{ConfigError, EphemerisSource, StationConfig};
    ///
    /// let mut cfg = StationConfig::default();
    /// assert!(cfg.resolve_tle_text().unwrap().contains("25544"));
    ///
    /// cfg.tracking.ephemeris = EphemerisSource::TleFile("no/such/file.tle".into());
    /// assert!(matches!(cfg.resolve_tle_text(), Err(ConfigError::TleRead { .. })));
    ///
    /// cfg.tracking.ephemeris = EphemerisSource::OemFile("iss.oem".into());
    /// assert!(matches!(cfg.resolve_tle_text(), Err(ConfigError::NotATleSource)));
    /// ```
    pub fn resolve_tle_text(&self) -> Result<String, ConfigError> {
        match &self.tracking.ephemeris {
            EphemerisSource::TleInline(text) if text.trim().is_empty() => {
                Err(ConfigError::EmptyTle)
            }
            EphemerisSource::TleInline(text) => Ok(text.clone()),
            EphemerisSource::OemFile(_) => Err(ConfigError::NotATleSource),
            EphemerisSource::TleFile(path) => {
                std::fs::read_to_string(path).map_err(|source| ConfigError::TleRead {
                    path: path.display().to_string(),
                    source,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_station_is_valid() {
        StationConfig::default()
            .validate()
            .expect("default station validates");
    }

    #[test]
    fn rejects_out_of_range_fields() {
        let bad_lat = {
            let mut c = StationConfig::default();
            c.site.latitude_deg = 91.0;
            c
        };
        assert!(matches!(
            bad_lat.validate(),
            Err(ConfigError::InvalidLatitude(_))
        ));

        let bad_lon = {
            let mut c = StationConfig::default();
            c.site.longitude_deg = 200.0;
            c
        };
        assert!(matches!(
            bad_lon.validate(),
            Err(ConfigError::InvalidLongitude(_))
        ));

        let bad_freq = {
            let mut c = StationConfig::default();
            c.site.nominal_carrier_hz = 0.0;
            c
        };
        assert!(matches!(
            bad_freq.validate(),
            Err(ConfigError::InvalidFrequency(_))
        ));

        let nan_alt = {
            let mut c = StationConfig::default();
            c.site.altitude_m = f64::NAN;
            c
        };
        assert!(matches!(
            nan_alt.validate(),
            Err(ConfigError::InvalidAltitude(_))
        ));

        let bad_doppler = {
            let mut c = StationConfig::default();
            c.tolerances.doppler_tolerance_hz = 0.0;
            c
        };
        assert!(matches!(
            bad_doppler.validate(),
            Err(ConfigError::InvalidDopplerTolerance(_))
        ));

        let bad_el_thresh = {
            let mut c = StationConfig::default();
            c.tolerances.minimum_elevation_deg = 91.0;
            c
        };
        assert!(matches!(
            bad_el_thresh.validate(),
            Err(ConfigError::InvalidMinimumElevation(_))
        ));

        let bad_link_tol = {
            let mut c = StationConfig::default();
            c.tolerances.link_budget_tolerance_db = 0.0;
            c
        };
        assert!(matches!(
            bad_link_tol.validate(),
            Err(ConfigError::InvalidLinkBudgetTolerance(_))
        ));

        let nan_tx = {
            let mut c = StationConfig::default();
            c.tolerances.tx_power_dbm = f64::NAN;
            c
        };
        assert!(matches!(
            nan_tx.validate(),
            Err(ConfigError::InvalidLinkBudgetField {
                field: "tx_power_dbm",
                ..
            })
        ));

        let bad_point = {
            let mut c = StationConfig::default();
            c.tolerances.pointing_tolerance_deg = 0.0;
            c
        };
        assert!(matches!(
            bad_point.validate(),
            Err(ConfigError::InvalidPointingTolerance(_))
        ));

        let bad_hil_apid = {
            let mut c = StationConfig::default();
            c.hil.tm_v1_apid_min = 0x100;
            c.hil.tm_v1_apid_max = 0x50;
            c
        };
        assert!(matches!(
            bad_hil_apid.validate(),
            Err(ConfigError::InvalidHilTmV1ApidRange { .. })
        ));

        let apid_overflow = {
            let mut c = StationConfig::default();
            c.hil.tm_v1_apid_min = 0x7FF;
            c.hil.tm_v1_apid_max = 0x800;
            c
        };
        assert!(matches!(
            apid_overflow.validate(),
            Err(ConfigError::InvalidHilTmV1ApidRange { .. })
        ));
    }

    #[test]
    fn apid_allows_hil_tm_v1_respects_range() {
        let s = StationConfig::default();
        assert!(s.apid_allows_hil_tm_v1(0x7B0));
        assert!(s.apid_allows_hil_tm_v1(0x7BF));
        assert!(!s.apid_allows_hil_tm_v1(0x7C0));
        assert!(!s.apid_allows_hil_tm_v1(0x100));
    }

    #[test]
    fn rejects_invalid_hil_cv4_tolerance() {
        let bad_eps = {
            let mut c = StationConfig::default();
            c.hil.eps_relative_tolerance = 1.5;
            c
        };
        assert!(matches!(
            bad_eps.validate(),
            Err(ConfigError::InvalidHilEpsRelativeTolerance(_))
        ));

        let bad_th = {
            let mut c = StationConfig::default();
            c.hil.thermal_absolute_tolerance_k = 0.0;
            c
        };
        assert!(matches!(
            bad_th.validate(),
            Err(ConfigError::InvalidHilThermalTolerance(_))
        ));

        let bad_br = {
            let mut c = StationConfig::default();
            c.hil.body_rate_max_abs_deg_s = 0.0;
            c
        };
        assert!(matches!(
            bad_br.validate(),
            Err(ConfigError::InvalidHilBodyRateMaxAbs(_))
        ));
    }

    #[test]
    fn resolves_inline_tle_and_rejects_empty() {
        let text = StationConfig::default()
            .resolve_tle_text()
            .expect("inline resolves");
        assert!(text.contains("25544"));

        let empty = {
            let mut c = StationConfig::default();
            c.tracking.ephemeris = EphemerisSource::TleInline("   ".into());
            c
        };
        assert!(matches!(
            empty.resolve_tle_text(),
            Err(ConfigError::EmptyTle)
        ));
        assert!(matches!(empty.validate(), Err(ConfigError::EmptyTle)));
    }

    #[test]
    fn missing_tle_file_is_reported() {
        let cfg = {
            let mut c = StationConfig::default();
            c.tracking.ephemeris = EphemerisSource::TleFile(PathBuf::from("does/not/exist.tle"));
            c
        };
        assert!(matches!(
            cfg.resolve_tle_text(),
            Err(ConfigError::TleRead { .. })
        ));
    }

    #[test]
    fn default_ingest_is_valid() {
        IngestConfig::default()
            .validate()
            .expect("default ingest validates");
    }

    #[test]
    fn zero_channel_capacity_is_rejected_before_tokio_panics() {
        let cfg = IngestConfig {
            channel_capacity: 0,
            ..IngestConfig::default()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ConfigError::InvalidChannelCapacity(0))
        ));
    }

    #[test]
    fn datagram_size_bounds_are_enforced() {
        for bad in [0, MIN_DATAGRAM_SIZE - 1, MAX_DATAGRAM_SIZE_LIMIT + 1] {
            let cfg = IngestConfig {
                max_datagram_size: bad,
                ..IngestConfig::default()
            };
            assert!(
                matches!(cfg.validate(), Err(ConfigError::InvalidMaxDatagramSize(v)) if v == bad),
                "{bad} should be rejected"
            );
        }
        for ok in [MIN_DATAGRAM_SIZE, MAX_DATAGRAM_SIZE_LIMIT] {
            let cfg = IngestConfig {
                max_datagram_size: ok,
                ..IngestConfig::default()
            };
            assert!(cfg.validate().is_ok(), "{ok} should be accepted");
        }
    }
}
