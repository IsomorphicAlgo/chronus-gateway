//! TOML file loading for [`super::IngestConfig`] and [`super::StationConfig`].
//!
//! Used by the binary (`--config` / `CHRONUS_GATEWAY_CONFIG`). Format is documented in
//! `gateway.example.toml` at the workspace root.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{ConfigError, IngestConfig, StationConfig, TleSource};

/// Errors from reading or applying a gateway TOML file.
#[derive(Debug, thiserror::Error)]
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
    /// Both `tle_inline` and `tle_file` were set under `[station]`.
    #[error("`station.tle_inline` and `station.tle_file` cannot both be set")]
    TleSourceAmbiguous,
    /// `[station]` was present but neither TLE source field was set.
    #[error("`[station]` requires exactly one of `tle_inline` or `tle_file`")]
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
    tle_inline: Option<String>,
    tle_file: Option<PathBuf>,
}

impl TryFrom<StationToml> for StationConfig {
    type Error = ConfigLoadError;

    fn try_from(t: StationToml) -> Result<Self, Self::Error> {
        let tle = match (&t.tle_inline, &t.tle_file) {
            (Some(_), Some(_)) => return Err(ConfigLoadError::TleSourceAmbiguous),
            (None, None) => return Err(ConfigLoadError::TleSourceMissing),
            (Some(text), None) => TleSource::Inline(text.clone()),
            (None, Some(path)) => TleSource::File(path.clone()),
        };
        let def = StationConfig::default();
        Ok(StationConfig {
            latitude_deg: t.latitude_deg,
            longitude_deg: t.longitude_deg,
            altitude_m: t.altitude_m,
            nominal_carrier_hz: t.nominal_carrier_hz,
            tle,
            min_recompute_interval_ms: t.min_recompute_interval_ms,
            doppler_tolerance_hz: t.doppler_tolerance_hz,
            minimum_elevation_deg: t.minimum_elevation_deg,
            tx_power_dbm: t.tx_power_dbm.unwrap_or(def.tx_power_dbm),
            tx_gain_dbi: t.tx_gain_dbi.unwrap_or(def.tx_gain_dbi),
            rx_gain_dbi: t.rx_gain_dbi.unwrap_or(def.rx_gain_dbi),
            link_budget_tolerance_db: t
                .link_budget_tolerance_db
                .unwrap_or(def.link_budget_tolerance_db),
            pointing_tolerance_deg: t
                .pointing_tolerance_deg
                .unwrap_or(def.pointing_tolerance_deg),
            hil_tm_v1_apid_min: t.hil_tm_v1_apid_min.unwrap_or(def.hil_tm_v1_apid_min),
            hil_tm_v1_apid_max: t.hil_tm_v1_apid_max.unwrap_or(def.hil_tm_v1_apid_max),
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
    let station = match doc.station {
        Some(t) => StationConfig::try_from(t)?,
        None => StationConfig::default(),
    };
    station.validate()?;
    let _tle = station.resolve_tle_text()?;
    drop(_tle);
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
        if let Some(rest) = a.strip_prefix("--config=") {
            if !rest.is_empty() {
                return Some(PathBuf::from(rest));
            }
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
        assert!(matches!(station.tle, TleSource::Inline(_)));
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
        assert!(matches!(station.tle, TleSource::File(_)));

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
}
