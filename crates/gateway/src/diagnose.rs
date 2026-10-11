//! Plain-English diagnostics: turns errors and counters into *what happened / why / how to fix it*.
//!
//! The rest of the crate reports problems with precise, typed errors ([`ConfigError`],
//! [`CcsdsError`], `std::io::Error`, …). Those are right for code, but an operator who just wants
//! the gateway running needs something different: a sentence they can act on. This module is that
//! translation layer. Nothing here changes behavior; it only explains.
//!
//! Every explanation is a [`Diagnosis`]:
//!
//! - a stable **code** (`CG-1xx` config, `CG-2xx` network, `CG-3xx` orbit data / tracking,
//!   `CG-4xx` incoming telemetry, `CG-5xx` runtime health) that can be searched for in
//!   `docs/USER_GUIDE.md`,
//! - a **severity**,
//! - a one-line **summary** of what happened,
//! - the most likely **cause**, written for a non-specialist, and
//! - one or more concrete **fixes**.
//!
//! The same pattern is used by the Ephemerust backend's `TleError::hint`, which this module reuses
//! for TLE problems.
//!
//! # Examples
//!
//! ```
//! use chronus_gateway::config::ConfigError;
//! use chronus_gateway::diagnose::{Explain, Severity};
//!
//! let d = ConfigError::InvalidLatitude(122.3).explain();
//! assert_eq!(d.code, "CG-101");
//! assert_eq!(d.severity, Severity::Error);
//! // The explanation spots the classic latitude/longitude swap.
//! assert!(d.cause.contains("swapped"));
//! println!("{d}");
//! ```

use std::fmt;
use std::io;
use std::net::SocketAddr;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::ccsds::{CCSDS_PRIMARY_HEADER_LEN, CcsdsError};
use crate::config::{
    ConfigError, ConfigLoadError, IngestConfig, MAX_DATAGRAM_SIZE_LIMIT, MIN_DATAGRAM_SIZE,
    StationConfig,
};
use crate::hil_tm::{HIL_TM_V1_PAYLOAD_LEN, HIL_TM_V1_SCHEMA_VERSION, HilTmV1DecodeError};
use crate::ingest::IngestSnapshot;
use crate::metrics::GatewayMetricsSnapshot;
use crate::validate::{
    FLAG_ADCS_BODY_RATE_ANOMALY, FLAG_BELOW_HORIZON, FLAG_DOPPLER_ANOMALY,
    FLAG_EPS_SUBSYSTEM_ANOMALY, FLAG_LINK_BUDGET_ANOMALY, FLAG_POINTING_ANOMALY,
    FLAG_THERMAL_SUBSYSTEM_ANOMALY,
};

/// Where the example configuration lives (referenced in fixes).
const EXAMPLE_CONFIG: &str = "gateway.example.toml";

/// TLEs older than this many days are too old for the Doppler (bit 0) and antenna-pointing
/// (bit 3) checks (warning, `CG-301`).
///
/// SGP4 along-track error grows by roughly 1–3 km per day. Near closest approach on a high LEO
/// pass at UHF, each kilometre of along-track error is about 25 Hz of Doppler error, so a few
/// days of drift uses up the default ±150 Hz band (`Methodology.md` D-041); the same drift is a
/// few tenths of a degree in pointing, the size of the 0.25° T-POINT tolerance.
pub const TLE_DOPPLER_DAYS: f64 = 3.0;
/// TLEs older than this many days are too inaccurate for any of the physics checks (warning,
/// `CG-302`).
pub const TLE_STALE_DAYS: f64 = 30.0;

/// How serious a [`Diagnosis`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Informational: nothing is wrong, but the operator may want to know.
    Info,
    /// The gateway keeps running, but results may be degraded or misleading.
    Warning,
    /// Something is broken and needs fixing (at startup, the gateway will not run).
    Error,
}

impl Severity {
    /// Short lowercase label used when printing (`note`, `warning`, `error`).
    ///
    /// ```
    /// use chronus_gateway::diagnose::Severity;
    /// assert_eq!(Severity::Warning.label(), "warning");
    /// ```
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Severity::Info => "note",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

/// A plain-English explanation of a problem: what happened, why, and how to fix it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnosis {
    /// Stable identifier, e.g. `CG-201`. Searchable in the user guide.
    pub code: &'static str,
    /// How serious the problem is.
    pub severity: Severity,
    /// One line: what happened.
    pub summary: String,
    /// The most likely reason, written for a non-specialist.
    pub cause: String,
    /// Concrete next steps, most likely fix first.
    pub fixes: Vec<String>,
}

impl Diagnosis {
    /// Creates a diagnosis with no fixes yet; chain [`Diagnosis::fix`] to add them.
    ///
    /// ```
    /// use chronus_gateway::diagnose::{Diagnosis, Severity};
    ///
    /// let d = Diagnosis::new("CG-999", Severity::Info, "Something happened", "Because reasons")
    ///     .fix("Do this first")
    ///     .fix("Then try this");
    /// assert_eq!(d.fixes.len(), 2);
    /// ```
    #[must_use]
    pub fn new(
        code: &'static str,
        severity: Severity,
        summary: impl Into<String>,
        cause: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity,
            summary: summary.into(),
            cause: cause.into(),
            fixes: Vec::new(),
        }
    }

    /// Appends a suggested fix (builder style).
    #[must_use]
    pub fn fix(mut self, step: impl Into<String>) -> Self {
        self.fixes.push(step.into());
        self
    }

    /// `true` if this diagnosis should stop the gateway from starting.
    #[must_use]
    pub fn is_fatal(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// Renders as a short, ASCII-only block (safe for the Windows console):
///
/// ```text
/// error [CG-201]: Can't listen for telemetry on UDP 127.0.0.1:7301 -- the port is already in use.
///   why: Another program is already using that port ...
///   fix: 1. Close the other program ...
///        2. Or pick a different port ...
/// ```
impl fmt::Display for Diagnosis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} [{}]: {}",
            self.severity.label(),
            self.code,
            self.summary
        )?;
        if !self.cause.is_empty() {
            write!(f, "\n  why: {}", self.cause)?;
        }
        for (i, step) in self.fixes.iter().enumerate() {
            if i == 0 {
                write!(f, "\n  fix: ")?;
            } else {
                write!(f, "\n       ")?;
            }
            if self.fixes.len() > 1 {
                write!(f, "{}. ", i + 1)?;
            }
            write!(f, "{step}")?;
        }
        Ok(())
    }
}

/// Types that can describe themselves in plain English.
pub trait Explain {
    /// Returns a [`Diagnosis`] describing this value for a non-specialist operator.
    fn explain(&self) -> Diagnosis;
}

// ---------------------------------------------------------------------------------------------
// CG-1xx — configuration
// ---------------------------------------------------------------------------------------------

