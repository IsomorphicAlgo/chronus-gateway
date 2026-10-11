//! TOML file loading for [`super::IngestConfig`] and [`super::StationConfig`].
//!
//! Used by the binary (`--config` / `CHRONUS_GATEWAY_CONFIG`). Format is documented in
//! `gateway.example.toml` at the workspace root.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::time::TimeFormat;

use super::{
    ConfigError, EphemerisSource, HilConfig, IngestConfig, SiteConfig, StationConfig,
    ToleranceConfig, TrackingConfig,
};

/// Errors from reading or applying a gateway TOML file.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigLoadError {
    /// Config file could not be read.
    #[error("failed to read config file {path}: {source}")]
    Io {
        /// Path attempted.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// TOML syntax or structure does not match the schema.
    #[error("failed to parse gateway config TOML: {0}")]
    Toml(#[from] toml::de::Error),
    /// A socket address field could not be parsed.
    #[error("invalid socket address in `{field}`: {source}")]
    AddrParse {
        /// Field name (e.g. `ingest.bind_addr`).
        field: &'static str,
        /// Underlying parse error.
        #[source]
        source: std::net::AddrParseError,
    },
    /// More than one of `tle_inline`, `tle_file` and `oem_file` was set under `[station]`.
    #[error(
        "only one of `station.tle_inline`, `station.tle_file` and `station.oem_file` may be set"
    )]
    TleSourceAmbiguous,
    /// `[station]` was present but no orbit source (`tle_inline`, `tle_file` or `oem_file`) was set.
    #[error("`[station]` requires exactly one of `tle_inline`, `tle_file` or `oem_file`")]
    TleSourceMissing,
    /// [`StationConfig::validate`] or [`StationConfig::resolve_tle_text`] failed after load.
    #[error(transparent)]
    Station(#[from] ConfigError),
}

/// Root document: optional `[ingest]` and `[station]` tables; omitted sections use defaults.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewayToml {
    #[serde(default)]
    ingest: Option<IngestToml>,
    #[serde(default)]
    station: Option<StationToml>,
    #[serde(default)]
    rf: Option<RfToml>,
    #[serde(default)]
    time: Option<TimeToml>,
}

/// Optional `[rf]` table: every key is optional and falls back to `RfConfig::default`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RfToml {
    bind_addr: Option<String>,
    window_ms: Option<u64>,
    capacity: Option<usize>,
}

/// Optional `[time]` table: every key is optional and falls back to `TimeConfig::default`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeToml {
    format: Option<String>,
    max_skew_s: Option<f64>,
    cuc_tai_minus_utc_s: Option<i32>,
}

