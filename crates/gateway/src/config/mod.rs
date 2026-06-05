//! Gateway configuration.
//!
//! Holds ingestion settings ([`IngestConfig`]) and ground-station / propagator settings
//! ([`StationConfig`]). HTTP/WebSocket bind is part of [`IngestConfig::http_bind`] (Milestone 5).
//! Optional TOML file loading lives in [`file`] (Milestone 8).

pub mod file;

pub use file::{
    load_effective_gateway_config, load_gateway_from_path, resolve_config_path, ConfigLoadError,
};

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::hil_tm::{CCSDS_APID_MAX, DEFAULT_HIL_TM_V1_APID_MAX, DEFAULT_HIL_TM_V1_APID_MIN};

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

/// Reference ISS (ZARYA) element set used as the default TLE (valid checksums; epoch ~2020-07-12).
/// Public reference data only — synthetic/public data per project compliance posture (README).
pub const DEFAULT_ISS_TLE: &str = "ISS (ZARYA)\n\
    1 25544U 98067A   20194.88612269 -.00002218  00000-0 -31515-4 0  9992\n\
    2 25544  51.6461 221.2784 0001413  89.1723 280.4612 15.49507896236008";

/// Where the tracked spacecraft's TLE comes from.
#[derive(Debug, Clone)]
pub enum TleSource {
    /// TLE text provided inline (2- or 3-line element set).
    Inline(String),
    /// TLE read from a file on disk. (CelesTrak/Space-Track fetch is deferred; see backlog.)
    File(PathBuf),
}

/// Ground-station and tracked-spacecraft configuration for the co-validation engine.
#[derive(Debug, Clone)]
pub struct StationConfig {
    /// Geodetic latitude in degrees, north positive (`[-90, 90]`).
    pub latitude_deg: f64,
    /// Geodetic longitude in degrees, east positive (`[-180, 180]`).
    pub longitude_deg: f64,
    /// Station height above the WGS84 ellipsoid, in metres.
    pub altitude_m: f64,
    /// Nominal (un-shifted) downlink carrier frequency, in Hz. Used by the Doppler check (M4).
    pub nominal_carrier_hz: f64,
    /// Source of the tracked spacecraft's TLE.
    pub tle: TleSource,
    /// Minimum interval between propagator recomputations, in milliseconds. Frames arriving within
    /// this window reuse the last tracking state (throttle; `0` disables caching). 10 ms ≈ 100 Hz.
    pub min_recompute_interval_ms: u64,

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
    /// Maximum allowed \|measured − predicted\| received power for the link-budget check (dB).
    /// Default **3 dB** — see `TEST_PLAN.md` (**T-RSSI**) and `Methodology.md` D-016.
    pub link_budget_tolerance_db: f64,
    /// Great-circle angular tolerance (degrees) for measured vs computed azimuth/elevation (**CV-2** / **T-POINT**).
    /// Default **0.25°** — see `TEST_PLAN.md` and `Methodology.md` D-016.
    pub pointing_tolerance_deg: f64,
    /// Inclusive lower bound of CCSDS APIDs carrying **chronus.hil.tm.v1** synthetic payloads (**CV-3**).
    /// Default [`DEFAULT_HIL_TM_V1_APID_MIN`](crate::hil_tm::DEFAULT_HIL_TM_V1_APID_MIN).
    pub hil_tm_v1_apid_min: u16,
    /// Inclusive upper bound (must be ≥ `hil_tm_v1_apid_min` and ≤ **0x7FF**).
    pub hil_tm_v1_apid_max: u16,

    /// **CV-4:** model bus voltage at full toy Sun illumination, volts (synthetic).
    pub hil_eps_voltage_full_sun_v: f64,
    /// **CV-4:** model bus voltage at zero illumination, volts.
    pub hil_eps_voltage_eclipse_v: f64,
    /// **CV-4:** model panel temperature at full illumination, °C.
    pub hil_thermal_c_full_sun: f64,
    /// **CV-4:** model panel temperature at eclipse, °C.
    pub hil_thermal_c_eclipse: f64,
    /// **T-EPS:** relative tolerance on the voltage span (default **0.1** = ±10 %).
    pub hil_eps_relative_tolerance: f64,
    /// **T-THERMAL:** absolute temperature band, kelvin (default **10**).
    pub hil_thermal_absolute_tolerance_k: f64,
}