impl Explain for ConfigError {
    fn explain(&self) -> Diagnosis {
        use Severity::Error as E;
        match self {
            ConfigError::InvalidLatitude(v) => {
                let cause = if v.is_finite() && (-180.0..=180.0).contains(v) {
                    format!(
                        "Latitude must be between -90 (South Pole) and 90 (North Pole). {v} looks \
                         like a longitude -- latitude and longitude may be swapped."
                    )
                } else {
                    "Latitude must be a number between -90 (South Pole) and 90 (North Pole)."
                        .to_string()
                };
                Diagnosis::new(
                    "CG-101",
                    E,
                    format!("Ground-station latitude {v} is not a real place on Earth."),
                    cause,
                )
                .fix("Set `station.latitude_deg` to your station's latitude in degrees (Seattle is 47.6).")
                .fix("North is positive, south is negative.")
            }
            ConfigError::InvalidLongitude(v) => Diagnosis::new(
                "CG-102",
                E,
                format!("Ground-station longitude {v} is not a real place on Earth."),
                "Longitude must be a number between -180 and 180 degrees.",
            )
            .fix("Set `station.longitude_deg` to your station's longitude (Seattle is -122.3).")
            .fix("East is positive, west is negative -- most of the Americas is negative."),
            ConfigError::InvalidAltitude(v) => Diagnosis::new(
                "CG-103",
                E,
                format!("Ground-station altitude {v} is not a usable number."),
                "Altitude must be an ordinary number of metres above sea level.",
            )
            .fix("Set `station.altitude_m` to your height in metres (use 0 if unsure)."),
            ConfigError::InvalidFrequency(v) => Diagnosis::new(
                "CG-104",
                E,
                format!("Radio carrier frequency {v} Hz is not valid."),
                "The carrier frequency must be a positive number in hertz (Hz).",
            )
            .fix("Set `station.nominal_carrier_hz` in Hz: 437.5 MHz is written 437500000."),
            ConfigError::EmptyTle => Diagnosis::new(
                "CG-105",
                E,
                "The satellite orbit data (TLE) is empty.",
                "The gateway needs a two-line element set (TLE) to know where the satellite is.",
            )
            .fix("Paste a TLE into `station.tle_inline`, or point `station.tle_file` at a TLE file.")
            .fix("Current TLEs are free at https://celestrak.org (search by satellite name or NORAD ID)."),
            ConfigError::InvalidDopplerTolerance(v) => tolerance(
                "CG-106",
                "Doppler tolerance",
                "station.doppler_tolerance_hz",
                *v,
                "Hz",
                "150",
            ),
            ConfigError::InvalidMinimumElevation(v) => Diagnosis::new(
                "CG-107",
                E,
                format!("Minimum elevation {v} degrees is not a possible sky angle."),
                "Elevation runs from -90 (straight down) through 0 (horizon) to 90 (overhead).",
            )
            .fix("Set `station.minimum_elevation_deg` between -90 and 90 (0 = the horizon is fine)."),
            ConfigError::InvalidLinkBudgetTolerance(v) => tolerance(
                "CG-108",
                "Link-budget tolerance",
                "station.link_budget_tolerance_db",
                *v,
                "dB",
                "3",
            ),
            ConfigError::InvalidPointingTolerance(v) => tolerance(
                "CG-109",
                "Pointing tolerance",
                "station.pointing_tolerance_deg",
                *v,
                "degrees",
                "0.25",
            ),
            ConfigError::InvalidLinkBudgetField { field, value } => Diagnosis::new(
                "CG-110",
                E,
                format!("Link-budget setting `station.{field}` = {value} is not a usable number."),
                "Radio power and antenna gains must be ordinary (finite) numbers.",
            )
            .fix(format!(
                "Fix or delete `station.{field}`; deleting it uses the built-in demo value."
            )),
            ConfigError::InvalidHilTmV1ApidRange { min, max, apid_max } => {
                let cause = if min > max {
                    format!(
                        "The range is backwards: the minimum ({min:#x}) is larger than the maximum ({max:#x})."
                    )
                } else {
                    format!("CCSDS packet IDs (APIDs) only go up to {apid_max:#x} ({apid_max}).")
                };
                Diagnosis::new(
                    "CG-111",
                    E,
                    format!("The simulator packet-ID range {min:#x}..={max:#x} is not valid."),
                    cause,
                )
                .fix("Make `station.hil_tm_v1_apid_min` <= `station.hil_tm_v1_apid_max`, both <= 0x7FF.")
                .fix("Or delete both lines to use the defaults.")
            }
            ConfigError::InvalidHilEpsRelativeTolerance(v) => {
                let mut d = Diagnosis::new(
                    "CG-112",
                    E,
                    format!("Battery-voltage tolerance {v} is not valid."),
                    "This tolerance is a fraction between 0 and 1 (0.1 means plus or minus 10%).",
                );
                if v.is_finite() && *v > 1.0 && *v <= 100.0 {
                    d = d.fix(format!(
                        "It looks like a percentage was entered. Use {} instead of {v}.",
                        v / 100.0
                    ));
                }
                d.fix("Set `station.hil_eps_relative_tolerance` to a value like 0.1.")
            }
            ConfigError::InvalidHilThermalTolerance(v) => tolerance(
                "CG-113",
                "Temperature tolerance",
                "station.hil_thermal_absolute_tolerance_k",
                *v,
                "kelvin",
                "10",
            ),
            ConfigError::InvalidHilSubsystemField { field, value } => Diagnosis::new(
                "CG-114",
                E,
                format!("Simulator model setting `station.{field}` = {value} is not a usable number."),
                "Voltage and temperature model values must be ordinary (finite) numbers.",
            )
            .fix(format!("Fix or delete `station.{field}`; deleting it uses the default.")),
            ConfigError::InvalidHilBodyRateMaxAbs(v) => tolerance(
                "CG-115",
                "Spin-rate limit",
                "station.hil_body_rate_max_abs_deg_s",
                *v,
                "deg/s",
                "5",
            ),
            ConfigError::InvalidChannelCapacity(v) => Diagnosis::new(
                "CG-116",
                E,
                format!("Internal queue size `ingest.channel_capacity` = {v} is too small."),
                "The gateway needs room for at least one packet between receiving and sending it on.",
            )
            .fix("Set `ingest.channel_capacity` to 1024 (the default) or delete the line."),
            ConfigError::InvalidMaxDatagramSize(v) => {
                let cause = if *v < MIN_DATAGRAM_SIZE {
                    format!(
                        "{v} bytes cannot even hold a {MIN_DATAGRAM_SIZE}-byte CCSDS header, so \
                         every packet would be rejected."
                    )
                } else {
                    format!(
                        "No single network packet is that large; values above {MAX_DATAGRAM_SIZE_LIMIT} \
                         bytes only waste memory."
                    )
                };
                Diagnosis::new(
                    "CG-117",
                    E,
                    format!("Packet size limit `ingest.max_datagram_size` = {v} bytes is not usable."),
                    cause,
                )
                .fix("Set `ingest.max_datagram_size` to 65542 (the default) or delete the line.")
            }
            ConfigError::TleRead { path, source } => explain_tle_read(path, source),
        }
    }
}

fn tolerance(
    code: &'static str,
    what: &str,
    field: &str,
    value: f64,
    unit: &str,
    default: &str,
) -> Diagnosis {
    let cause = if value.is_finite() && value <= 0.0 {
        format!("A tolerance of {value} would flag every frame; it must be greater than zero.")
    } else {
        "A tolerance must be an ordinary number greater than zero.".to_string()
    };
    Diagnosis::new(
        code,
        Severity::Error,
        format!("{what} {value} {unit} is not valid."),
        cause,
    )
    .fix(format!(
        "Set `{field}` to a positive number (default {default} {unit}), or delete the line."
    ))
}

fn explain_tle_read(path: &str, source: &io::Error) -> Diagnosis {
    let cwd = current_dir_display();
    match source.kind() {
        io::ErrorKind::NotFound => Diagnosis::new(
            "CG-118",
            Severity::Error,
            format!("Can't find the orbit-data (TLE) file `{path}`."),
            format!(
                "Relative paths are looked up from the folder the gateway was started in ({cwd})."
            ),
        )
        .fix("Check the spelling of `station.tle_file`, or use a full path.")
        .fix("Or paste the TLE text into `station.tle_inline` instead."),
        io::ErrorKind::PermissionDenied => Diagnosis::new(
            "CG-118",
            Severity::Error,
            format!("Not allowed to read the orbit-data (TLE) file `{path}`."),
            "Your user account does not have permission to open that file.",
        )
        .fix("Move the file somewhere you own (e.g. next to your config) or fix its permissions."),
        _ => Diagnosis::new(
            "CG-118",
            Severity::Error,
            format!("Couldn't read the orbit-data (TLE) file `{path}`."),
            format!("The operating system reported: {source}."),
        )
        .fix("Make sure the file is a plain-text TLE and not open in another program."),
    }
}

impl Explain for ConfigLoadError {
    fn explain(&self) -> Diagnosis {
        match self {
            ConfigLoadError::Io { path, source } => explain_config_io(path, source),
            ConfigLoadError::Toml(e) => explain_toml(&e.to_string()),
            ConfigLoadError::AddrParse { field, .. } => Diagnosis::new(
                "CG-122",
                Severity::Error,
                format!("`{field}` is not a valid network address."),
                "Addresses must be written as IP:port, with numbers only -- for example \
                 \"127.0.0.1:7301\". Host names like \"localhost\" are not accepted here.",
            )
            .fix(format!(
                "Use \"127.0.0.1:<port>\" for this computer only, or \"0.0.0.0:<port>\" to accept \
                 data from other machines. See {EXAMPLE_CONFIG}."
            )),
            ConfigLoadError::TleSourceAmbiguous => Diagnosis::new(
                "CG-123",
                Severity::Error,
                "The config gives the orbit data (TLE) twice.",
                "Both `station.tle_inline` and `station.tle_file` are set, so it is unclear which to use.",
            )
            .fix("Delete one of the two lines."),
            ConfigLoadError::TleSourceMissing => Diagnosis::new(
                "CG-124",
                Severity::Error,
                "The `[station]` section has no orbit data (TLE).",
                "The gateway needs to know which satellite to track.",
            )
            .fix("Add `tle_inline = \"\"\"...\"\"\"` with the TLE text, or `tle_file = \"path/to/file.tle\"`.")
            .fix("Or delete the whole `[station]` section to use the built-in demo station."),
            ConfigLoadError::Station(e) => e.explain(),
        }
    }
}

fn explain_config_io(path: &str, source: &io::Error) -> Diagnosis {
    let cwd = current_dir_display();
    let d = match source.kind() {
        io::ErrorKind::NotFound => Diagnosis::new(
            "CG-120",
            Severity::Error,
            format!("Can't find the config file `{path}`."),
            format!(
                "Relative paths are looked up from the folder the gateway was started in ({cwd})."
            ),
        )
        .fix("Check the path given to `--config` or the CHRONUS_GATEWAY_CONFIG variable.")
        .fix(format!(
            "To start from a working example, copy {EXAMPLE_CONFIG}."
        )),
        io::ErrorKind::PermissionDenied => Diagnosis::new(
            "CG-120",
            Severity::Error,
            format!("Not allowed to read the config file `{path}`."),
            "Your user account does not have permission to open that file.",
        )
        .fix("Copy the config somewhere you own, or fix the file's permissions."),
        _ => Diagnosis::new(
            "CG-120",
            Severity::Error,
            format!("Couldn't read the config file `{path}`."),
            format!("The operating system reported: {source}."),
        )
        .fix("Make sure the path points to a text file, not a folder."),
    };
    d.fix("Or run without `--config` to use the built-in demo settings.")
}