/// Applies the optional `[rf]` / `[time]` tables onto an already-built station config.
fn apply_rf_time(
    station: &mut StationConfig,
    rf: Option<RfToml>,
    time: Option<TimeToml>,
) -> Result<(), ConfigLoadError> {
    if let Some(rf) = rf {
        if let Some(addr) = rf.bind_addr {
            station.rf.bind_addr =
                Some(addr.parse().map_err(|source| ConfigLoadError::AddrParse {
                    field: "rf.bind_addr",
                    source,
                })?);
        }
        if let Some(v) = rf.window_ms {
            station.rf.window_ms = v;
        }
        if let Some(v) = rf.capacity {
            station.rf.capacity = v;
        }
    }
    if let Some(time) = time {
        if let Some(f) = time.format {
            station.time.format = match f.to_ascii_lowercase().as_str() {
                "none" => TimeFormat::None,
                "cds" => TimeFormat::Cds,
                "cuc" => TimeFormat::Cuc,
                _ => {
                    return Err(ConfigError::InvalidRfTime {
                        field: "time.format",
                        value: format!("\"{f}\""),
                        expected: "\"none\", \"cds\" or \"cuc\"",
                    }
                    .into());
                }
            };
        }
        if let Some(v) = time.max_skew_s {
            station.time.max_skew_s = v;
        }
        if let Some(v) = time.cuc_tai_minus_utc_s {
            station.time.cuc_tai_minus_utc_s = v;
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IngestToml {
    bind_addr: String,
    channel_capacity: usize,
    max_datagram_size: usize,
    http_bind: String,
}

impl TryFrom<IngestToml> for IngestConfig {
    type Error = ConfigLoadError;

    fn try_from(t: IngestToml) -> Result<Self, Self::Error> {
        let bind_addr: SocketAddr =
            t.bind_addr
                .parse()
                .map_err(|source| ConfigLoadError::AddrParse {
                    field: "ingest.bind_addr",
                    source,
                })?;
        let http_bind: SocketAddr =
            t.http_bind
                .parse()
                .map_err(|source| ConfigLoadError::AddrParse {
                    field: "ingest.http_bind",
                    source,
                })?;
        Ok(IngestConfig {
            bind_addr,
            channel_capacity: t.channel_capacity,
            max_datagram_size: t.max_datagram_size,
            http_bind,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StationToml {
    latitude_deg: f64,
    longitude_deg: f64,
    altitude_m: f64,
    nominal_carrier_hz: f64,
    min_recompute_interval_ms: u64,
    doppler_tolerance_hz: f64,
    minimum_elevation_deg: f64,
    #[serde(default)]
    tx_power_dbm: Option<f64>,
    #[serde(default)]
    tx_gain_dbi: Option<f64>,
    #[serde(default)]
    rx_gain_dbi: Option<f64>,
    #[serde(default)]
    link_budget_tolerance_db: Option<f64>,
    #[serde(default)]
    pointing_tolerance_deg: Option<f64>,
    #[serde(default)]
    hil_tm_v1_apid_min: Option<u16>,
    #[serde(default)]
    hil_tm_v1_apid_max: Option<u16>,
    #[serde(default)]
    hil_eps_voltage_full_sun_v: Option<f64>,
    #[serde(default)]
    hil_eps_voltage_eclipse_v: Option<f64>,
    #[serde(default)]
    hil_thermal_c_full_sun: Option<f64>,
    #[serde(default)]
    hil_thermal_c_eclipse: Option<f64>,
    #[serde(default)]
    hil_eps_relative_tolerance: Option<f64>,
    #[serde(default)]
    hil_thermal_absolute_tolerance_k: Option<f64>,
    #[serde(default)]
    hil_body_rate_max_abs_deg_s: Option<f64>,
    tle_inline: Option<String>,
    tle_file: Option<PathBuf>,
    #[serde(default)]
    oem_file: Option<PathBuf>,
    #[serde(default)]
    ut1_minus_utc_s: Option<f64>,
    #[serde(default)]
    polar_motion_x_arcsec: Option<f64>,
    #[serde(default)]
    polar_motion_y_arcsec: Option<f64>,
    #[serde(default)]
    refraction: Option<bool>,
    #[serde(default)]
    refraction_pressure_hpa: Option<f64>,
    #[serde(default)]
    refraction_temperature_c: Option<f64>,
    #[serde(default)]
    doppler_tle_age_margin: Option<bool>,
    #[serde(default)]
    tle_error_km_at_epoch: Option<f64>,
    #[serde(default)]
    tle_error_growth_km_per_day: Option<f64>,
}

impl TryFrom<StationToml> for StationConfig {
    type Error = ConfigLoadError;

    /// Maps the flat, operator-facing `[station]` table onto the grouped [`StationConfig`].
    /// The TOML keys are a stable contract and deliberately do not mirror the in-memory grouping.
    fn try_from(t: StationToml) -> Result<Self, Self::Error> {
        let def = StationConfig::default();
        let ephemeris = match (t.tle_inline, t.tle_file, t.oem_file) {
            (Some(text), None, None) => EphemerisSource::TleInline(text),
            (None, Some(path), None) => EphemerisSource::TleFile(path),
            (None, None, Some(path)) => EphemerisSource::OemFile(path),
            (None, None, None) => return Err(ConfigLoadError::TleSourceMissing),
            _ => return Err(ConfigLoadError::TleSourceAmbiguous),
        };
        Ok(StationConfig {
            site: SiteConfig {
                latitude_deg: t.latitude_deg,
                longitude_deg: t.longitude_deg,
                altitude_m: t.altitude_m,
                nominal_carrier_hz: t.nominal_carrier_hz,
            },
            tracking: TrackingConfig {
                ephemeris,
                min_recompute_interval_ms: t.min_recompute_interval_ms,
                ut1_minus_utc_s: t.ut1_minus_utc_s.unwrap_or(def.tracking.ut1_minus_utc_s),
                polar_motion_x_arcsec: t
                    .polar_motion_x_arcsec
                    .unwrap_or(def.tracking.polar_motion_x_arcsec),
                polar_motion_y_arcsec: t
                    .polar_motion_y_arcsec
                    .unwrap_or(def.tracking.polar_motion_y_arcsec),
                refraction: t.refraction.unwrap_or(def.tracking.refraction),
                refraction_pressure_hpa: t
                    .refraction_pressure_hpa
                    .unwrap_or(def.tracking.refraction_pressure_hpa),
                refraction_temperature_c: t
                    .refraction_temperature_c
                    .unwrap_or(def.tracking.refraction_temperature_c),
                doppler_tle_age_margin: t
                    .doppler_tle_age_margin
                    .unwrap_or(def.tracking.doppler_tle_age_margin),
                tle_error_km_at_epoch: t
                    .tle_error_km_at_epoch
                    .unwrap_or(def.tracking.tle_error_km_at_epoch),
                tle_error_growth_km_per_day: t
                    .tle_error_growth_km_per_day
                    .unwrap_or(def.tracking.tle_error_growth_km_per_day),
            },
            tolerances: ToleranceConfig {
                doppler_tolerance_hz: t.doppler_tolerance_hz,
                minimum_elevation_deg: t.minimum_elevation_deg,
                tx_power_dbm: t.tx_power_dbm.unwrap_or(def.tolerances.tx_power_dbm),
                tx_gain_dbi: t.tx_gain_dbi.unwrap_or(def.tolerances.tx_gain_dbi),
                rx_gain_dbi: t.rx_gain_dbi.unwrap_or(def.tolerances.rx_gain_dbi),
                link_budget_tolerance_db: t
                    .link_budget_tolerance_db
                    .unwrap_or(def.tolerances.link_budget_tolerance_db),
                pointing_tolerance_deg: t
                    .pointing_tolerance_deg
                    .unwrap_or(def.tolerances.pointing_tolerance_deg),
            },
            hil: HilConfig {
                tm_v1_apid_min: t.hil_tm_v1_apid_min.unwrap_or(def.hil.tm_v1_apid_min),
                tm_v1_apid_max: t.hil_tm_v1_apid_max.unwrap_or(def.hil.tm_v1_apid_max),
                eps_voltage_full_sun_v: t
                    .hil_eps_voltage_full_sun_v
                    .unwrap_or(def.hil.eps_voltage_full_sun_v),
                eps_voltage_eclipse_v: t
                    .hil_eps_voltage_eclipse_v
                    .unwrap_or(def.hil.eps_voltage_eclipse_v),
                thermal_c_full_sun: t
                    .hil_thermal_c_full_sun
                    .unwrap_or(def.hil.thermal_c_full_sun),
                thermal_c_eclipse: t.hil_thermal_c_eclipse.unwrap_or(def.hil.thermal_c_eclipse),
                eps_relative_tolerance: t
                    .hil_eps_relative_tolerance
                    .unwrap_or(def.hil.eps_relative_tolerance),
                thermal_absolute_tolerance_k: t
                    .hil_thermal_absolute_tolerance_k
                    .unwrap_or(def.hil.thermal_absolute_tolerance_k),
                body_rate_max_abs_deg_s: t
                    .hil_body_rate_max_abs_deg_s
                    .unwrap_or(def.hil.body_rate_max_abs_deg_s),
            },
            rf: def.rf,
            time: def.time,
        })
    }
}

/// Reads `path`, parses gateway TOML, merges missing top-level sections with defaults, validates
/// the station, and ensures the TLE can be resolved (inline non-empty; file readable).
pub fn load_gateway_from_path(
    path: &Path,
) -> Result<(IngestConfig, StationConfig), ConfigLoadError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigLoadError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let doc: GatewayToml = toml::from_str(&text)?;
    let ingest = match doc.ingest {
        Some(t) => IngestConfig::try_from(t)?,
        None => IngestConfig::default(),
    };
    let mut station = match doc.station {
        Some(t) => StationConfig::try_from(t)?,
        None => StationConfig::default(),
    };
    apply_rf_time(&mut station, doc.rf, doc.time)?;
    ingest.validate()?;
    station.validate()?;
    if !matches!(station.tracking.ephemeris, EphemerisSource::OemFile(_)) {
        let _tle = station.resolve_tle_text()?;
    }
    Ok((ingest, station))
}

/// Loads [`load_gateway_from_path`] when `resolve_config_path()` is set; otherwise built-in
/// defaults (same as pre–Milestone 8 behavior), still validated and with TLE resolution checked.
pub fn load_effective_gateway_config() -> Result<(IngestConfig, StationConfig), ConfigLoadError> {
    match resolve_config_path() {
        Some(path) => load_gateway_from_path(&path),
        None => {
            let ingest = IngestConfig::default();
            let station = StationConfig::default();
            ingest.validate()?;
            station.validate()?;
            let _ = station.resolve_tle_text()?;
            Ok((ingest, station))
        }
    }
}

/// Resolves config file path: `--config` / `-c` (CLI) overrides `CHRONUS_GATEWAY_CONFIG` (env).
pub fn resolve_config_path() -> Option<PathBuf> {
    if let Some(p) = config_path_from_cli() {
        return Some(p);
    }
    std::env::var_os("CHRONUS_GATEWAY_CONFIG").map(PathBuf::from)
}

fn config_path_from_cli() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--config" || a == "-c" {
            return args.next().map(PathBuf::from);
        }
        if let Some(rest) = a.strip_prefix("--config=")
            && !rest.is_empty()
        {
            return Some(PathBuf::from(rest));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const MINIMAL_VALID: &str = r#"
[ingest]
bind_addr = "127.0.0.1:7301"
channel_capacity = 512
max_datagram_size = 2048
http_bind = "127.0.0.1:8080"

[station]
latitude_deg = 35.0
longitude_deg = -116.0
altitude_m = 1000.0
nominal_carrier_hz = 437500000.0
min_recompute_interval_ms = 10
doppler_tolerance_hz = 150.0
minimum_elevation_deg = 0.0
tle_inline = """
ISS (ZARYA)
1 25544U 98067A   20194.88612269 -.00002218  00000-0 -31515-4 0  9992
2 25544  51.6461 221.2784 0001413  89.1723 280.4612 15.49507896236008
"""
"#;

    #[test]
    fn parses_minimal_valid_toml() {
        let doc: GatewayToml = toml::from_str(MINIMAL_VALID).expect("toml");
        let ingest = IngestConfig::try_from(doc.ingest.expect("ingest")).expect("ingest");
        assert_eq!(ingest.channel_capacity, 512);
        let station = StationConfig::try_from(doc.station.expect("station")).expect("station");
        station.validate().expect("valid");
        assert!(matches!(
            station.tracking.ephemeris,
            EphemerisSource::TleInline(_)
        ));
    }

    #[test]
    fn load_from_path_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("gw.toml");
        std::fs::write(&p, MINIMAL_VALID).expect("write");
        let (ingest, station) = load_gateway_from_path(&p).expect("load");
        assert_eq!(ingest.channel_capacity, 512);
        assert!(station.resolve_tle_text().unwrap().contains("25544"));
    }

    #[test]
    fn tier2_station_keys_round_trip_with_defaults() {
        // Omitted: defaults (refraction on, EOP zero, TLE-age margin off).
        let doc: GatewayToml = toml::from_str(MINIMAL_VALID).expect("toml");
        let d = StationConfig::try_from(doc.station.unwrap()).unwrap();
        assert!(
            d.tracking.refraction
                && !d.tracking.doppler_tle_age_margin
                && d.tracking.ut1_minus_utc_s == 0.0
        );
        assert!(!matches!(d.tracking.ephemeris, EphemerisSource::OemFile(_)));

        let extra = MINIMAL_VALID.replace(
            "minimum_elevation_deg = 0.0\n",
            "minimum_elevation_deg = 0.0\nut1_minus_utc_s = -0.2273\npolar_motion_x_arcsec = 0.1875\n\
             polar_motion_y_arcsec = 0.4171\nrefraction = false\nrefraction_pressure_hpa = 850.0\n\
             refraction_temperature_c = -5.0\ndoppler_tle_age_margin = true\n\
             tle_error_km_at_epoch = 0.5\ntle_error_growth_km_per_day = 1.5\n",
        );
        let doc: GatewayToml = toml::from_str(&extra).expect("toml with tier-2 keys");
        let s = StationConfig::try_from(doc.station.unwrap()).unwrap();
        s.validate().expect("valid");
        assert_eq!(s.tracking.ut1_minus_utc_s, -0.2273);
        assert!(!s.tracking.refraction && s.tracking.doppler_tle_age_margin);
        assert_eq!(
            (
                s.tracking.tle_error_km_at_epoch,
                s.tracking.tle_error_growth_km_per_day
            ),
            (0.5, 1.5)
        );

        // Milliseconds instead of seconds is caught and explained.
        let ms = extra.replace("ut1_minus_utc_s = -0.2273", "ut1_minus_utc_s = -227.3");
        let doc: GatewayToml = toml::from_str(&ms).unwrap();
        let err = StationConfig::try_from(doc.station.unwrap())
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(
            matches!(err, ConfigError::InvalidEarthOrientation(_)),
            "{err}"
        );
    }

    #[test]
    fn oem_file_is_an_exclusive_orbit_source() {
        let oem_only = MINIMAL_VALID.replace(
            "tle_inline = \"\"\"\nISS (ZARYA)\n1 25544U 98067A   20194.88612269 -.00002218  00000-0 -31515-4 0  9992\n2 25544  51.6461 221.2784 0001413  89.1723 280.4612 15.49507896236008\n\"\"\"",
            "oem_file = \"ephemeris.oem\"",
        );
        assert!(oem_only.contains("oem_file"), "fixture replacement");
        let doc: GatewayToml = toml::from_str(&oem_only).unwrap();
        let s = StationConfig::try_from(doc.station.unwrap()).unwrap();
        assert!(matches!(
            &s.tracking.ephemeris,
            EphemerisSource::OemFile(p) if p == std::path::Path::new("ephemeris.oem")
        ));

        let both = MINIMAL_VALID.replace(
            "minimum_elevation_deg = 0.0\n",
            "minimum_elevation_deg = 0.0\noem_file = \"ephemeris.oem\"\n",
        );
        let doc: GatewayToml = toml::from_str(&both).unwrap();
        assert!(matches!(
            StationConfig::try_from(doc.station.unwrap()),
            Err(ConfigLoadError::TleSourceAmbiguous)
        ));
    }

    #[test]
    fn rejects_ambiguous_tle() {
        let bad = r#"
[ingest]
bind_addr = "127.0.0.1:7301"
channel_capacity = 1024
max_datagram_size = 65542
http_bind = "127.0.0.1:8080"

[station]
latitude_deg = 35.0
longitude_deg = -116.0
altitude_m = 1000.0
nominal_carrier_hz = 437500000.0
min_recompute_interval_ms = 10
doppler_tolerance_hz = 150.0
minimum_elevation_deg = 0.0
tle_inline = "1 25544U\n2 25544"
tle_file = "x.tle"
"#;
        let doc: GatewayToml = toml::from_str(bad).expect("parse");
        let err = StationConfig::try_from(doc.station.unwrap()).unwrap_err();
        assert!(matches!(err, ConfigLoadError::TleSourceAmbiguous));
    }

    #[test]
    fn rejects_missing_tle_when_station_present() {
        let bad = r#"
[ingest]
bind_addr = "127.0.0.1:7301"
channel_capacity = 1024
max_datagram_size = 65542
http_bind = "127.0.0.1:8080"

[station]
latitude_deg = 35.0
longitude_deg = -116.0
altitude_m = 1000.0
nominal_carrier_hz = 437500000.0
min_recompute_interval_ms = 10
doppler_tolerance_hz = 150.0
minimum_elevation_deg = 0.0
"#;
        let doc: GatewayToml = toml::from_str(bad).expect("parse");
        let err = StationConfig::try_from(doc.station.unwrap()).unwrap_err();
        assert!(matches!(err, ConfigLoadError::TleSourceMissing));
    }

    #[test]
    fn rejects_bad_bind_addr() {
        let bad = r#"
[ingest]
bind_addr = "not-a-socket"
channel_capacity = 1024
max_datagram_size = 65542
http_bind = "127.0.0.1:8080"
"#;
        let doc: GatewayToml = toml::from_str(bad).expect("parse");
        let err = IngestConfig::try_from(doc.ingest.unwrap()).unwrap_err();
        assert!(matches!(
            err,
            ConfigLoadError::AddrParse {
                field: "ingest.bind_addr",
                ..
            }
        ));
    }

    #[test]
    fn empty_document_uses_defaults() {
        let doc: GatewayToml = toml::from_str("").expect("empty TOML document");
        assert!(doc.ingest.is_none());
        assert!(doc.station.is_none());
        let ingest = match doc.ingest {
            Some(t) => IngestConfig::try_from(t).unwrap(),
            None => IngestConfig::default(),
        };
        let station = match doc.station {
            Some(t) => StationConfig::try_from(t).unwrap(),
            None => StationConfig::default(),
        };
        station.validate().unwrap();
        assert_eq!(ingest.bind_addr.port(), 7301);
    }

    #[test]
    fn tle_file_must_exist_at_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tle_path = dir.path().join("orbit.tle");
        let mut f = std::fs::File::create(&tle_path).expect("tle");
        writeln!(f, "ISS (ZARYA)").expect("w");
        writeln!(
            f,
            "1 25544U 98067A   20194.88612269 -.00002218  00000-0 -31515-4 0  9992"
        )
        .expect("w");
        writeln!(
            f,
            "2 25544  51.6461 221.2784 0001413  89.1723 280.4612 15.49507896236008"
        )
        .expect("w");
        drop(f);

        let cfg_path = dir.path().join("gw.toml");
        let abs = tle_path.canonicalize().expect("canonical TLE path");
        let body = format!(
            r#"
[ingest]
bind_addr = "127.0.0.1:7301"
channel_capacity = 1024
max_datagram_size = 65542
http_bind = "127.0.0.1:8080"

[station]
latitude_deg = 35.0
longitude_deg = -116.0
altitude_m = 1000.0
nominal_carrier_hz = 437500000.0
min_recompute_interval_ms = 10
doppler_tolerance_hz = 150.0
minimum_elevation_deg = 0.0
tle_file = '''{}'''
"#,
            abs.display()
        );
        std::fs::write(&cfg_path, body).expect("write cfg");
        let (_ingest, station) = load_gateway_from_path(&cfg_path).expect("load");
        assert!(matches!(
            station.tracking.ephemeris,
            EphemerisSource::TleFile(_)
        ));

        let missing = dir.path().join("missing.toml");
        std::fs::write(
            &missing,
            r#"
[ingest]
bind_addr = "127.0.0.1:7301"
channel_capacity = 1024
max_datagram_size = 65542
http_bind = "127.0.0.1:8080"

[station]
latitude_deg = 35.0
longitude_deg = -116.0
altitude_m = 1000.0
nominal_carrier_hz = 437500000.0
min_recompute_interval_ms = 10
doppler_tolerance_hz = 150.0
minimum_elevation_deg = 0.0
tle_file = "nope-not-here.tle"
"#,
        )
        .expect("write");
        let err = load_gateway_from_path(&missing).unwrap_err();
        assert!(matches!(
            err,
            ConfigLoadError::Station(ConfigError::TleRead { .. })
        ));
    }

    #[test]
    fn deny_unknown_top_level_key() {
        let bad = "foo = 1\n";
        assert!(toml::from_str::<GatewayToml>(bad).is_err());
    }

    #[test]
    fn zero_channel_capacity_in_file_is_a_clear_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("gw.toml");
        let text = MINIMAL_VALID.replace("channel_capacity = 512", "channel_capacity = 0");
        std::fs::write(&p, text).expect("write");
        let err = load_gateway_from_path(&p).unwrap_err();
        assert!(matches!(
            err,
            ConfigLoadError::Station(ConfigError::InvalidChannelCapacity(0))
        ));
    }

    #[test]
    fn missing_config_file_is_an_io_error_with_path() {
        let err = load_gateway_from_path(Path::new("does/not/exist.toml")).unwrap_err();
        match err {
            ConfigLoadError::Io { path, source } => {
                assert!(path.contains("exist.toml"));
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected Io, got {other:?}"),
        }
    }

    // ---- Tranche 2: [rf] and [time] tables ---------------------------------------------------

    fn load_str(toml_text: &str) -> Result<(IngestConfig, StationConfig), ConfigLoadError> {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("gw.toml");
        std::fs::write(&p, toml_text).expect("write");
        load_gateway_from_path(&p)
    }

    #[test]
    fn rf_and_time_default_to_off() {
        let (_, st) = load_str("").expect("empty doc loads");
        assert!(st.rf.bind_addr.is_none());
        assert_eq!(st.time.format, TimeFormat::None);
    }

    #[test]
    fn rf_and_time_tables_round_trip_partial_keys() {
        let (_, st) = load_str(
            r#"
[rf]
bind_addr = "127.0.0.1:7302"
window_ms = 80

[time]
format = "CUC"
max_skew_s = 0.0
"#,
        )
        .expect("loads");
        assert_eq!(st.rf.bind_addr, Some("127.0.0.1:7302".parse().unwrap()));
        assert_eq!(st.rf.window_ms, 80);
        assert_eq!(st.rf.capacity, 1024, "omitted keys keep defaults");
        assert_eq!(
            st.time.format,
            TimeFormat::Cuc,
            "format is case-insensitive"
        );
        assert_eq!(st.time.max_skew_s, 0.0);
        assert_eq!(st.time.cuc_tai_minus_utc_s, 37);
    }

    #[test]
    fn rf_time_tables_apply_alongside_a_station_table() {
        let (_, st) = load_str(&format!(
            "[station]\nlatitude_deg = 10.0\nlongitude_deg = 20.0\naltitude_m = 0.0\n\
             nominal_carrier_hz = 437500000.0\nmin_recompute_interval_ms = 10\n\
             doppler_tolerance_hz = 150.0\nminimum_elevation_deg = 0.0\n\
             tle_inline = \"\"\"\n{}\n\"\"\"\n[time]\nformat = \"cds\"\n",
            crate::config::DEFAULT_ISS_TLE
        ))
        .expect("loads");
        assert_eq!(st.site.latitude_deg, 10.0);
        assert_eq!(st.time.format, TimeFormat::Cds);
    }

    #[test]
    fn bad_rf_time_values_are_clear_errors() {
        for (doc, field) in [
            ("[rf]\nbind_addr = \"nonsense\"", "rf.bind_addr"),
            ("[time]\nformat = \"utc\"", "time.format"),
            ("[time]\nmax_skew_s = -1.0", "time.max_skew_s"),
            (
                "[time]\ncuc_tai_minus_utc_s = 500",
                "time.cuc_tai_minus_utc_s",
            ),
            ("[rf]\nwindow_ms = 0", "rf.window_ms"),
            ("[rf]\ncapacity = 0", "rf.capacity"),
            ("[rf]\ncapacity = 99999999", "rf.capacity"),
        ] {
            let err = load_str(doc).expect_err(doc);
            assert!(err.to_string().contains(field), "{doc} -> {err}");
        }
    }

    #[test]
    fn unknown_keys_in_rf_and_time_are_rejected() {
        assert!(load_str("[rf]\nbogus = 1").is_err());
        assert!(load_str("[time]\nbogus = 1").is_err());
    }
}