impl Default for StationConfig {
    fn default() -> Self {
        Self {
            latitude_deg: 35.0,
            longitude_deg: -116.0,
            altitude_m: 1000.0,
            nominal_carrier_hz: 437_500_000.0,
            tle: TleSource::Inline(DEFAULT_ISS_TLE.to_string()),
            min_recompute_interval_ms: 10,
            doppler_tolerance_hz: 150.0,
            minimum_elevation_deg: 0.0,
            // Synthetic demo link: ~1 W transmit + modest gains — not a real spacecraft EIRP.
            tx_power_dbm: 30.0,
            tx_gain_dbi: 2.0,
            rx_gain_dbi: 5.0,
            link_budget_tolerance_db: 3.0,
            pointing_tolerance_deg: 0.25,
            hil_tm_v1_apid_min: DEFAULT_HIL_TM_V1_APID_MIN,
            hil_tm_v1_apid_max: DEFAULT_HIL_TM_V1_APID_MAX,
            hil_eps_voltage_full_sun_v: 28.0,
            hil_eps_voltage_eclipse_v: 24.0,
            hil_thermal_c_full_sun: 32.0,
            hil_thermal_c_eclipse: 12.0,
            hil_eps_relative_tolerance: 0.1,
            hil_thermal_absolute_tolerance_k: 10.0,
        }
    }
}