/// Explains a TOML parse failure from its rendered message (which already carries line/column).
fn explain_toml(message: &str) -> Diagnosis {
    let first_line = message.lines().next().unwrap_or(message).trim();
    let (summary, cause) = if message.contains("unknown field") {
        (
            "The config file contains a setting name the gateway doesn't recognize.",
            "This is usually a typo, or a setting placed under the wrong [section].",
        )
    } else if message.contains("missing field") {
        (
            "The config file is missing a required setting.",
            "Inside `[ingest]` and `[station]`, the core settings must all be present.",
        )
    } else if message.contains("invalid type") {
        (
            "A setting in the config file has the wrong kind of value.",
            "For example, numbers must not be in quotes and addresses must be in quotes.",
        )
    } else {
        (
            "The config file isn't valid TOML.",
            "Look for a missing quote, bracket or `=` near the line shown below.",
        )
    };
    Diagnosis::new(
        "CG-121",
        Severity::Error,
        summary,
        format!("{cause} Parser said: {first_line}"),
    )
    .fix(format!(
        "Compare your file with {EXAMPLE_CONFIG}, which lists every valid setting."
    ))
}

fn current_dir_display() -> String {
    std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Non-fatal configuration smells: values that are *legal* but almost certainly not intended.
///
/// Run after validation succeeds. Each returned diagnosis is a [`Severity::Warning`] or
/// [`Severity::Info`]; none of them should stop startup.
///
/// ```
/// use chronus_gateway::config::{IngestConfig, StationConfig};
/// use chronus_gateway::diagnose::config_warnings;
///
/// // 437.5 entered in MHz instead of Hz -- legal, but surely a mistake.
/// let station = StationConfig { nominal_carrier_hz: 437.5, ..StationConfig::default() };
/// let w = config_warnings(&IngestConfig::default(), &station);
/// assert!(w.iter().any(|d| d.code == "CG-130"));
/// ```
#[must_use]
pub fn config_warnings(ingest: &IngestConfig, station: &StationConfig) -> Vec<Diagnosis> {
    let mut out = Vec::new();
    let f = station.nominal_carrier_hz;
    if f.is_finite() && f > 0.0 && f < 1.0e6 {
        let as_hz = if f < 1.0e4 { f * 1.0e6 } else { f * 1.0e3 };
        out.push(
            Diagnosis::new(
                "CG-130",
                Severity::Warning,
                format!("Carrier frequency {f} Hz is unusually low for a satellite downlink."),
                "The value looks like it was entered in MHz or kHz; the setting is in Hz. \
                 With the wrong unit, every frame will be flagged as a Doppler anomaly.",
            )
            .fix(format!(
                "If you meant {f} {}, set `station.nominal_carrier_hz = {as_hz:.0}`.",
                if f < 1.0e4 { "MHz" } else { "kHz" }
            )),
        );
    }
    if ingest.http_bind.ip().is_unspecified() {
        out.push(
            Diagnosis::new(
                "CG-131",
                Severity::Info,
                format!(
                    "The dashboard API on {} is reachable from other computers on your network.",
                    ingest.http_bind
                ),
                "`ingest.http_bind` uses 0.0.0.0, which listens on every network interface.",
            )
            .fix("If only this computer needs the dashboard, use \"127.0.0.1\" instead."),
        );
    }
    if ingest.bind_addr.ip().is_loopback() {
        out.push(
            Diagnosis::new(
                "CG-132",
                Severity::Info,
                format!(
                    "Telemetry is only accepted from this computer (UDP {}).",
                    ingest.bind_addr
                ),
                "`ingest.bind_addr` uses 127.0.0.1, so radios or simulators on other machines \
                 can't reach the gateway.",
            )
            .fix("To receive from another machine, set `ingest.bind_addr = \"0.0.0.0:7301\"`."),
        );
    }
    if ingest.max_datagram_size < 64 {
        out.push(
            Diagnosis::new(
                "CG-133",
                Severity::Warning,
                format!(
                    "Packet size limit of {} bytes will cut off most telemetry packets.",
                    ingest.max_datagram_size
                ),
                "Anything longer than this limit is truncated or dropped before it can be read.",
            )
            .fix("Use the default `ingest.max_datagram_size = 65542` unless you have a reason not to."),
        );
    }
    out
}

// ---------------------------------------------------------------------------------------------
// CG-2xx — network
// ---------------------------------------------------------------------------------------------

/// Which listening socket a bind error came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listener {
    /// The UDP telemetry downlink socket (`ingest.bind_addr`).
    Udp,
    /// The HTTP + WebSocket dashboard server (`ingest.http_bind`).
    Http,
}

impl Listener {
    fn field(self) -> &'static str {
        match self {
            Listener::Udp => "ingest.bind_addr",
            Listener::Http => "ingest.http_bind",
        }
    }

    fn purpose(self) -> &'static str {
        match self {
            Listener::Udp => "listen for telemetry on UDP",
            Listener::Http => "start the dashboard server on HTTP",
        }
    }
}

/// Explains why the gateway could not open a listening socket.
///
/// ```
/// use std::io;
/// use chronus_gateway::diagnose::{Listener, explain_bind_error};
///
/// let err = io::Error::from(io::ErrorKind::AddrInUse);
/// let d = explain_bind_error(Listener::Udp, "127.0.0.1:7301".parse().unwrap(), &err);
/// assert_eq!(d.code, "CG-201");
/// assert!(d.summary.contains("already in use"));
/// ```
#[must_use]
pub fn explain_bind_error(listener: Listener, addr: SocketAddr, err: &io::Error) -> Diagnosis {
    let field = listener.field();
    let port = addr.port();
    let next_port = port.checked_add(1).unwrap_or(port.saturating_sub(1));
    match err.kind() {
        io::ErrorKind::AddrInUse => Diagnosis::new(
            "CG-201",
            Severity::Error,
            format!(
                "Can't {} {addr} -- the port is already in use.",
                listener.purpose()
            ),
            format!(
                "Another program is already using port {port}. Most often this is a second copy \
                 of the gateway that is still running."
            ),
        )
        .fix("Close the other copy of the gateway (check other terminal windows) and try again.")
        .fix(format!(
            "Or pick a different port: set `{field} = \"{}:{next_port}\"` in your config.",
            addr.ip()
        ))
        .fix(format!(
            "To see what is using the port: `netstat -ano | findstr :{port}` (Windows) or \
             `lsof -i :{port}` (macOS/Linux)."
        )),
        io::ErrorKind::PermissionDenied => {
            let cause = if port < 1024 {
                format!(
                    "Ports below 1024 are reserved and need administrator rights; {port} is one of them."
                )
            } else {
                "The operating system or a firewall refused access to that port.".to_string()
            };
            Diagnosis::new(
                "CG-202",
                Severity::Error,
                format!("Not allowed to {} {addr}.", listener.purpose()),
                cause,
            )
            .fix(format!(
                "Use a port above 1024 in `{field}` (the defaults are 7301 for UDP and 8080 for HTTP)."
            ))
        }
        io::ErrorKind::AddrNotAvailable => Diagnosis::new(
            "CG-203",
            Severity::Error,
            format!(
                "Can't {} {addr} -- this computer doesn't have that IP address.",
                listener.purpose()
            ),
            format!(
                "`{field}` names IP {}, but none of this computer's network adapters use it. \
                 This happens after moving to a different network or copying a config from \
                 another machine.",
                addr.ip()
            ),
        )
        .fix(format!(
            "Use \"127.0.0.1:{port}\" (this computer only) or \"0.0.0.0:{port}\" (all adapters)."
        )),
        _ => Diagnosis::new(
            "CG-204",
            Severity::Error,
            format!("Can't {} {addr}.", listener.purpose()),
            format!("The operating system reported: {err}."),
        )
        .fix(format!("Try a different address or port in `{field}`.")),
    }
}

// ---------------------------------------------------------------------------------------------
// CG-3xx — orbit data (TLE) and tracking
// ---------------------------------------------------------------------------------------------

/// Reads the epoch (the moment the orbit data describes) from TLE text, if it parses.
///
/// ```
/// use chronus_gateway::config::DEFAULT_ISS_TLE;
/// use chronus_gateway::diagnose::tle_epoch;
///
/// let epoch = tle_epoch(DEFAULT_ISS_TLE).expect("reference TLE parses");
/// assert_eq!(epoch.format("%Y-%m-%d").to_string(), "2020-07-12");
/// assert!(tle_epoch("not a TLE").is_none());
/// ```
#[must_use]
pub fn tle_epoch(tle_text: &str) -> Option<DateTime<Utc>> {
    ephemerust::Tle::parse(tle_text).ok().map(|t| t.epoch)
}

/// Warns when orbit data is too old (or dated in the future) to trust the physics checks.
///
/// SGP4 predictions drift by roughly 1–3 km per day along the orbit as a TLE ages. The Doppler
/// and pointing checks feel this first: after about [`TLE_DOPPLER_DAYS`] the drift can push them
/// past their tolerances near closest approach, so **healthy passes get flagged** (`CG-301`).
/// After [`TLE_STALE_DAYS`] every physics check is unreliable (`CG-302`). Returns `None` when the
/// TLE is fresh (up to [`TLE_DOPPLER_DAYS`] old).
///
/// ```
/// use chrono::{Duration, Utc};
/// use chronus_gateway::diagnose::check_tle_age;
///
/// let now = Utc::now();
/// assert!(check_tle_age(now - Duration::days(2), now).is_none());
/// assert_eq!(check_tle_age(now - Duration::days(400), now).unwrap().code, "CG-302");
/// ```
#[must_use]
pub fn check_tle_age(epoch: DateTime<Utc>, now: DateTime<Utc>) -> Option<Diagnosis> {
    let age_days = (now - epoch).num_seconds() as f64 / 86_400.0;
    let refresh = "Download a fresh TLE from https://celestrak.org and update \
                   `station.tle_inline` or the file named by `station.tle_file`.";
    if age_days < -1.0 {
        return Some(
            Diagnosis::new(
                "CG-303",
                Severity::Warning,
                format!(
                    "The orbit data (TLE) is dated {:.0} day(s) in the future.",
                    -age_days
                ),
                "Either this computer's clock is wrong, or the TLE was edited by hand.",
            )
            .fix("Check the system date and time.")
            .fix(refresh),
        );
    }
    if age_days > TLE_STALE_DAYS {
        return Some(
            Diagnosis::new(
                "CG-302",
                Severity::Warning,
                format!(
                    "The orbit data (TLE) is {:.0} days old -- too old to trust the physics checks.",
                    age_days
                ),
                format!(
                    "Orbit predictions drift a little every day. After about {TLE_STALE_DAYS:.0} \
                     days the predicted position and Doppler shift are far enough off that \
                     healthy telemetry will be flagged as anomalous."
                ),
            )
            .fix(refresh)
            .fix("If you are replaying old recorded data, this warning can be ignored."),
        );
    }
    if age_days > TLE_DOPPLER_DAYS {
        return Some(
            Diagnosis::new(
                "CG-301",
                Severity::Warning,
                format!(
                    "The orbit data (TLE) is {age_days:.1} days old -- too old for the Doppler \
                     and pointing checks."
                ),
                format!(
                    "Orbit predictions drift by roughly 1-3 km per day. After about \
                     {TLE_DOPPLER_DAYS:.0} days that is enough to push the predicted Doppler \
                     shift past the default 150 Hz tolerance near closest approach (and \
                     pointing past 0.25 degrees), so healthy passes can be flagged on bits 0 \
                     and 3. The horizon, link-budget and shadow checks are much less sensitive."
                ),
            )
            .fix(refresh)
            .fix(
                "CelesTrak updates element sets several times a day; refreshing daily keeps \
                 every check trustworthy.",
            )
            .fix(
                "If you are not feeding measured carrier frequency or antenna angles, the \
                 Doppler and pointing checks are skipped and this warning can be ignored.",
            ),
        );
    }
    None
}

/// Explains why orbital tracking could not be set up or computed.
///
/// Understands [`ConfigError`] (e.g. unreadable TLE file) and Ephemerust's errors, reusing
/// Ephemerust's own `hint()` for TLE format problems.
///
/// ```
/// use chronus_gateway::diagnose::explain_tracking_error;
/// use chronus_gateway::propagator::EphemerustPropagator;
///
/// let err = EphemerustPropagator::new("garbage", 35.0, -116.0, 0.0).err().unwrap();
/// let d = explain_tracking_error(&err);
/// assert_eq!(d.code, "CG-310");
/// ```
#[must_use]
pub fn explain_tracking_error(err: &anyhow::Error) -> Diagnosis {
    if let Some(cfg) = err.downcast_ref::<ConfigError>() {
        return cfg.explain();
    }
    if let Some(astro) = err.downcast_ref::<ephemerust::AstroError>() {
        return match astro {
            ephemerust::AstroError::Tle(tle) => {
                let mut d = Diagnosis::new(
                    "CG-310",
                    Severity::Warning,
                    "The satellite orbit data (TLE) couldn't be read, so physics checks are off.",
                    tle.to_string(),
                );
                if let Some(h) = tle.hint() {
                    d = d.fix(h);
                }
                d.fix("Copy a fresh TLE from https://celestrak.org exactly as published.")
            }
            other => Diagnosis::new(
                "CG-311",
                Severity::Warning,
                "The satellite's position couldn't be calculated for this moment.",
                format!(
                    "This usually means the orbit data (TLE) is very old or the satellite has \
                     re-entered. Details: {other}"
                ),
            )
            .fix("Download a current TLE from https://celestrak.org."),
        };
    }
    Diagnosis::new(
        "CG-312",
        Severity::Warning,
        "Orbital tracking is unavailable, so physics checks are off.",
        err.to_string(),
    )
    .fix("Check the `[station]` section of your config against gateway.example.toml.")
}

// ---------------------------------------------------------------------------------------------
// CG-4xx — incoming telemetry
// ---------------------------------------------------------------------------------------------

impl Explain for CcsdsError {
    fn explain(&self) -> Diagnosis {
        let hil_sim = "To check the gateway itself, send known-good test packets with \
                       `cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 2000`.";
        match self {
            CcsdsError::TooShort { len } => Diagnosis::new(
                "CG-401",
                Severity::Warning,
                format!("Received a {len}-byte packet -- too small to be CCSDS telemetry."),
                format!(
                    "Every CCSDS packet starts with a {CCSDS_PRIMARY_HEADER_LEN}-byte header. \
                     Something other than your telemetry source is probably sending to this port \
                     (a network test, or another tool)."
                ),
            )
            .fix("Make sure only your telemetry source sends to the gateway's UDP port.")
            .fix(hil_sim),
            CcsdsError::Header(e) => Diagnosis::new(
                "CG-402",
                Severity::Warning,
                "Received a packet whose header couldn't be decoded.",
                format!("The sender is probably not producing CCSDS Space Packets. Details: {e}"),
            )
            .fix("Confirm the sender is configured for CCSDS Space Packet output.")
            .fix(hil_sim),
            CcsdsError::Truncated {
                declared,
                available,
            } => Diagnosis::new(
                "CG-403",
                Severity::Warning,
                format!(
                    "Received a packet that was cut off: it says it is {declared} bytes but only \
                     {available} arrived."
                ),
                "Either the gateway's packet size limit is too small, or the sender split one \
                 packet across several network messages.",
            )
            .fix(format!(
                "Raise `ingest.max_datagram_size` to at least {declared} (the default 65542 fits any packet)."
            ))
            .fix("Make sure the sender puts exactly one whole packet in each UDP message."),
            CcsdsError::NotTelemetry { apid } => Diagnosis::new(
                "CG-404",
                Severity::Warning,
                format!("Received a command packet (APID {apid}) on the telemetry port."),
                "The gateway only accepts telemetry coming *down* from the spacecraft; this \
                 packet is marked as a command going *up* to it.",
            )
            .fix("Point command traffic somewhere else; only telemetry should reach this port."),
        }
    }
}

impl Explain for HilTmV1DecodeError {
    fn explain(&self) -> Diagnosis {
        let apid_fix = "If this APID isn't simulator data, narrow `station.hil_tm_v1_apid_min` / \
                        `station.hil_tm_v1_apid_max` so it is not decoded as such.";
        match self {
            HilTmV1DecodeError::TooShort { have } => Diagnosis::new(
                "CG-410",
                Severity::Info,
                format!(
                    "Simulator payload is {have} bytes; {HIL_TM_V1_PAYLOAD_LEN} are needed, so \
                     subsystem checks were skipped."
                ),
                "The packet's APID is in the simulator range but its payload is too short.",
            )
            .fix(apid_fix),
            HilTmV1DecodeError::BadMagic { got } => Diagnosis::new(
                "CG-411",
                Severity::Info,
                "A packet in the simulator APID range doesn't carry simulator data, so subsystem \
                 checks were skipped.",
                format!(
                    "Simulator payloads start with the marker \"CHI1\"; this one starts with {:?}.",
                    String::from_utf8_lossy(got)
                ),
            )
            .fix(apid_fix),
            HilTmV1DecodeError::WrongVersion { got } => Diagnosis::new(
                "CG-412",
                Severity::Info,
                format!(
                    "Simulator payload is format version {got}; this gateway understands version \
                     {HIL_TM_V1_SCHEMA_VERSION}."
                ),
                "The simulator and gateway were built from different releases.",
            )
            .fix("Update `chronus-hil-sim` and `chronus-gateway` to the same version."),
            HilTmV1DecodeError::NonZeroReserved => Diagnosis::new(
                "CG-413",
                Severity::Info,
                "Simulator payload has unexpected data in its reserved bytes.",
                "The payload was probably produced by a newer or modified simulator.",
            )
            .fix("Update `chronus-hil-sim` and `chronus-gateway` to the same version."),
        }
    }
}