/// Errors produced while validating or resolving [`StationConfig`].
#[derive(Debug, thiserror::Error)]
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
    /// HIL TM v1 synthetic APID range is empty or outside the 11-bit CCSDS APID space.
    #[error(
        "HIL TM v1 APID range {min:#x}..={max:#x} is invalid (expected min <= max and both <= {apid_max:#x})"
    )]
    InvalidHilTmV1ApidRange { min: u16, max: u16, apid_max: u16 },
    /// HIL CV-4 EPS relative tolerance is not finite or not in `(0, 1]`.
    #[error(
        "HIL EPS relative tolerance {0} is invalid (expected a finite value in (0, 1])"
    )]
    InvalidHilEpsRelativeTolerance(f64),
    /// HIL CV-4 thermal absolute tolerance is not a finite positive value.
    #[error(
        "HIL thermal absolute tolerance {0} K is invalid (expected a finite value > 0)"
    )]
    InvalidHilThermalTolerance(f64),
    /// A HIL CV-4 voltage or temperature endpoint is non-finite.
    #[error("HIL CV-4 field `{field}` is invalid (expected a finite value)")]
    InvalidHilSubsystemField {
        /// Which field failed validation.
        field: &'static str,
        /// The offending value.
        value: f64,
    },
    /// A link-budget power or gain field is non-finite.
    #[error("link budget field `{field}` is invalid (expected a finite value)")]
    InvalidLinkBudgetField {
        /// Which field failed validation.
        field: &'static str,
        /// The offending value.
        value: f64,
    },
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
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.latitude_deg.is_finite() || !(-90.0..=90.0).contains(&self.latitude_deg) {
            return Err(ConfigError::InvalidLatitude(self.latitude_deg));
        }
        if !self.longitude_deg.is_finite() || !(-180.0..=180.0).contains(&self.longitude_deg) {
            return Err(ConfigError::InvalidLongitude(self.longitude_deg));
        }
        if !self.altitude_m.is_finite() {
            return Err(ConfigError::InvalidAltitude(self.altitude_m));
        }
        if !self.nominal_carrier_hz.is_finite() || self.nominal_carrier_hz <= 0.0 {
            return Err(ConfigError::InvalidFrequency(self.nominal_carrier_hz));
        }
        if !self.doppler_tolerance_hz.is_finite() || self.doppler_tolerance_hz <= 0.0 {
            return Err(ConfigError::InvalidDopplerTolerance(
                self.doppler_tolerance_hz,
            ));
        }
        if !self.minimum_elevation_deg.is_finite()
            || !(-90.0..=90.0).contains(&self.minimum_elevation_deg)
        {
            return Err(ConfigError::InvalidMinimumElevation(
                self.minimum_elevation_deg,
            ));
        }
        if let TleSource::Inline(text) = &self.tle {
            if text.trim().is_empty() {
                return Err(ConfigError::EmptyTle);
            }
        }
        if !self.tx_power_dbm.is_finite() {
            return Err(ConfigError::InvalidLinkBudgetField {
                field: "tx_power_dbm",
                value: self.tx_power_dbm,
            });
        }
        if !self.tx_gain_dbi.is_finite() {
            return Err(ConfigError::InvalidLinkBudgetField {
                field: "tx_gain_dbi",
                value: self.tx_gain_dbi,
            });
        }
        if !self.rx_gain_dbi.is_finite() {
            return Err(ConfigError::InvalidLinkBudgetField {
                field: "rx_gain_dbi",
                value: self.rx_gain_dbi,
            });
        }
        if !self.link_budget_tolerance_db.is_finite() || self.link_budget_tolerance_db <= 0.0 {
            return Err(ConfigError::InvalidLinkBudgetTolerance(
                self.link_budget_tolerance_db,
            ));
        }
        if !self.pointing_tolerance_deg.is_finite() || self.pointing_tolerance_deg <= 0.0 {
            return Err(ConfigError::InvalidPointingTolerance(
                self.pointing_tolerance_deg,
            ));
        }
        if self.hil_tm_v1_apid_min > self.hil_tm_v1_apid_max
            || self.hil_tm_v1_apid_min > CCSDS_APID_MAX
            || self.hil_tm_v1_apid_max > CCSDS_APID_MAX
        {
            return Err(ConfigError::InvalidHilTmV1ApidRange {
                min: self.hil_tm_v1_apid_min,
                max: self.hil_tm_v1_apid_max,
                apid_max: CCSDS_APID_MAX,
            });
        }
        for (field, v) in [
            ("hil_eps_voltage_full_sun_v", self.hil_eps_voltage_full_sun_v),
            ("hil_eps_voltage_eclipse_v", self.hil_eps_voltage_eclipse_v),
            ("hil_thermal_c_full_sun", self.hil_thermal_c_full_sun),
            ("hil_thermal_c_eclipse", self.hil_thermal_c_eclipse),
        ] {
            if !v.is_finite() {
                return Err(ConfigError::InvalidHilSubsystemField { field, value: v });
            }
        }
        if !self.hil_eps_relative_tolerance.is_finite()
            || self.hil_eps_relative_tolerance <= 0.0
            || self.hil_eps_relative_tolerance > 1.0
        {
            return Err(ConfigError::InvalidHilEpsRelativeTolerance(
                self.hil_eps_relative_tolerance,
            ));
        }
        if !self.hil_thermal_absolute_tolerance_k.is_finite()
            || self.hil_thermal_absolute_tolerance_k <= 0.0
        {
            return Err(ConfigError::InvalidHilThermalTolerance(
                self.hil_thermal_absolute_tolerance_k,
            ));
        }
        Ok(())
    }

    /// Returns `true` if `apid` lies in the configured inclusive **HIL TM v1** synthetic band (**CV-3**).
    #[must_use]
    pub fn apid_allows_hil_tm_v1(&self, apid: u16) -> bool {
        (self.hil_tm_v1_apid_min..=self.hil_tm_v1_apid_max).contains(&apid)
    }

    /// Resolves the configured [`TleSource`] to TLE text (reading the file if necessary).
    pub fn resolve_tle_text(&self) -> Result<String, ConfigError> {
        match &self.tle {
            TleSource::Inline(text) if text.trim().is_empty() => Err(ConfigError::EmptyTle),
            TleSource::Inline(text) => Ok(text.clone()),
            TleSource::File(path) => {
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
        let bad_lat = StationConfig {
            latitude_deg: 91.0,
            ..Default::default()
        };
        assert!(matches!(
            bad_lat.validate(),
            Err(ConfigError::InvalidLatitude(_))
        ));

        let bad_lon = StationConfig {
            longitude_deg: 200.0,
            ..Default::default()
        };
        assert!(matches!(
            bad_lon.validate(),
            Err(ConfigError::InvalidLongitude(_))
        ));

        let bad_freq = StationConfig {
            nominal_carrier_hz: 0.0,
            ..Default::default()
        };
        assert!(matches!(
            bad_freq.validate(),
            Err(ConfigError::InvalidFrequency(_))
        ));

        let nan_alt = StationConfig {
            altitude_m: f64::NAN,
            ..Default::default()
        };
        assert!(matches!(
            nan_alt.validate(),
            Err(ConfigError::InvalidAltitude(_))
        ));

        let bad_doppler = StationConfig {
            doppler_tolerance_hz: 0.0,
            ..Default::default()
        };
        assert!(matches!(
            bad_doppler.validate(),
            Err(ConfigError::InvalidDopplerTolerance(_))
        ));

        let bad_el_thresh = StationConfig {
            minimum_elevation_deg: 91.0,
            ..Default::default()
        };
        assert!(matches!(
            bad_el_thresh.validate(),
            Err(ConfigError::InvalidMinimumElevation(_))
        ));

        let bad_link_tol = StationConfig {
            link_budget_tolerance_db: 0.0,
            ..Default::default()
        };
        assert!(matches!(
            bad_link_tol.validate(),
            Err(ConfigError::InvalidLinkBudgetTolerance(_))
        ));

        let nan_tx = StationConfig {
            tx_power_dbm: f64::NAN,
            ..Default::default()
        };
        assert!(matches!(
            nan_tx.validate(),
            Err(ConfigError::InvalidLinkBudgetField {
                field: "tx_power_dbm",
                ..
            })
        ));

        let bad_point = StationConfig {
            pointing_tolerance_deg: 0.0,
            ..Default::default()
        };
        assert!(matches!(
            bad_point.validate(),
            Err(ConfigError::InvalidPointingTolerance(_))
        ));

        let bad_hil_apid = StationConfig {
            hil_tm_v1_apid_min: 0x100,
            hil_tm_v1_apid_max: 0x50,
            ..Default::default()
        };
        assert!(matches!(
            bad_hil_apid.validate(),
            Err(ConfigError::InvalidHilTmV1ApidRange { .. })
        ));

        let apid_overflow = StationConfig {
            hil_tm_v1_apid_min: 0x7FF,
            hil_tm_v1_apid_max: 0x800,
            ..Default::default()
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
        let bad_eps = StationConfig {
            hil_eps_relative_tolerance: 1.5,
            ..Default::default()
        };
        assert!(matches!(
            bad_eps.validate(),
            Err(ConfigError::InvalidHilEpsRelativeTolerance(_))
        ));

        let bad_th = StationConfig {
            hil_thermal_absolute_tolerance_k: 0.0,
            ..Default::default()
        };
        assert!(matches!(
            bad_th.validate(),
            Err(ConfigError::InvalidHilThermalTolerance(_))
        ));
    }

    #[test]
    fn resolves_inline_tle_and_rejects_empty() {
        let text = StationConfig::default()
            .resolve_tle_text()
            .expect("inline resolves");
        assert!(text.contains("25544"));

        let empty = StationConfig {
            tle: TleSource::Inline("   ".into()),
            ..Default::default()
        };
        assert!(matches!(
            empty.resolve_tle_text(),
            Err(ConfigError::EmptyTle)
        ));
        assert!(matches!(empty.validate(), Err(ConfigError::EmptyTle)));
    }

    #[test]
    fn missing_tle_file_is_reported() {
        let cfg = StationConfig {
            tle: TleSource::File(PathBuf::from("does/not/exist.tle")),
            ..Default::default()
        };
        assert!(matches!(
            cfg.resolve_tle_text(),
            Err(ConfigError::TleRead { .. })
        ));
    }
}