/// Lists the physics checks that a frame's `physics_flags` says failed, in plain English.
///
/// ```
/// use chronus_gateway::diagnose::describe_physics_flags;
/// use chronus_gateway::validate::{FLAG_BELOW_HORIZON, FLAG_DOPPLER_ANOMALY};
///
/// assert!(describe_physics_flags(0).is_empty());
/// let why = describe_physics_flags(FLAG_DOPPLER_ANOMALY | FLAG_BELOW_HORIZON);
/// assert_eq!(why.len(), 2);
/// assert!(why[1].contains("below"));
/// ```
#[must_use]
pub fn describe_physics_flags(flags: u8) -> Vec<&'static str> {
    const TABLE: [(u8, &str); 7] = [
        (
            FLAG_DOPPLER_ANOMALY,
            "Radio frequency doesn't match the satellite's predicted motion (Doppler).",
        ),
        (
            FLAG_BELOW_HORIZON,
            "Satellite should be below the horizon, so the station shouldn't hear it.",
        ),
        (
            FLAG_LINK_BUDGET_ANOMALY,
            "Signal strength doesn't match the satellite's distance (link budget).",
        ),
        (
            FLAG_POINTING_ANOMALY,
            "Antenna pointing doesn't match where the satellite should be.",
        ),
        (
            FLAG_EPS_SUBSYSTEM_ANOMALY,
            "Battery bus voltage doesn't match the predicted sunlight.",
        ),
        (
            FLAG_THERMAL_SUBSYSTEM_ANOMALY,
            "Panel temperature is outside the range expected for the predicted sunlight.",
        ),
        (
            FLAG_ADCS_BODY_RATE_ANOMALY,
            "Spacecraft is spinning faster than the configured limit.",
        ),
    ];
    TABLE
        .iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, text)| *text)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// CG-5xx — runtime health
// ---------------------------------------------------------------------------------------------

/// Overall status word for [`HealthReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OverallStatus {
    /// Running and receiving good telemetry.
    Ok,
    /// Running, but no telemetry has arrived yet.
    Waiting,
    /// Running with warnings worth a look.
    Attention,
    /// Something is broken.
    Problem,
}

/// Plain-English health summary served at `GET /api/v1/chronus/status`.
#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    /// One word for dashboards and scripts.
    pub status: OverallStatus,
    /// One sentence for humans.
    pub headline: String,
    /// Everything worth knowing, most severe first.
    pub findings: Vec<Diagnosis>,
}

/// Inputs to [`assess_health`]: a snapshot of counters plus context.
#[derive(Debug, Clone)]
pub struct HealthInputs<'a> {
    /// UDP ingest counters.
    pub ingest: &'a IngestSnapshot,
    /// Distribution-path counters.
    pub gateway: &'a GatewayMetricsSnapshot,
    /// Whether an orbital propagator is configured.
    pub tracking_enabled: bool,
    /// Where telemetry should be sent, if known.
    pub udp_addr: Option<SocketAddr>,
    /// Where the dashboard server listens, if known.
    pub http_addr: Option<SocketAddr>,
}

/// Turns live counters into a plain-English [`HealthReport`].
///
/// Pure function (no I/O), so every rule is unit-tested.
///
/// ```
/// use chronus_gateway::diagnose::{HealthInputs, OverallStatus, assess_health};
/// use chronus_gateway::ingest::IngestStats;
/// use chronus_gateway::metrics::GatewayMetrics;
///
/// let ingest = IngestStats::default().snapshot_struct();
/// let gateway = GatewayMetrics::default().snapshot();
/// let report = assess_health(&HealthInputs {
///     ingest: &ingest,
///     gateway: &gateway,
///     tracking_enabled: true,
///     udp_addr: None,
///     http_addr: None,
/// });
/// assert_eq!(report.status, OverallStatus::Waiting);
/// ```
#[must_use]
pub fn assess_health(inputs: &HealthInputs<'_>) -> HealthReport {
    let ing = inputs.ingest;
    let gw = inputs.gateway;
    let udp = inputs.udp_addr.map_or_else(
        || "the gateway's UDP port".to_string(),
        |a| format!("UDP {a}"),
    );
    let mut findings = Vec::new();

    if ing.frames_received == 0 {
        findings.push(
            Diagnosis::new(
                "CG-501",
                Severity::Info,
                format!("Waiting for telemetry: nothing has arrived on {udp} yet."),
                "Either the sender isn't running yet, or it is sending somewhere else.",
            )
            .fix(format!("Check that your sender's destination is exactly {udp}."))
            .fix("If the sender is on another computer, the gateway must listen on 0.0.0.0 and the firewall must allow UDP.")
            .fix("For a quick test: `cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 2000`."),
        );
    }

    let parse_errors = gw.telemetry_parse_errors;
    let emitted = gw.telemetry_frames_emitted;
    if parse_errors > 0 && emitted == 0 {
        findings.push(
            Diagnosis::new(
                "CG-502",
                Severity::Error,
                format!("Data is arriving, but none of it ({parse_errors} packet(s)) is valid CCSDS telemetry."),
                "The sender is probably using a different format, or a different program is \
                 sending to this port.",
            )
            .fix("Configure the sender for CCSDS Space Packets (telemetry, one packet per UDP message).")
            .fix("Check the gateway log for the first rejected packet; it explains exactly what was wrong."),
        );
    } else if parse_errors > 0 && parse_errors * 10 > parse_errors + emitted {
        findings.push(
            Diagnosis::new(
                "CG-503",
                Severity::Warning,
                format!(
                    "{parse_errors} of {} packets were rejected as invalid.",
                    parse_errors + emitted
                ),
                "More than 10% of incoming data is not valid CCSDS telemetry -- likely a second \
                 sender or a damaged link.",
            )
            .fix("Check the gateway log for the first rejected packet; it explains exactly what was wrong."),
        );
    }

    if ing.oversized_dropped > 0 {
        findings.push(
            Diagnosis::new(
                "CG-504",
                Severity::Warning,
                format!(
                    "{} packet(s) were too large and were dropped.",
                    ing.oversized_dropped
                ),
                "They exceeded `ingest.max_datagram_size`.",
            )
            .fix("Raise `ingest.max_datagram_size` (65542 fits any CCSDS packet)."),
        );
    }

    if ing.recv_errors > 0 {
        findings.push(
            Diagnosis::new(
                "CG-505",
                Severity::Warning,
                format!("{} network receive error(s) occurred.", ing.recv_errors),
                "The operating system reported errors on the UDP socket. Occasional errors are \
                 harmless; a rising count points to a network problem.",
            )
            .fix("Check the gateway log for the error details."),
        );
    }

    if !inputs.tracking_enabled {
        findings.push(
            Diagnosis::new(
                "CG-506",
                Severity::Warning,
                "Physics checks are off: no satellite orbit data is loaded.",
                "Telemetry still flows to dashboards, but without position, Doppler or \
                 subsystem cross-checks.",
            )
            .fix("Check the startup log for why orbit data (TLE) couldn't be loaded."),
        );
    } else if gw.tracking_errors > 0 {
        findings.push(
            Diagnosis::new(
                "CG-507",
                Severity::Warning,
                format!(
                    "The satellite position couldn't be calculated for {} frame(s).",
                    gw.tracking_errors
                ),
                "Those frames were sent without physics checks. This almost always means the \
                 orbit data (TLE) is too old.",
            )
            .fix("Download a fresh TLE from https://celestrak.org."),
        );
    }

    if emitted >= 10 && gw.anomaly_frames * 2 > emitted {
        findings.push(
            Diagnosis::new(
                "CG-508",
                Severity::Warning,
                format!(
                    "{} of {emitted} frames were flagged as physics anomalies.",
                    gw.anomaly_frames
                ),
                "When most frames are flagged, the cause is usually the setup rather than the \
                 spacecraft: an old TLE, the wrong station location, or the wrong carrier frequency.",
            )
            .fix("Check `station.latitude_deg`, `station.longitude_deg` and `station.nominal_carrier_hz`.")
            .fix("Refresh the TLE from https://celestrak.org."),
        );
    }

    if ing.frames_received > 0 && gw.ws_clients_connected == 0 {
        let ws = inputs.http_addr.map_or_else(
            || "the gateway's /telemetry/openmct endpoint".to_string(),
            |a| format!("ws://{a}/telemetry/openmct"),
        );
        findings.push(
            Diagnosis::new(
                "CG-509",
                Severity::Info,
                "Telemetry is arriving, but no dashboard is connected to view it.",
                "Telemetry is streamed live only to connected dashboards; nothing is stored.",
            )
            .fix(format!(
                "Connect Open MCT or another WebSocket client to {ws}."
            )),
        );
    }

    if gw.serialize_errors > 0 {
        findings.push(
            Diagnosis::new(
                "CG-510",
                Severity::Error,
                format!(
                    "{} frame(s) could not be converted to JSON and were dropped.",
                    gw.serialize_errors
                ),
                "This should never happen and indicates a bug in the gateway.",
            )
            .fix("Please report it at https://github.com/IsomorphicAlgo/chronus-gateway/issues."),
        );
    }

    findings.sort_by_key(|d| std::cmp::Reverse(d.severity));

    let worst = findings.iter().map(|d| d.severity).max();
    let (status, headline) = match worst {
        Some(Severity::Error) => (
            OverallStatus::Problem,
            "Something needs fixing -- see the first finding.".to_string(),
        ),
        Some(Severity::Warning) => (
            OverallStatus::Attention,
            "Running, with warnings worth a look.".to_string(),
        ),
        _ if ing.frames_received == 0 => (
            OverallStatus::Waiting,
            format!("Running and waiting for telemetry on {udp}."),
        ),
        _ => (
            OverallStatus::Ok,
            format!(
                "Running normally: {} frame(s) received.",
                ing.frames_received
            ),
        ),
    };

    HealthReport {
        status,
        headline,
        findings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn addr(s: &str) -> SocketAddr {
        s.parse().expect("test address")
    }

    // ---- Diagnosis / Display ----------------------------------------------------------------

    #[test]
    fn display_single_fix_has_no_numbering() {
        let d = Diagnosis::new("CG-000", Severity::Error, "Broke", "Because").fix("Mend it");
        let s = d.to_string();
        assert_eq!(s, "error [CG-000]: Broke\n  why: Because\n  fix: Mend it");
    }

    #[test]
    fn display_multiple_fixes_are_numbered_and_aligned() {
        let d = Diagnosis::new("CG-000", Severity::Warning, "S", "C")
            .fix("one")
            .fix("two");
        let s = d.to_string();
        assert!(s.contains("  fix: 1. one\n       2. two"), "{s}");
        assert!(s.starts_with("warning [CG-000]"));
    }

    #[test]
    fn display_is_ascii_only_for_windows_consoles() {
        let samples = [
            ConfigError::InvalidLatitude(122.0).explain(),
            explain_bind_error(
                Listener::Udp,
                addr("127.0.0.1:7301"),
                &io::Error::from(io::ErrorKind::AddrInUse),
            ),
            CcsdsError::TooShort { len: 2 }.explain(),
        ];
        for d in samples {
            assert!(d.to_string().is_ascii(), "non-ASCII output: {d}");
        }
    }

    #[test]
    fn severity_orders_info_warning_error() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
        assert_eq!(Severity::Info.label(), "note");
    }

    #[test]
    fn only_errors_are_fatal() {
        assert!(Diagnosis::new("X", Severity::Error, "", "").is_fatal());
        assert!(!Diagnosis::new("X", Severity::Warning, "", "").is_fatal());
        assert!(!Diagnosis::new("X", Severity::Info, "", "").is_fatal());
    }

    #[test]
    fn diagnosis_serializes_to_json_for_the_status_endpoint() {
        let d = Diagnosis::new("CG-000", Severity::Warning, "S", "C").fix("F");
        let v = serde_json::to_value(&d).expect("json");
        assert_eq!(v["severity"], "warning");
        assert_eq!(v["fixes"][0], "F");
    }

    // ---- CG-1xx config -----------------------------------------------------------------------

    #[test]
    fn latitude_that_looks_like_a_longitude_suggests_a_swap() {
        let d = ConfigError::InvalidLatitude(-122.3).explain();
        assert_eq!(d.code, "CG-101");
        assert!(d.cause.contains("swapped"));
    }

    #[test]
    fn nonsense_latitude_does_not_suggest_a_swap() {
        let d = ConfigError::InvalidLatitude(f64::NAN).explain();
        assert!(!d.cause.contains("swapped"));
        let d = ConfigError::InvalidLatitude(500.0).explain();
        assert!(!d.cause.contains("swapped"));
    }

    #[test]
    fn longitude_explains_sign_convention() {
        let d = ConfigError::InvalidLongitude(200.0).explain();
        assert_eq!(d.code, "CG-102");
        assert!(d.fixes.iter().any(|f| f.contains("west is negative")));
    }

    #[test]
    fn frequency_error_shows_hz_example() {
        let d = ConfigError::InvalidFrequency(-1.0).explain();
        assert!(d.fixes[0].contains("437500000"));
    }

    #[test]
    fn empty_tle_points_to_celestrak() {
        let d = ConfigError::EmptyTle.explain();
        assert_eq!(d.code, "CG-105");
        assert!(d.fixes.iter().any(|f| f.contains("celestrak")));
    }

    #[test]
    fn zero_tolerance_explains_it_would_flag_everything() {
        let d = ConfigError::InvalidDopplerTolerance(0.0).explain();
        assert_eq!(d.code, "CG-106");
        assert!(d.cause.contains("flag every frame"));
        assert!(d.fixes[0].contains("station.doppler_tolerance_hz"));
    }

    #[test]
    fn every_tolerance_variant_names_its_own_field() {
        let cases = [
            (
                ConfigError::InvalidLinkBudgetTolerance(-1.0),
                "link_budget_tolerance_db",
            ),
            (
                ConfigError::InvalidPointingTolerance(0.0),
                "pointing_tolerance_deg",
            ),
            (
                ConfigError::InvalidHilThermalTolerance(0.0),
                "hil_thermal_absolute_tolerance_k",
            ),
            (
                ConfigError::InvalidHilBodyRateMaxAbs(-5.0),
                "hil_body_rate_max_abs_deg_s",
            ),
        ];
        for (err, field) in cases {
            let d = err.explain();
            assert!(d.fixes.iter().any(|f| f.contains(field)), "{field}: {d}");
        }
    }

    #[test]
    fn eps_tolerance_entered_as_percent_is_caught() {
        let d = ConfigError::InvalidHilEpsRelativeTolerance(10.0).explain();
        assert_eq!(d.code, "CG-112");
        assert!(d.fixes[0].contains("percentage"));
        assert!(d.fixes[0].contains("0.1"));
    }

    #[test]
    fn eps_tolerance_negative_has_no_percent_hint() {
        let d = ConfigError::InvalidHilEpsRelativeTolerance(-0.5).explain();
        assert!(!d.fixes.iter().any(|f| f.contains("percentage")));
    }

    #[test]
    fn backwards_apid_range_is_called_out() {
        let d = ConfigError::InvalidHilTmV1ApidRange {
            min: 0x200,
            max: 0x100,
            apid_max: 0x7FF,
        }
        .explain();
        assert!(d.cause.contains("backwards"));
    }

    #[test]
    fn apid_above_ccsds_limit_explains_limit() {
        let d = ConfigError::InvalidHilTmV1ApidRange {
            min: 0x100,
            max: 0x900,
            apid_max: 0x7FF,
        }
        .explain();
        assert!(d.cause.contains("0x7ff"));
    }

    #[test]
    fn link_budget_and_subsystem_fields_are_named() {
        let d = ConfigError::InvalidLinkBudgetField {
            field: "tx_gain_dbi",
            value: f64::INFINITY,
        }
        .explain();
        assert!(d.summary.contains("station.tx_gain_dbi"));
        let d = ConfigError::InvalidHilSubsystemField {
            field: "hil_thermal_c_eclipse",
            value: f64::NAN,
        }
        .explain();
        assert!(d.summary.contains("station.hil_thermal_c_eclipse"));
    }

    #[test]
    fn channel_capacity_and_datagram_size_explanations() {
        let d = ConfigError::InvalidChannelCapacity(0).explain();
        assert_eq!(d.code, "CG-116");
        let small = ConfigError::InvalidMaxDatagramSize(2).explain();
        assert!(small.cause.contains("header"));
        let huge = ConfigError::InvalidMaxDatagramSize(usize::MAX).explain();
        assert!(huge.cause.contains("waste memory"));
    }

    #[test]
    fn missing_tle_file_mentions_working_directory() {
        let err = ConfigError::TleRead {
            path: "iss.tle".into(),
            source: io::Error::from(io::ErrorKind::NotFound),
        };
        let d = err.explain();
        assert_eq!(d.code, "CG-118");
        assert!(d.summary.contains("Can't find"));
        assert!(d.cause.contains("started in"));
    }

    #[test]
    fn every_config_error_explanation_is_an_error_with_a_fix() {
        let all = [
            ConfigError::InvalidLatitude(91.0),
            ConfigError::InvalidLongitude(181.0),
            ConfigError::InvalidAltitude(f64::NAN),
            ConfigError::InvalidFrequency(0.0),
            ConfigError::EmptyTle,
            ConfigError::InvalidDopplerTolerance(0.0),
            ConfigError::InvalidMinimumElevation(95.0),
            ConfigError::InvalidLinkBudgetTolerance(0.0),
            ConfigError::InvalidPointingTolerance(0.0),
            ConfigError::InvalidLinkBudgetField {
                field: "tx_power_dbm",
                value: f64::NAN,
            },
            ConfigError::InvalidHilTmV1ApidRange {
                min: 2,
                max: 1,
                apid_max: 0x7FF,
            },
            ConfigError::InvalidHilEpsRelativeTolerance(0.0),
            ConfigError::InvalidHilThermalTolerance(0.0),
            ConfigError::InvalidHilSubsystemField {
                field: "hil_eps_voltage_eclipse_v",
                value: f64::NAN,
            },
            ConfigError::InvalidHilBodyRateMaxAbs(0.0),
            ConfigError::InvalidChannelCapacity(0),
            ConfigError::InvalidMaxDatagramSize(0),
            ConfigError::TleRead {
                path: "x".into(),
                source: io::Error::other("boom"),
            },
        ];
        let mut codes = std::collections::HashSet::new();
        for e in all {
            let d = e.explain();
            assert_eq!(d.severity, Severity::Error, "{d}");
            assert!(!d.fixes.is_empty(), "no fix for {d}");
            assert!(d.code.starts_with("CG-1"), "{d}");
            codes.insert(d.code);
        }
        assert_eq!(codes.len(), 18, "each variant should have its own code");
    }

    #[test]
    fn config_load_io_not_found_suggests_example_and_defaults() {
        let e = ConfigLoadError::Io {
            path: "gw.toml".into(),
            source: io::Error::from(io::ErrorKind::NotFound),
        };
        let d = e.explain();
        assert_eq!(d.code, "CG-120");
        assert!(d.fixes.iter().any(|f| f.contains(EXAMPLE_CONFIG)));
        assert!(d.fixes.last().unwrap().contains("without `--config`"));
    }

    #[test]
    fn toml_unknown_field_is_called_a_typo() {
        #[derive(Debug, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct T {
            a: u8,
        }
        let err = toml::from_str::<T>("a = 1\nlatitdue = 2").unwrap_err();
        let d = ConfigLoadError::Toml(err).explain();
        assert_eq!(d.code, "CG-121");
        assert!(d.cause.contains("typo"), "{d}");
    }

    #[test]
    fn toml_missing_field_and_syntax_errors_are_distinguished() {
        #[derive(Debug, serde::Deserialize)]
        #[allow(dead_code)]
        struct T {
            a: u8,
        }
        let missing = toml::from_str::<T>("").unwrap_err();
        assert!(
            ConfigLoadError::Toml(missing)
                .explain()
                .summary
                .contains("missing")
        );
        let syntax = toml::from_str::<T>("a = = 1").unwrap_err();
        assert!(
            ConfigLoadError::Toml(syntax)
                .explain()
                .summary
                .contains("valid TOML")
        );
        let ty = toml::from_str::<T>("a = \"one\"").unwrap_err();
        assert!(
            ConfigLoadError::Toml(ty)
                .explain()
                .summary
                .contains("wrong kind")
        );
    }

    #[test]
    fn addr_parse_rejects_localhost_with_explanation() {
        let source = "localhost:7301".parse::<SocketAddr>().unwrap_err();
        let d = ConfigLoadError::AddrParse {
            field: "ingest.bind_addr",
            source,
        }
        .explain();
        assert_eq!(d.code, "CG-122");
        assert!(d.cause.contains("localhost"));
    }

    #[test]
    fn tle_source_ambiguous_and_missing() {
        assert_eq!(ConfigLoadError::TleSourceAmbiguous.explain().code, "CG-123");
        assert_eq!(ConfigLoadError::TleSourceMissing.explain().code, "CG-124");
    }

    #[test]
    fn config_load_station_error_delegates() {
        let d = ConfigLoadError::Station(ConfigError::EmptyTle).explain();
        assert_eq!(d.code, "CG-105");
    }

    #[test]
    fn warns_when_frequency_entered_in_mhz() {
        let station = StationConfig {
            nominal_carrier_hz: 437.5,
            ..StationConfig::default()
        };
        let w = config_warnings(&IngestConfig::default(), &station);
        let d = w.iter().find(|d| d.code == "CG-130").expect("MHz warning");
        assert!(d.fixes[0].contains("437500000"), "{d}");
    }

    #[test]
    fn warns_when_frequency_entered_in_khz() {
        let station = StationConfig {
            nominal_carrier_hz: 437_500.0,
            ..StationConfig::default()
        };
        let w = config_warnings(&IngestConfig::default(), &station);
        let d = w.iter().find(|d| d.code == "CG-130").expect("kHz warning");
        assert!(d.fixes[0].contains("kHz"));
    }

    #[test]
    fn default_config_has_only_informational_notes() {
        let w = config_warnings(&IngestConfig::default(), &StationConfig::default());
        assert!(w.iter().all(|d| d.severity == Severity::Info), "{w:?}");
        assert!(
            w.iter().any(|d| d.code == "CG-132"),
            "loopback note expected"
        );
    }

    #[test]
    fn open_http_bind_and_tiny_datagram_are_noted() {
        let ingest = IngestConfig {
            http_bind: addr("0.0.0.0:8080"),
            bind_addr: addr("0.0.0.0:7301"),
            max_datagram_size: 32,
            ..IngestConfig::default()
        };
        let codes: Vec<_> = config_warnings(&ingest, &StationConfig::default())
            .into_iter()
            .map(|d| d.code)
            .collect();
        assert!(codes.contains(&"CG-131"));
        assert!(codes.contains(&"CG-133"));
        assert!(!codes.contains(&"CG-132"));
    }

    // ---- CG-2xx network ---------------------------------------------------------------------

    #[test]
    fn port_in_use_suggests_next_port_and_lookup_commands() {
        let d = explain_bind_error(
            Listener::Http,
            addr("127.0.0.1:8080"),
            &io::Error::from(io::ErrorKind::AddrInUse),
        );
        assert_eq!(d.code, "CG-201");
        assert!(
            d.fixes
                .iter()
                .any(|f| f.contains("ingest.http_bind = \"127.0.0.1:8081\""))
        );
        assert!(
            d.fixes
                .iter()
                .any(|f| f.contains("netstat") && f.contains("lsof"))
        );
    }

    #[test]
    fn port_in_use_at_max_port_does_not_overflow() {
        let d = explain_bind_error(
            Listener::Udp,
            addr("127.0.0.1:65535"),
            &io::Error::from(io::ErrorKind::AddrInUse),
        );
        assert!(d.fixes.iter().any(|f| f.contains(":65534")));
    }

    #[test]
    fn privileged_port_explains_admin_rights() {
        let d = explain_bind_error(
            Listener::Udp,
            addr("0.0.0.0:80"),
            &io::Error::from(io::ErrorKind::PermissionDenied),
        );
        assert_eq!(d.code, "CG-202");
        assert!(d.cause.contains("below 1024"));
    }

    #[test]
    fn unprivileged_permission_denied_blames_firewall() {
        let d = explain_bind_error(
            Listener::Udp,
            addr("0.0.0.0:7301"),
            &io::Error::from(io::ErrorKind::PermissionDenied),
        );
        assert!(d.cause.contains("firewall"));
    }

    #[test]
    fn foreign_ip_explains_wrong_network() {
        let d = explain_bind_error(
            Listener::Udp,
            addr("10.99.99.99:7301"),
            &io::Error::from(io::ErrorKind::AddrNotAvailable),
        );
        assert_eq!(d.code, "CG-203");
        assert!(d.fixes[0].contains("127.0.0.1:7301"));
    }

    #[test]
    fn real_port_conflict_is_explained() {
        let first = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
        let taken = first.local_addr().expect("addr");
        let err = std::net::UdpSocket::bind(taken).expect_err("second bind must fail");
        let d = explain_bind_error(Listener::Udp, taken, &err);
        assert_eq!(d.code, "CG-201", "{d}");
    }

    #[test]
    fn unknown_bind_error_is_generic_but_actionable() {
        let d = explain_bind_error(
            Listener::Udp,
            addr("127.0.0.1:7301"),
            &io::Error::other("weird"),
        );
        assert_eq!(d.code, "CG-204");
        assert!(d.cause.contains("weird"));
    }

    // ---- CG-3xx TLE / tracking --------------------------------------------------------------

    #[test]
    fn fresh_tle_produces_no_warning() {
        let now = Utc::now();
        assert!(check_tle_age(now - Duration::days(2), now).is_none());
        assert!(check_tle_age(now - Duration::hours(71), now).is_none());
    }

    #[test]
    fn tle_older_than_three_days_warns_about_doppler_and_pointing() {
        let now = Utc::now();
        let d = check_tle_age(now - Duration::hours(73), now).expect("just past 3 days");
        assert_eq!((d.code, d.severity), ("CG-301", Severity::Warning));
        assert!(d.summary.contains("3.0 days"), "{}", d.summary);
        assert!(d.cause.contains("Doppler") && d.cause.contains("bits 0"));
        assert!(d.fixes.iter().any(|f| f.contains("skipped")));
    }

    #[test]
    fn weeks_old_tle_is_still_the_doppler_warning_below_thirty_days() {
        let now = Utc::now();
        let d = check_tle_age(now - Duration::days(20), now).expect("aging");
        assert_eq!((d.code, d.severity), ("CG-301", Severity::Warning));
    }

    #[test]
    fn stale_tle_warns_about_false_anomalies() {
        let now = Utc::now();
        let d = check_tle_age(now - Duration::days(90), now).expect("stale");
        assert_eq!(d.code, "CG-302");
        assert!(d.cause.contains("flagged as anomalous"));
        assert!(d.summary.contains("90 days"));
    }

    #[test]
    fn future_tle_points_at_the_clock() {
        let now = Utc::now();
        let d = check_tle_age(now + Duration::days(10), now).expect("future");
        assert_eq!(d.code, "CG-303");
        assert!(d.fixes[0].contains("date and time"));
    }

    #[test]
    fn default_reference_tle_is_flagged_stale_today() {
        let epoch = tle_epoch(crate::config::DEFAULT_ISS_TLE).expect("parses");
        let d = check_tle_age(epoch, Utc::now()).expect("2020 TLE is stale");
        assert_eq!(d.code, "CG-302");
    }

    #[test]
    fn malformed_tle_uses_ephemerust_hint() {
        let err = crate::propagator::EphemerustPropagator::new("only one line", 0.0, 0.0, 0.0)
            .err()
            .expect("must fail");
        let d = explain_tracking_error(&err);
        assert_eq!(d.code, "CG-310");
        assert!(
            d.fixes.len() >= 2,
            "expected Ephemerust hint + refresh: {d}"
        );
    }

    #[test]
    fn checksum_error_is_explained() {
        // Last digit of line 1 changed from 2 to 3: checksum mismatch.
        let bad = "ISS (ZARYA)\n\
            1 25544U 98067A   20194.88612269 -.00002218  00000-0 -31515-4 0  9993\n\
            2 25544  51.6461 221.2784 0001413  89.1723 280.4612 15.49507896236008";
        let err = crate::propagator::EphemerustPropagator::new(bad, 0.0, 0.0, 0.0)
            .err()
            .expect("checksum must fail");
        let d = explain_tracking_error(&err);
        assert!(d.fixes.iter().any(|f| f.contains("Re-download")), "{d}");
    }

    #[test]
    fn tracking_error_from_config_delegates() {
        let station = StationConfig {
            tle: crate::config::TleSource::File("definitely/missing.tle".into()),
            ..StationConfig::default()
        };
        let err = crate::propagator::EphemerustPropagator::from_station(&station)
            .err()
            .expect("missing file");
        assert_eq!(explain_tracking_error(&err).code, "CG-118");
    }

    #[test]
    fn unknown_tracking_error_is_generic() {
        let d = explain_tracking_error(&anyhow::anyhow!("mystery"));
        assert_eq!(d.code, "CG-312");
        assert!(d.cause.contains("mystery"));
    }

    // ---- CG-4xx telemetry -------------------------------------------------------------------

    #[test]
    fn ccsds_errors_each_have_codes_and_fixes() {
        let cases = [
            (CcsdsError::TooShort { len: 3 }, "CG-401"),
            (
                CcsdsError::Truncated {
                    declared: 200,
                    available: 100,
                },
                "CG-403",
            ),
            (CcsdsError::NotTelemetry { apid: 7 }, "CG-404"),
        ];
        for (e, code) in cases {
            let d = e.explain();
            assert_eq!(d.code, code);
            assert!(!d.fixes.is_empty());
        }
    }

    #[test]
    fn truncated_packet_suggests_exact_minimum_size() {
        let d = CcsdsError::Truncated {
            declared: 1500,
            available: 512,
        }
        .explain();
        assert!(d.fixes[0].contains("at least 1500"));
    }

    #[test]
    fn hil_decode_errors_explain_apid_range() {
        let d = HilTmV1DecodeError::BadMagic { got: *b"ABCD" }.explain();
        assert_eq!(d.code, "CG-411");
        assert!(d.cause.contains("ABCD"));
        assert!(d.fixes[0].contains("hil_tm_v1_apid_min"));
        assert_eq!(
            HilTmV1DecodeError::TooShort { have: 3 }.explain().code,
            "CG-410"
        );
        assert_eq!(
            HilTmV1DecodeError::WrongVersion { got: 9 }.explain().code,
            "CG-412"
        );
        assert_eq!(HilTmV1DecodeError::NonZeroReserved.explain().code, "CG-413");
    }

    #[test]
    fn all_physics_flags_are_described() {
        assert_eq!(describe_physics_flags(0x7F).len(), 7);
        assert!(describe_physics_flags(0x80).is_empty(), "bit 7 is reserved");
        assert_eq!(
            describe_physics_flags(FLAG_ADCS_BODY_RATE_ANOMALY),
            vec!["Spacecraft is spinning faster than the configured limit."]
        );
    }

    // ---- CG-5xx health ----------------------------------------------------------------------

    fn ingest(frames: u64) -> IngestSnapshot {
        IngestSnapshot {
            frames_received: frames,
            bytes_received: frames * 30,
            oversized_dropped: 0,
            recv_errors: 0,
        }
    }

    fn gw() -> GatewayMetricsSnapshot {
        crate::metrics::GatewayMetrics::default().snapshot()
    }

    fn report(ing: &IngestSnapshot, g: &GatewayMetricsSnapshot, tracking: bool) -> HealthReport {
        assess_health(&HealthInputs {
            ingest: ing,
            gateway: g,
            tracking_enabled: tracking,
            udp_addr: Some(addr("127.0.0.1:7301")),
            http_addr: Some(addr("127.0.0.1:8080")),
        })
    }

    fn codes(r: &HealthReport) -> Vec<&'static str> {
        r.findings.iter().map(|d| d.code).collect()
    }

    #[test]
    fn health_waiting_names_the_udp_address() {
        let r = report(&ingest(0), &gw(), true);
        assert_eq!(r.status, OverallStatus::Waiting);
        assert!(r.headline.contains("127.0.0.1:7301"));
        assert_eq!(codes(&r), vec!["CG-501"]);
    }

    #[test]
    fn health_ok_when_frames_flow_to_a_dashboard() {
        let mut g = gw();
        g.telemetry_frames_emitted = 100;
        g.ws_clients_connected = 1;
        let r = report(&ingest(100), &g, true);
        assert_eq!(r.status, OverallStatus::Ok, "{:?}", r.findings);
        assert!(r.findings.is_empty());
    }

    #[test]
    fn health_problem_when_nothing_parses() {
        let mut g = gw();
        g.telemetry_parse_errors = 40;
        g.ws_clients_connected = 1;
        let r = report(&ingest(40), &g, true);
        assert_eq!(r.status, OverallStatus::Problem);
        assert_eq!(r.findings[0].code, "CG-502", "most severe first");
    }

    #[test]
    fn health_warns_on_high_reject_ratio_only() {
        let mut g = gw();
        g.ws_clients_connected = 1;
        g.telemetry_frames_emitted = 95;
        g.telemetry_parse_errors = 5;
        assert!(!codes(&report(&ingest(100), &g, true)).contains(&"CG-503"));
        g.telemetry_frames_emitted = 50;
        g.telemetry_parse_errors = 50;
        assert!(codes(&report(&ingest(100), &g, true)).contains(&"CG-503"));
    }

    #[test]
    fn health_reports_oversized_and_recv_errors() {
        let mut i = ingest(10);
        i.oversized_dropped = 2;
        i.recv_errors = 1;
        let mut g = gw();
        g.ws_clients_connected = 1;
        g.telemetry_frames_emitted = 10;
        let c = codes(&report(&i, &g, true));
        assert!(c.contains(&"CG-504") && c.contains(&"CG-505"));
    }

    #[test]
    fn health_tracking_disabled_vs_failing() {
        let mut g = gw();
        g.ws_clients_connected = 1;
        g.telemetry_frames_emitted = 10;
        g.tracking_errors = 10;
        assert!(codes(&report(&ingest(10), &g, false)).contains(&"CG-506"));
        let c = codes(&report(&ingest(10), &g, true));
        assert!(c.contains(&"CG-507") && !c.contains(&"CG-506"));
    }

    #[test]
    fn health_flags_mostly_anomalous_stream_as_setup_problem() {
        let mut g = gw();
        g.ws_clients_connected = 1;
        g.telemetry_frames_emitted = 20;
        g.anomaly_frames = 18;
        let r = report(&ingest(20), &g, true);
        assert_eq!(r.status, OverallStatus::Attention);
        assert!(codes(&r).contains(&"CG-508"));
        // A handful of anomalies is normal and not reported.
        g.anomaly_frames = 2;
        assert!(!codes(&report(&ingest(20), &g, true)).contains(&"CG-508"));
    }

    #[test]
    fn health_suggests_websocket_url_when_no_dashboard() {
        let mut g = gw();
        g.telemetry_frames_emitted = 0;
        let r = report(&ingest(5), &g, true);
        let d = r
            .findings
            .iter()
            .find(|d| d.code == "CG-509")
            .expect("no-dashboard note");
        assert!(d.fixes[0].contains("ws://127.0.0.1:8080/telemetry/openmct"));
    }

    #[test]
    fn health_serialize_errors_are_bugs() {
        let mut g = gw();
        g.ws_clients_connected = 1;
        g.telemetry_frames_emitted = 10;
        g.serialize_errors = 1;
        let r = report(&ingest(10), &g, true);
        assert_eq!(r.status, OverallStatus::Problem);
        assert!(r.findings[0].fixes[0].contains("github.com"));
    }

    #[test]
    fn health_report_serializes_status_lowercase() {
        let r = report(&ingest(0), &gw(), true);
        let v = serde_json::to_value(&r).expect("json");
        assert_eq!(v["status"], "waiting");
        assert_eq!(v["findings"][0]["code"], "CG-501");
    }
}
