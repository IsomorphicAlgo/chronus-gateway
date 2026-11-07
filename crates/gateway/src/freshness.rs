//! Keeping orbit and Earth-orientation data current (Tranche 4, `Methodology.md` D-052).
//!
//! Physics is only as good as its inputs. A TLE goes stale by kilometres a day, Earth orientation
//! changes by about a millisecond a day, and an OEM covers a fixed window. Before 0.6.0 all three
//! were read once at startup. Now:
//!
//! - **Files are reloaded when they change.** Every `freshness.reload_interval_s` the gateway
//!   checks `station.tle_file`, `station.oem_file` and `freshness.eop_file`. When one changed (or
//!   a new UTC day starts and the Earth-orientation values move on), it builds a new propagator
//!   and swaps it in without dropping a frame. A file that fails to load is reported and the
//!   previous data stays in use.
//! - **Earth orientation comes from an IERS file.** `freshness.eop_file` names an IERS
//!   `finals2000A` file (`finals.daily`, `finals2000A.daily`); the values for **today**
//!   (interpolated between daily rows) replace the fixed `ut1_minus_utc_s` / `polar_motion_*`
//!   settings.
//! - **Fetching is optional.** Any scheduler can refresh the files (`deploy/fetch-data.sh`). A
//!   build with the `refresh` cargo feature can fetch them itself from `freshness.tle_url` /
//!   `eop_url`, at most every two hours, with no retries (CelesTrak's published expectations).
//! - **Data age is visible.** [`DataStatus`] feeds the `data` object of `/metrics` and `/status`,
//!   the Prometheus gauges, and the `/status` findings (TLE age, OEM coverage, Earth orientation
//!   coverage, reload and fetch errors).

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::Serialize;

use crate::config::{EphemerisSource, StationConfig};
use crate::diagnose::{Diagnosis, Severity};
use crate::propagator::TrackingProvider;

// ---------------------------------------------------------------------------------------------
// IERS finals2000A
// ---------------------------------------------------------------------------------------------

/// One daily row of an IERS `finals2000A` file (Bulletin A columns).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct EopRow {
    /// Modified Julian Date (UTC, 0h) of the row.
    pub mjd: f64,
    /// Pole x, arcseconds.
    pub x_arcsec: f64,
    /// Pole y, arcseconds.
    pub y_arcsec: f64,
    /// UT1 − UTC, seconds.
    pub ut1_minus_utc_s: f64,
    /// Whether UT1 − UTC is a prediction (`P`) rather than an IERS value (`I`).
    pub predicted: bool,
}

/// Earth orientation at an instant, interpolated from an [`EopTable`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[non_exhaustive]
pub struct EopValues {
    /// UT1 − UTC, seconds.
    pub ut1_minus_utc_s: f64,
    /// Pole x, arcseconds.
    pub x_arcsec: f64,
    /// Pole y, arcseconds.
    pub y_arcsec: f64,
    /// Whether any value used was a prediction.
    pub predicted: bool,
}

/// Why an Earth-orientation file could not be used.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EopError {
    /// The file could not be read.
    #[error("cannot read the Earth orientation file {path}: {source}")]
    Read {
        /// The configured path.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A line does not have the `finals2000A` layout.
    #[error("line {line} is not in IERS finals2000A format: {reason}")]
    Format {
        /// 1-based line number.
        line: usize,
        /// What was wrong.
        reason: String,
    },
    /// No usable rows.
    #[error("the file has no Earth orientation rows")]
    Empty,
}

/// The rows of a `finals2000A` file, in date order.
#[derive(Debug, Clone, Default)]
pub struct EopTable {
    rows: Vec<EopRow>,
}

/// Modified Julian Date of the Unix epoch (1970-01-01).
const MJD_UNIX_EPOCH: f64 = 40_587.0;

fn mjd_of(t: DateTime<Utc>) -> f64 {
    MJD_UNIX_EPOCH + t.timestamp_millis() as f64 / 86_400_000.0
}

fn date_of_mjd(mjd: f64) -> Option<NaiveDate> {
    DateTime::from_timestamp(((mjd - MJD_UNIX_EPOCH) * 86_400.0).round() as i64, 0)
        .map(|t| t.date_naive())
}

fn field(line: &str, from: usize, to: usize) -> Option<&str> {
    line.get(from..to.min(line.len())).map(str::trim)
}

/// Parses an IERS `finals2000A` file, in either of the two forms the IERS publishes:
///
/// - **Fixed-width text** (`finals.daily`, `finals2000A.daily`, `finals2000A.all`). Columns used
///   (1-based, from the IERS format description): 1–6 date (YYMMDD), 8–15 MJD, 19–27 pole x,
///   38–46 pole y (arcsec), 58 I/P flag and 59–68 UT1−UTC (s).
/// - **Semicolon-separated CSV** from the IERS data centre (`finals2000A.daily.csv`,
///   `finals.daily.iau2000.csv`), recognised by its `MJD;Year;Month;Day;...` header. Columns are
///   found by name (`MJD`, `Year`, `Month`, `Day`, `x_pole`, `y_pole`, `UT1-UTC`, and the `Type`
///   just before `UT1-UTC`: `final` or `prediction`).
///
/// Either way, every row's calendar date is cross-checked against its MJD, so a file in another
/// layout is rejected rather than misread. Rows past the end of the predictions (blank values) are
/// skipped.
///
/// # Errors
///
/// [`EopError::Format`] on the first line that is not in the format, [`EopError::Empty`] when no
/// row has values.
///
/// # Examples
///
/// ```
/// use chronus_gateway::freshness::parse_finals;
///
/// let line = "261024 61337.00 I  0.212345 0.000091  0.345678 0.000088  I-0.0432100 0.0000150  0.1234 0.0104";
/// let t = parse_finals(line).unwrap();
/// assert_eq!(t.len(), 1);
/// ```
pub fn parse_finals(text: &str) -> Result<EopTable, EopError> {
    // Tolerate a UTF-8 byte-order mark (spreadsheet exports add one).
    let text = text.trim_start_matches('\u{feff}');
    if text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .is_some_and(|l| l.starts_with("MJD;"))
    {
        return parse_finals_csv(text);
    }
    let mut rows: Vec<EopRow> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if line.trim().is_empty() {
            continue;
        }
        let bad = |reason: &str| EopError::Format {
            line: n,
            reason: reason.to_string(),
        };
        let (Some(yy), Some(mm), Some(dd), Some(mjd)) = (
            field(line, 0, 2),
            field(line, 2, 4),
            field(line, 4, 6),
            field(line, 7, 15),
        ) else {
            return Err(bad("line too short for the date and MJD columns"));
        };
        let mjd: f64 = mjd
            .parse()
            .map_err(|_| bad("columns 8-15 are not an MJD"))?;
        let (Ok(yy), Ok(mm), Ok(dd)) = (yy.parse::<i32>(), mm.parse::<u32>(), dd.parse::<u32>())
        else {
            return Err(bad("columns 1-6 are not a YYMMDD date"));
        };
        let year = if mjd <= 51_543.0 {
            1900 + yy
        } else {
            2000 + yy
        };
        let date = NaiveDate::from_ymd_opt(year, mm, dd).ok_or_else(|| bad("invalid date"))?;
        if date_of_mjd(mjd) != Some(date) || mjd.fract() != 0.0 {
            return Err(bad(
                "the date in columns 1-6 does not match the MJD in columns 8-15",
            ));
        }
        // Past the prediction horizon the value columns are blank: stop using the file there.
        let (Some(x), Some(y), Some(flag), Some(dut1)) = (
            field(line, 18, 27).filter(|s| !s.is_empty()),
            field(line, 37, 46).filter(|s| !s.is_empty()),
            field(line, 57, 58).filter(|s| !s.is_empty()),
            field(line, 58, 68).filter(|s| !s.is_empty()),
        ) else {
            continue;
        };
        let parse = |s: &str, what: &str| {
            s.parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| bad(&format!("{what} is not a number")))
        };
        let row = EopRow {
            mjd,
            x_arcsec: parse(x, "pole x (columns 19-27)")?,
            y_arcsec: parse(y, "pole y (columns 38-46)")?,
            ut1_minus_utc_s: parse(dut1, "UT1-UTC (columns 59-68)")?,
            predicted: flag != "I",
        };
        push_row(&mut rows, row, n)?;
    }
    finish(rows)
}

/// Range and order checks shared by both forms.
fn push_row(rows: &mut Vec<EopRow>, row: EopRow, line: usize) -> Result<(), EopError> {
    let bad = |reason: &str| EopError::Format {
        line,
        reason: reason.to_string(),
    };
    if row.x_arcsec.abs() > 1.0 || row.y_arcsec.abs() > 1.0 || row.ut1_minus_utc_s.abs() > 1.0 {
        return Err(bad(
            "values out of range (pole within 1 arcsec, |UT1-UTC| < 1 s)",
        ));
    }
    if rows.last().is_some_and(|last| last.mjd >= row.mjd) {
        return Err(bad("rows are not in date order"));
    }
    rows.push(row);
    Ok(())
}

fn finish(rows: Vec<EopRow>) -> Result<EopTable, EopError> {
    if rows.is_empty() {
        return Err(EopError::Empty);
    }
    Ok(EopTable { rows })
}

/// The IERS data centre's semicolon-separated form of `finals2000A`.
fn parse_finals_csv(text: &str) -> Result<EopTable, EopError> {
    let mut lines = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty());
    let Some((header_idx, header)) = lines.next() else {
        return Err(EopError::Empty);
    };
    let names: Vec<&str> = header.trim().split(';').map(str::trim).collect();
    let col = |name: &str| {
        names
            .iter()
            .position(|c| *c == name)
            .ok_or_else(|| EopError::Format {
                line: header_idx + 1,
                reason: format!("CSV header has no `{name}` column"),
            })
    };
    let (i_mjd, i_year, i_month, i_day) = (col("MJD")?, col("Year")?, col("Month")?, col("Day")?);
    let (i_x, i_y, i_dut1) = (col("x_pole")?, col("y_pole")?, col("UT1-UTC")?);
    // The UT1-UTC flag is the `Type` column immediately before `UT1-UTC`.
    let i_flag = i_dut1
        .checked_sub(1)
        .filter(|&i| names[i] == "Type")
        .ok_or_else(|| EopError::Format {
            line: header_idx + 1,
            reason: "CSV header has no `Type` column before `UT1-UTC`".into(),
        })?;

    let mut rows = Vec::new();
    for (i, line) in lines {
        let n = i + 1;
        let bad = |reason: String| EopError::Format { line: n, reason };
        let cells: Vec<&str> = line.trim().split(';').map(str::trim).collect();
        let get = |i: usize| cells.get(i).copied().filter(|s| !s.is_empty());
        let num = |i: usize, what: &str| -> Result<Option<f64>, EopError> {
            get(i)
                .map(|s| {
                    s.parse::<f64>()
                        .ok()
                        .filter(|v| v.is_finite())
                        .ok_or_else(|| bad(format!("{what} `{s}` is not a number")))
                })
                .transpose()
        };
        let (Some(mjd), Some(year), Some(month), Some(day)) = (
            num(i_mjd, "MJD")?,
            num(i_year, "Year")?,
            num(i_month, "Month")?,
            num(i_day, "Day")?,
        ) else {
            return Err(bad("row is missing its MJD or date".into()));
        };
        let date = NaiveDate::from_ymd_opt(year as i32, month as u32, day as u32)
            .ok_or_else(|| bad("invalid date".into()))?;
        if date_of_mjd(mjd) != Some(date) || mjd.fract() != 0.0 {
            return Err(bad("the date does not match the MJD".into()));
        }
        let (Some(x), Some(y), Some(dut1)) = (
            num(i_x, "x_pole")?,
            num(i_y, "y_pole")?,
            num(i_dut1, "UT1-UTC")?,
        ) else {
            continue; // past the prediction horizon
        };
        let row = EopRow {
            mjd,
            x_arcsec: x,
            y_arcsec: y,
            ut1_minus_utc_s: dut1,
            predicted: get(i_flag) != Some("final"),
        };
        push_row(&mut rows, row, n)?;
    }
    finish(rows)
}

impl EopTable {
    /// Reads and parses a `finals2000A` file.
    ///
    /// # Errors
    ///
    /// [`EopError`] when the file cannot be read or parsed.
    pub fn from_file(path: &Path) -> Result<Self, EopError> {
        let text = std::fs::read_to_string(path).map_err(|source| EopError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        parse_finals(&text)
    }

    /// Number of daily rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// `true` when there are no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// First and last day covered.
    #[must_use]
    pub fn span(&self) -> Option<(NaiveDate, NaiveDate)> {
        Some((
            date_of_mjd(self.rows.first()?.mjd)?,
            date_of_mjd(self.rows.last()?.mjd)?,
        ))
    }

    /// Last day with an IERS (non-predicted) UT1−UTC value.
    #[must_use]
    pub fn observed_until(&self) -> Option<NaiveDate> {
        self.rows
            .iter()
            .rev()
            .find(|r| !r.predicted)
            .and_then(|r| date_of_mjd(r.mjd))
    }

    /// Earth orientation at `t`, linearly interpolated between the daily rows around it.
    /// `None` outside the file. Across a leap second (a UT1−UTC jump of more than half a second)
    /// the earlier day's value is used instead of interpolating through the jump.
    #[must_use]
    pub fn at(&self, t: DateTime<Utc>) -> Option<EopValues> {
        let m = mjd_of(t);
        let i = self.rows.partition_point(|r| r.mjd <= m).checked_sub(1)?;
        let a = self.rows[i];
        let Some(b) = self.rows.get(i + 1).filter(|b| b.mjd - a.mjd <= 1.0 + 1e-9) else {
            // The last row covers its own day only.
            return (m - a.mjd < 1.0).then_some(EopValues {
                ut1_minus_utc_s: a.ut1_minus_utc_s,
                x_arcsec: a.x_arcsec,
                y_arcsec: a.y_arcsec,
                predicted: a.predicted,
            });
        };
        let f = (m - a.mjd) / (b.mjd - a.mjd);
        let lerp = |x: f64, y: f64| x + (y - x) * f;
        let dut1 = if (b.ut1_minus_utc_s - a.ut1_minus_utc_s).abs() > 0.5 {
            a.ut1_minus_utc_s
        } else {
            lerp(a.ut1_minus_utc_s, b.ut1_minus_utc_s)
        };
        Some(EopValues {
            ut1_minus_utc_s: dut1,
            x_arcsec: lerp(a.x_arcsec, b.x_arcsec),
            y_arcsec: lerp(a.y_arcsec, b.y_arcsec),
            predicted: a.predicted || b.predicted,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------------------------

/// What data the physics is running on, and how old it is (`data` in `/metrics` and `/status`).
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct DataSnapshot {
    /// `tle_inline`, `tle_file`, `oem_file`, or `none` (physics off).
    pub ephemeris_source: &'static str,
    /// The ephemeris file, when there is one.
    pub ephemeris_path: Option<String>,
    /// Name from the TLE title line.
    pub tle_name: Option<String>,
    /// TLE epoch.
    pub tle_epoch: Option<DateTime<Utc>>,
    /// Age of the TLE now, days (negative = dated in the future).
    pub tle_age_days: Option<f64>,
    /// OEM coverage start.
    pub oem_start: Option<DateTime<Utc>>,
    /// OEM coverage end.
    pub oem_stop: Option<DateTime<Utc>>,
    /// Hours of OEM coverage left from now (negative = already ended).
    pub oem_hours_remaining: Option<f64>,
    /// `file` (`freshness.eop_file`), `config` (fixed values), or `none` (all zero).
    pub eop_source: &'static str,
    /// The Earth orientation file, when there is one.
    pub eop_path: Option<String>,
    /// UT1 − UTC in use, seconds.
    pub ut1_minus_utc_s: f64,
    /// Pole x in use, arcseconds.
    pub polar_motion_x_arcsec: f64,
    /// Pole y in use, arcseconds.
    pub polar_motion_y_arcsec: f64,
    /// Whether the values in use are IERS predictions.
    pub eop_predicted: Option<bool>,
    /// Day the Earth orientation values were last taken from the file for.
    pub eop_applied_for: Option<NaiveDate>,
    /// Last day the file covers.
    pub eop_file_last_day: Option<NaiveDate>,
    /// Last day with observed (non-predicted) values.
    pub eop_observed_until: Option<NaiveDate>,
    /// When data was last (re)loaded.
    pub loaded_at: Option<DateTime<Utc>>,
    /// Successful reloads after startup.
    pub reloads: u64,
    /// Failed reloads (the previous data stayed in use).
    pub reload_errors: u64,
    /// The last reload error.
    pub last_reload_error: Option<String>,
    /// Successful fetches (`refresh` feature).
    pub fetches: u64,
    /// Failed fetches.
    pub fetch_errors: u64,
    /// When data was last fetched.
    pub last_fetch: Option<DateTime<Utc>>,
    /// The last fetch error.
    pub last_fetch_error: Option<String>,
}

/// Shared, updatable [`DataSnapshot`].
#[derive(Debug, Default)]
pub struct DataStatus {
    inner: RwLock<DataSnapshot>,
}

impl DataStatus {
    fn update(&self, f: impl FnOnce(&mut DataSnapshot)) {
        let mut g = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut g);
    }

    /// The current snapshot with ages computed for `now`.
    #[must_use]
    pub fn snapshot(&self, now: DateTime<Utc>) -> DataSnapshot {
        let mut s = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        s.tle_age_days = s
            .tle_epoch
            .map(|e| (now - e).num_seconds() as f64 / 86_400.0);
        s.oem_hours_remaining = s.oem_stop.map(|e| (e - now).num_seconds() as f64 / 3_600.0);
        s
    }

    /// Records a fetch result (`refresh` feature, or an embedder's own fetcher).
    pub fn record_fetch(&self, now: DateTime<Utc>, result: Result<(), String>) {
        self.update(|s| match result {
            Ok(()) => {
                s.fetches += 1;
                s.last_fetch = Some(now);
                s.last_fetch_error = None;
            }
            Err(e) => {
                s.fetch_errors += 1;
                s.last_fetch_error = Some(e);
            }
        });
    }
}

/// Plain-English findings about data age and freshness problems (`CG-3xx`, `CG-516`, `CG-517`).
#[must_use]
pub fn assess_data(d: &DataSnapshot, now: DateTime<Utc>) -> Vec<Diagnosis> {
    let mut out = Vec::new();
    if let Some(epoch) = d.tle_epoch
        && let Some(diag) = crate::diagnose::check_tle_age(epoch, now)
    {
        out.push(diag);
    }
    if let (Some(start), Some(stop)) = (d.oem_start, d.oem_stop)
        && let Some(diag) = crate::diagnose::check_oem_coverage((start, stop), now)
    {
        out.push(diag);
    }
    if d.eop_source == "file"
        && let Some(last) = d.eop_file_last_day
        && now.date_naive() > last
    {
        out.push(
            Diagnosis::new(
                "CG-517",
                Severity::Warning,
                format!(
                    "The Earth orientation file ends on {last}; the values from that day are still in use."
                ),
                "UT1-UTC drifts by about a millisecond a day (roughly half a metre of Earth \
                 rotation), so the physics slowly loses accuracy until the file is refreshed.",
            )
            .fix("Download a current `finals.daily` from the IERS (deploy/fetch-data.sh does this) into `freshness.eop_file`; the gateway reloads it within `freshness.reload_interval_s`."),
        );
    }
    if d.reload_errors > 0
        && let Some(e) = &d.last_reload_error
    {
        out.push(
            Diagnosis::new(
                "CG-516",
                Severity::Warning,
                format!("A changed data file could not be reloaded: {e}"),
                "The previous orbit and Earth orientation data are still in use.",
            )
            .fix("Fix or replace the file; the gateway retries on its next check."),
        );
    }
    if d.fetch_errors > 0
        && let Some(e) = &d.last_fetch_error
    {
        out.push(
            Diagnosis::new(
                "CG-516",
                Severity::Warning,
                format!("Fetching fresh data failed: {e}"),
                "The gateway will try again at the next fetch interval (it never retries in a \
                 loop). The data already on disk stays in use.",
            )
            .fix("Check the URL and the network; the provider may also be rate-limiting."),
        );
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The refresher
// ---------------------------------------------------------------------------------------------

/// Why data could not be (re)loaded.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FreshnessError {
    /// The Earth-orientation file is unusable.
    #[error(transparent)]
    Eop(#[from] EopError),
    /// The ephemeris could not be loaded or propagated.
    #[error("{0}")]
    Ephemeris(String),
}

/// File identity used to notice changes: modification time and size.
type Stamp = Option<(SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

fn ephemeris_path(station: &StationConfig) -> Option<&Path> {
    match &station.tracking.ephemeris {
        EphemerisSource::TleFile(p) | EphemerisSource::OemFile(p) => Some(p),
        _ => None,
    }
}

/// Watches the data files and swaps in a new propagator when they change.
#[derive(Debug)]
pub struct Refresher {
    base: StationConfig,
    status: Arc<DataStatus>,
    eop: Option<EopTable>,
    eph_stamp: Stamp,
    eop_stamp: Stamp,
    applied_day: Option<NaiveDate>,
}

impl Refresher {
    /// Loads the Earth-orientation file (if configured), and returns the refresher plus the
    /// **effective** station configuration: `base` with today's Earth orientation applied. Build
    /// the propagator from the returned configuration.
    ///
    /// # Errors
    ///
    /// [`FreshnessError::Eop`] when `freshness.eop_file` is set but unreadable or not in
    /// `finals2000A` format. (A file that merely does not cover today is accepted and reported on
    /// `/status`.)
    pub fn new(
        base: &StationConfig,
        status: Arc<DataStatus>,
        now: DateTime<Utc>,
    ) -> Result<(Self, StationConfig), FreshnessError> {
        let eop = match &base.freshness.eop_file {
            Some(p) => Some(EopTable::from_file(p)?),
            None => None,
        };
        let mut r = Self {
            eph_stamp: ephemeris_path(base).and_then(stamp),
            eop_stamp: base.freshness.eop_file.as_deref().and_then(stamp),
            base: base.clone(),
            status,
            eop,
            applied_day: None,
        };
        let effective = r.effective(now);
        r.describe(&effective, now);
        Ok((r, effective))
    }

    /// The base configuration with Earth orientation for `now` from the file, if any.
    fn effective(&mut self, now: DateTime<Utc>) -> StationConfig {
        let mut s = self.base.clone();
        if let Some(table) = &self.eop {
            // Outside the file, the nearest end of it is the best estimate available.
            let v = table.at(now).or_else(|| {
                let (first, last) = table.span()?;
                let edge = if now.date_naive() < first {
                    first
                } else {
                    last
                };
                table.at(edge.and_hms_opt(0, 0, 0)?.and_utc())
            });
            if let Some(v) = v {
                s.tracking.ut1_minus_utc_s = v.ut1_minus_utc_s;
                s.tracking.polar_motion_x_arcsec = v.x_arcsec;
                s.tracking.polar_motion_y_arcsec = v.y_arcsec;
            }
            self.applied_day = Some(now.date_naive());
        }
        s
    }

    /// Records what `station` holds in the shared status.
    fn describe(&self, station: &StationConfig, now: DateTime<Utc>) {
        let (source, path) = match &station.tracking.ephemeris {
            EphemerisSource::TleInline(_) => ("tle_inline", None),
            EphemerisSource::TleFile(p) => ("tle_file", Some(p.display().to_string())),
            EphemerisSource::OemFile(p) => ("oem_file", Some(p.display().to_string())),
        };
        let (mut tle_name, mut tle_epoch, mut oem) = (None, None, None);
        if source == "oem_file" {
            if let Some(p) = ephemeris_path(station) {
                oem = crate::oem::OemEphemeris::from_file(p)
                    .ok()
                    .map(|e| e.coverage());
            }
        } else if let Ok(text) = station.resolve_tle_text() {
            tle_epoch = crate::diagnose::tle_epoch(&text);
            tle_name = text
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .filter(|l| !l.starts_with("1 "))
                .map(str::to_string);
        }
        let t = &station.tracking;
        let eop_source = if self.eop.is_some() {
            "file"
        } else if t.ut1_minus_utc_s != 0.0
            || t.polar_motion_x_arcsec != 0.0
            || t.polar_motion_y_arcsec != 0.0
        {
            "config"
        } else {
            "none"
        };
        let predicted = self
            .eop
            .as_ref()
            .and_then(|e| e.at(now))
            .map(|v| v.predicted);
        let span = self.eop.as_ref().and_then(EopTable::span);
        let observed = self.eop.as_ref().and_then(EopTable::observed_until);
        self.status.update(|s| {
            s.ephemeris_source = source;
            s.ephemeris_path = path;
            s.tle_name = tle_name;
            s.tle_epoch = tle_epoch;
            s.oem_start = oem.map(|c| c.0);
            s.oem_stop = oem.map(|c| c.1);
            s.eop_source = eop_source;
            s.eop_path = station
                .freshness
                .eop_file
                .as_ref()
                .map(|p| p.display().to_string());
            s.ut1_minus_utc_s = t.ut1_minus_utc_s;
            s.polar_motion_x_arcsec = t.polar_motion_x_arcsec;
            s.polar_motion_y_arcsec = t.polar_motion_y_arcsec;
            s.eop_predicted = predicted;
            s.eop_applied_for = self.applied_day;
            s.eop_file_last_day = span.map(|s| s.1);
            s.eop_observed_until = observed;
            s.loaded_at = Some(now);
        });
    }

    /// Marks physics as off (no propagator could be built at startup).
    pub fn mark_physics_off(&self) {
        self.status.update(|s| s.ephemeris_source = "none");
    }

    /// Checks the files once. When the ephemeris or Earth-orientation file changed, or a new UTC
    /// day began with an Earth-orientation file in use, builds a new propagator and installs it
    /// in `tracking`. Returns whether it did.
    ///
    /// # Errors
    ///
    /// When a changed file cannot be loaded. The previous data stays in use and the error is
    /// recorded for `/status`.
    pub fn check(
        &mut self,
        tracking: &TrackingProvider,
        now: DateTime<Utc>,
    ) -> Result<bool, FreshnessError> {
        let eph_now = ephemeris_path(&self.base).and_then(stamp);
        let eop_now = self.base.freshness.eop_file.as_deref().and_then(stamp);
        let eph_changed = eph_now != self.eph_stamp;
        let eop_changed = eop_now != self.eop_stamp;
        let new_day = self.eop.is_some() && self.applied_day != Some(now.date_naive());
        if !(eph_changed || eop_changed || new_day) {
            return Ok(false);
        }
        let result = self.reload(tracking, now, eop_changed);
        // Remember the stamps either way: a broken file is reported once, not every interval,
        // and is picked up again as soon as it changes.
        self.eph_stamp = eph_now;
        self.eop_stamp = eop_now;
        match &result {
            Ok(()) => self.status.update(|s| {
                s.reloads += 1;
                s.last_reload_error = None;
            }),
            Err(e) => {
                let msg = e.to_string();
                tracing::warn!(error = %msg, "data reload failed; keeping the previous data");
                self.status.update(|s| {
                    s.reload_errors += 1;
                    s.last_reload_error = Some(msg);
                });
            }
        }
        result.map(|()| true)
    }

    fn reload(
        &mut self,
        tracking: &TrackingProvider,
        now: DateTime<Utc>,
        eop_changed: bool,
    ) -> Result<(), FreshnessError> {
        let previous_eop = if eop_changed {
            let path = self.base.freshness.eop_file.clone();
            let fresh = match path {
                Some(p) => Some(EopTable::from_file(&p)?),
                None => None,
            };
            Some(std::mem::replace(&mut self.eop, fresh))
        } else {
            None
        };
        let effective = self.effective(now);
        match crate::propagator::propagator_from_station(&effective) {
            Ok(p) => {
                tracking.replace(p);
                self.describe(&effective, now);
                tracing::info!(
                    ut1_minus_utc_s = effective.tracking.ut1_minus_utc_s,
                    "orbit / Earth orientation data reloaded"
                );
                Ok(())
            }
            Err(e) => {
                if let Some(prev) = previous_eop {
                    self.eop = prev;
                }
                Err(FreshnessError::Ephemeris(e.to_string()))
            }
        }
    }
}

/// Runs [`Refresher::check`] every `freshness.reload_interval_s` until `shutdown` (no-op when the
/// interval is 0 or there is no propagator).
pub async fn run(
    mut refresher: Refresher,
    tracking: Option<Arc<TrackingProvider>>,
    shutdown: impl std::future::Future<Output = ()>,
) {
    let secs = refresher.base.freshness.reload_interval_s;
    let Some(tracking) = tracking.filter(|_| secs > 0) else {
        return;
    };
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(secs));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // the first tick is immediate; startup already loaded everything
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            _ = tick.tick() => {
                // File reads are small; a blocking read here is bounded and rare.
                let _ = refresher.check(&tracking, Utc::now());
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Fetching (the `refresh` feature)
// ---------------------------------------------------------------------------------------------

/// Writes `text` to `path` atomically (temp file in the same folder, then rename).
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("download.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Title line of the configured TLE, used to pick the same object out of a fetched bulletin.
#[must_use]
pub fn configured_tle_name(station: &StationConfig) -> Option<String> {
    if let Some(n) = &station.freshness.tle_name {
        return Some(n.clone());
    }
    let text = station.resolve_tle_text().ok()?;
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .filter(|l| !l.starts_with("1 "))
        .map(str::to_string)
}

/// Picks one element set out of a bulletin and writes it (title + two lines) to `path`.
///
/// # Errors
///
/// A message when the bulletin has no matching set or the file cannot be written.
pub fn store_tle(bulletin: &str, selector: Option<&str>, path: &Path) -> Result<(), String> {
    let tle = ephemerust::satellite::select_tle(bulletin, selector).map_err(|e| e.to_string())?;
    let mut text = String::new();
    if let Some(n) = &tle.name {
        text.push_str(n);
        text.push('\n');
    }
    text.push_str(&tle.line1);
    text.push('\n');
    text.push_str(&tle.line2);
    text.push('\n');
    write_atomically(path, &text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Checks that `text` is a usable `finals2000A` file and writes it to `path`. A server error page
/// or a truncated download never replaces a good file.
///
/// # Errors
///
/// A message when the text does not parse or the file cannot be written.
pub fn store_eop(text: &str, path: &Path) -> Result<(), String> {
    parse_finals(text).map_err(|e| format!("downloaded Earth orientation data rejected: {e}"))?;
    write_atomically(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Whether `path` is missing or older than `max_age`.
#[must_use]
pub fn needs_fetch(path: &Path, max_age: std::time::Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_none_or(|age| age >= max_age)
}

/// Fetches whatever is due, once (`refresh` feature). Blocking: call from `spawn_blocking`.
#[cfg(feature = "refresh")]
pub fn fetch_due(station: &StationConfig, status: &DataStatus, force: bool) {
    let f = &station.freshness;
    let max_age = std::time::Duration::from_secs_f64(f.fetch_interval_h * 3_600.0);
    if let (Some(url), EphemerisSource::TleFile(path)) = (&f.tle_url, &station.tracking.ephemeris)
        && (force || needs_fetch(path, max_age))
    {
        let selector = configured_tle_name(station);
        let r = ephemerust::net::fetch_tle_text(url)
            .map_err(|e| e.to_string())
            .and_then(|text| store_tle(&text, selector.as_deref(), path));
        if let Err(e) = &r {
            tracing::warn!(url, error = %e, "TLE fetch failed (no retry until the next interval)");
        } else {
            tracing::info!(url, path = %path.display(), "TLE fetched");
        }
        status.record_fetch(Utc::now(), r);
    }
    if let (Some(url), Some(path)) = (&f.eop_url, &f.eop_file)
        && (force || needs_fetch(path, max_age))
    {
        let r = ephemerust::net::fetch_tle_text(url)
            .map_err(|e| e.to_string())
            .and_then(|text| store_eop(&text, path));
        if let Err(e) = &r {
            tracing::warn!(url, error = %e, "Earth orientation fetch failed (no retry until the next interval)");
        } else {
            tracing::info!(url, path = %path.display(), "Earth orientation data fetched");
        }
        status.record_fetch(Utc::now(), r);
    }
}

/// Fetches on a schedule until `shutdown` (`refresh` feature). The reload loop picks up the files.
#[cfg(feature = "refresh")]
pub async fn run_fetcher(
    station: Arc<StationConfig>,
    status: Arc<DataStatus>,
    shutdown: impl std::future::Future<Output = ()>,
) {
    let f = &station.freshness;
    if f.tle_url.is_none() && f.eop_url.is_none() {
        return;
    }
    let mut tick = tokio::time::interval(std::time::Duration::from_secs_f64(
        f.fetch_interval_h * 3_600.0,
    ));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // startup fetched already
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            _ = tick.tick() => {
                let (st, ss) = (Arc::clone(&station), Arc::clone(&status));
                let _ = tokio::task::spawn_blocking(move || fetch_due(&st, &ss, true)).await;
            }
        }
    }
}

/// The year of a date as a two-digit `finals2000A` field (used by tests and fixture tools).
#[doc(hidden)]
#[must_use]
pub fn finals_line(date: NaiveDate, x: f64, y: f64, dut1: f64, predicted: bool) -> String {
    let mjd = (date - NaiveDate::from_ymd_opt(1858, 11, 17).unwrap_or_default()).num_days();
    let flag = if predicted { 'P' } else { 'I' };
    format!(
        "{:02}{:02}{:02} {:8.2} {flag} {:9.6}{:9.6} {:9.6}{:9.6}  {flag}{:10.7}{:10.7}",
        date.year() % 100,
        date.month(),
        date.day(),
        mjd as f64,
        x,
        0.000_091,
        y,
        0.000_088,
        dut1,
        0.000_015
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn sample() -> String {
        [
            finals_line(d(2026, 10, 20), 0.20, 0.30, -0.040, false),
            finals_line(d(2026, 10, 21), 0.21, 0.31, -0.041, false),
            finals_line(d(2026, 10, 22), 0.22, 0.32, -0.042, true),
            // Beyond the predictions: date and MJD only.
            "261023 61336.00".to_string(),
        ]
        .join("\n")
    }

    #[test]
    fn layout_matches_the_documented_columns() {
        let l = finals_line(d(2026, 10, 24), 0.212345, 0.345678, -0.04321, false);
        assert_eq!(&l[0..6], "261024");
        assert_eq!(l[7..15].trim(), "61337.00");
        assert_eq!(&l[16..17], "I");
        assert_eq!(l[18..27].trim(), "0.212345");
        assert_eq!(l[37..46].trim(), "0.345678");
        assert_eq!(&l[57..58], "I");
        assert_eq!(l[58..68].trim(), "-0.0432100");
    }

    #[test]
    fn mjd_conversions_agree_with_known_dates() {
        // MJD 51544 = 2000-01-01, MJD 40587 = 1970-01-01.
        assert_eq!(date_of_mjd(51_544.0), Some(d(2000, 1, 1)));
        assert_eq!(date_of_mjd(40_587.0), Some(d(1970, 1, 1)));
        let t = Utc.with_ymd_and_hms(2000, 1, 1, 12, 0, 0).unwrap();
        assert!((mjd_of(t) - 51_544.5).abs() < 1e-9);
    }

    #[test]
    fn parses_interpolates_and_reports_coverage() {
        let t = parse_finals(&sample()).unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(t.span(), Some((d(2026, 10, 20), d(2026, 10, 22))));
        assert_eq!(t.observed_until(), Some(d(2026, 10, 21)));
        let noon = Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap();
        let v = t.at(noon).unwrap();
        assert!((v.ut1_minus_utc_s - -0.0405).abs() < 1e-12);
        assert!((v.x_arcsec - 0.205).abs() < 1e-12);
        assert!(!v.predicted);
        let v = t
            .at(Utc.with_ymd_and_hms(2026, 10, 22, 6, 0, 0).unwrap())
            .unwrap();
        assert!(v.predicted, "the last day is a prediction");
        assert!(
            t.at(Utc.with_ymd_and_hms(2026, 10, 19, 0, 0, 0).unwrap())
                .is_none()
        );
        assert!(
            t.at(Utc.with_ymd_and_hms(2026, 10, 23, 1, 0, 0).unwrap())
                .is_none()
        );
    }

    #[test]
    fn leap_second_is_not_interpolated_through() {
        let text = [
            finals_line(d(2016, 12, 31), 0.1, 0.2, -0.40, false),
            finals_line(d(2017, 1, 1), 0.1, 0.2, 0.59, false),
        ]
        .join("\n");
        let t = parse_finals(&text).unwrap();
        let v = t
            .at(Utc.with_ymd_and_hms(2016, 12, 31, 18, 0, 0).unwrap())
            .unwrap();
        assert_eq!(v.ut1_minus_utc_s, -0.40);
    }

    #[test]
    fn other_layouts_and_bad_values_are_rejected_with_the_line_number() {
        // Date and MJD disagree: a shifted column layout.
        let shifted = format!(" {}", finals_line(d(2026, 10, 20), 0.2, 0.3, -0.04, false));
        assert!(matches!(
            parse_finals(&shifted),
            Err(EopError::Format { line: 1, .. })
        ));
        let wrong_mjd =
            finals_line(d(2026, 10, 20), 0.2, 0.3, -0.04, false).replace("61333", "61334");
        let e = parse_finals(&wrong_mjd).unwrap_err();
        assert!(e.to_string().contains("does not match"), "{e}");
        let big = finals_line(d(2026, 10, 20), 0.2, 0.3, -1.5, false);
        assert!(
            parse_finals(&big)
                .unwrap_err()
                .to_string()
                .contains("out of range")
        );
        let unordered = [
            finals_line(d(2026, 10, 21), 0.2, 0.3, -0.04, false),
            finals_line(d(2026, 10, 20), 0.2, 0.3, -0.04, false),
        ]
        .join("\n");
        assert!(
            parse_finals(&unordered)
                .unwrap_err()
                .to_string()
                .contains("order")
        );
        assert!(matches!(parse_finals("\n\n"), Err(EopError::Empty)));
        assert!(parse_finals("<html>Service unavailable</html>").is_err());
    }

    fn station_with_files(dir: &Path) -> StationConfig {
        let tle = dir.join("iss.tle");
        std::fs::write(&tle, crate::config::DEFAULT_ISS_TLE).unwrap();
        let eop = dir.join("finals.daily");
        std::fs::write(&eop, sample()).unwrap();
        let mut s = StationConfig::default();
        s.tracking.ephemeris = EphemerisSource::TleFile(tle);
        s.freshness.eop_file = Some(eop);
        s
    }

    #[test]
    fn refresher_applies_todays_eop_and_reloads_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let base = station_with_files(dir.path());
        let status = Arc::new(DataStatus::default());
        let now = Utc.with_ymd_and_hms(2026, 10, 21, 0, 0, 0).unwrap();
        let (mut r, eff) = Refresher::new(&base, Arc::clone(&status), now).unwrap();
        assert!((eff.tracking.ut1_minus_utc_s - -0.041).abs() < 1e-12);
        let snap = status.snapshot(now);
        assert_eq!(snap.eop_source, "file");
        assert_eq!(snap.ephemeris_source, "tle_file");
        assert_eq!(snap.tle_name.as_deref(), Some("ISS (ZARYA)"));
        assert!(snap.tle_age_days.unwrap() > 2000.0);
        assert_eq!(snap.eop_observed_until, Some(d(2026, 10, 21)));

        let provider =
            TrackingProvider::new(crate::propagator::propagator_from_station(&eff).unwrap(), 0);
        assert!(!r.check(&provider, now).unwrap(), "nothing changed");
        // A new day re-applies Earth orientation even without a file change.
        let next = now + chrono::Duration::days(1);
        assert!(r.check(&provider, next).unwrap());
        assert!((status.snapshot(next).ut1_minus_utc_s - -0.042).abs() < 1e-12);

        // A broken TLE file is reported and the old propagator kept.
        let tle = ephemeris_path(&base).unwrap().to_path_buf();
        std::fs::write(&tle, "not a tle at all, and longer than before").unwrap();
        assert!(r.check(&provider, next).is_err());
        let snap = status.snapshot(next);
        assert_eq!(snap.reload_errors, 1);
        assert!(snap.last_reload_error.is_some());
        assert!(
            provider.tracking_state(next).is_ok(),
            "previous data still works"
        );
        // Reported once, not every interval.
        assert!(!r.check(&provider, next).unwrap());
        // Fixed file: reloaded, error cleared.
        std::fs::write(&tle, crate::config::DEFAULT_ISS_TLE).unwrap();
        assert!(r.check(&provider, next).unwrap());
        let snap = status.snapshot(next);
        assert_eq!(snap.reloads, 2);
        assert!(snap.last_reload_error.is_none());
        let findings = assess_data(&snap, next);
        assert!(
            findings.iter().any(|f| f.code == "CG-302"),
            "stale 2020 TLE is reported"
        );
    }

    #[test]
    fn eop_outside_the_file_uses_the_nearest_end_and_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let base = station_with_files(dir.path());
        let status = Arc::new(DataStatus::default());
        let later = Utc.with_ymd_and_hms(2026, 11, 30, 0, 0, 0).unwrap();
        let (_r, eff) = Refresher::new(&base, Arc::clone(&status), later).unwrap();
        assert!((eff.tracking.ut1_minus_utc_s - -0.042).abs() < 1e-12);
        let f = assess_data(&status.snapshot(later), later);
        assert!(f.iter().any(|d| d.code == "CG-517"));
    }

    #[test]
    fn unreadable_eop_file_fails_startup() {
        let mut s = StationConfig::default();
        s.freshness.eop_file = Some(PathBuf::from("does/not/exist"));
        let e = Refresher::new(&s, Arc::default(), Utc::now()).unwrap_err();
        assert!(e.to_string().contains("does/not/exist"));
    }

    #[test]
    fn store_helpers_validate_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("finals.daily");
        std::fs::write(&p, sample()).unwrap();
        assert!(store_eop("<html>error</html>", &p).is_err());
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            sample(),
            "good file kept"
        );
        assert!(store_eop(&sample(), &p).is_ok());

        let bulletin = format!(
            "{}\nOTHER SAT\n1 49044U 21066A   20194.50000000  .00000000  00000-0  00000-0 0  9990\n2 49044  51.6400 200.0000 0002000  90.0000 270.0000 15.48000000    10\n",
            crate::config::DEFAULT_ISS_TLE
        );
        let t = dir.path().join("iss.tle");
        store_tle(&bulletin, Some("ISS (ZARYA)"), &t).unwrap();
        let written = std::fs::read_to_string(&t).unwrap();
        assert!(written.starts_with("ISS (ZARYA)\n1 25544U"), "{written}");
        assert!(store_tle("garbage", None, &t).is_err());
        assert!(needs_fetch(
            &dir.path().join("missing"),
            std::time::Duration::from_secs(1)
        ));
        assert!(!needs_fetch(&t, std::time::Duration::from_secs(3600)));
    }

    #[test]
    fn configured_tle_name_prefers_the_setting() {
        let mut s = StationConfig::default();
        assert_eq!(configured_tle_name(&s).as_deref(), Some("ISS (ZARYA)"));
        s.freshness.tle_name = Some("NAUKA".into());
        assert_eq!(configured_tle_name(&s).as_deref(), Some("NAUKA"));
    }

    /// A one-request HTTP server on loopback (the `refresh` client allows plain http there).
    #[cfg(feature = "refresh")]
    fn serve_once(status: &'static str, body: String) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let _ = write!(
                    s,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        format!("http://{addr}/data")
    }

    #[cfg(feature = "refresh")]
    #[test]
    fn fetch_writes_the_selected_tle_and_counts_failures() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = StationConfig::default();
        let tle = dir.path().join("iss.tle");
        s.tracking.ephemeris = EphemerisSource::TleFile(tle.clone());
        s.freshness.tle_name = Some("ISS (ZARYA)".into());
        let bulletin = format!(
            "OTHER SAT\n1 49044U 21066A   20194.50000000  .00000000  00000-0  00000-0 0  9990\n2 49044  51.6400 200.0000 0002000  90.0000 270.0000 15.48000000    10\n{}\n",
            crate::config::DEFAULT_ISS_TLE
        );
        s.freshness.tle_url = Some(serve_once("200 OK", bulletin));
        let status = DataStatus::default();
        fetch_due(&s, &status, false);
        let written = std::fs::read_to_string(&tle).unwrap();
        assert!(written.starts_with("ISS (ZARYA)\n1 25544U"), "{written}");
        assert_eq!(status.snapshot(Utc::now()).fetches, 1);

        // Fresh file: not fetched again before the interval...
        s.freshness.tle_url = Some("http://127.0.0.1:9/unused".into());
        fetch_due(&s, &status, false);
        assert_eq!(status.snapshot(Utc::now()).fetch_errors, 0);
        // ...and a server error never replaces the file.
        s.freshness.tle_url = Some(serve_once("404 Not Found", "nope".into()));
        fetch_due(&s, &status, true);
        let snap = status.snapshot(Utc::now());
        assert_eq!(snap.fetch_errors, 1);
        assert!(snap.last_fetch_error.unwrap().contains("404"));
        assert_eq!(std::fs::read_to_string(&tle).unwrap(), written);
        assert!(!assess_data(&status.snapshot(Utc::now()), Utc::now()).is_empty());
    }

    #[cfg(feature = "refresh")]
    #[test]
    fn fetched_eop_is_validated_before_it_replaces_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let eop = dir.path().join("finals.daily");
        let mut s = StationConfig::default();
        s.freshness.eop_file = Some(eop.clone());
        s.freshness.eop_url = Some(serve_once("200 OK", sample()));
        let status = DataStatus::default();
        fetch_due(&s, &status, false);
        assert_eq!(std::fs::read_to_string(&eop).unwrap(), sample());
        s.freshness.eop_url = Some(serve_once("200 OK", "<html>maintenance</html>".into()));
        fetch_due(&s, &status, true);
        assert_eq!(status.snapshot(Utc::now()).fetch_errors, 1);
        assert_eq!(std::fs::read_to_string(&eop).unwrap(), sample());
    }

    /// The real IERS data-centre CSV (`demo/fixtures/iers/finals2000A.daily.csv`, downloaded by the
    /// owner on 2026-11-07). Spot values below are copied by hand from that file's rows.
    fn real_csv() -> String {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../demo/fixtures/iers/finals2000A.daily.csv"
        ))
        .expect("IERS CSV fixture")
    }

    #[test]
    fn real_iers_csv_parses_with_the_published_values() {
        let t = parse_finals(&real_csv()).unwrap();
        assert_eq!(t.len(), 181);
        assert_eq!(t.span(), Some((d(2026, 8, 10), d(2027, 2, 6))));
        assert_eq!(t.observed_until(), Some(d(2026, 11, 7)), "last `final` row");
        // Row 61262 (2026-08-10): x 0.223254, y 0.358654, UT1-UTC 0.0114721, final.
        let v = t
            .at(Utc.with_ymd_and_hms(2026, 8, 10, 0, 0, 0).unwrap())
            .unwrap();
        assert_eq!(
            (v.x_arcsec, v.y_arcsec, v.ut1_minus_utc_s),
            (0.223254, 0.358654, 0.0114721)
        );
        assert!(!v.predicted);
        // Midday between 61351 (final, -0.0622121) and 61352 (prediction, -0.0632884).
        let v = t
            .at(Utc.with_ymd_and_hms(2026, 11, 7, 12, 0, 0).unwrap())
            .unwrap();
        assert!((v.ut1_minus_utc_s - (-0.0622121 + -0.0632884) / 2.0).abs() < 1e-12);
        assert!(
            v.predicted,
            "uses a prediction for the second half of the day"
        );
    }

    #[test]
    fn real_csv_and_fixed_width_forms_agree() {
        // Re-express the real CSV rows in the fixed-width layout and parse both.
        let csv = parse_finals(&real_csv()).unwrap();
        let fixed: Vec<String> = csv
            .rows
            .iter()
            .map(|r| {
                finals_line(
                    date_of_mjd(r.mjd).unwrap(),
                    r.x_arcsec,
                    r.y_arcsec,
                    r.ut1_minus_utc_s,
                    r.predicted,
                )
            })
            .collect();
        let fw = parse_finals(&fixed.join("\n")).unwrap();
        assert_eq!(fw.rows, csv.rows);
    }

    #[test]
    fn csv_header_and_rows_are_checked() {
        let header = "MJD;Year;Month;Day;Type;x_pole;sigma_x_pole;y_pole;sigma_y_pole;Type;UT1-UTC";
        let ok = format!(
            "{header}\n61262;2026;08;10;final;0.2;0;0.3;0;final;0.01\n61263;2026;08;11;prediction;;;;;prediction;\n"
        );
        let t = parse_finals(&format!("\u{feff}{ok}")).unwrap();
        assert_eq!(t.len(), 1, "blank prediction row skipped, BOM tolerated");
        let mismatch = format!("{header}\n61263;2026;08;10;final;0.2;0;0.3;0;final;0.01\n");
        assert!(
            parse_finals(&mismatch)
                .unwrap_err()
                .to_string()
                .contains("does not match")
        );
        let no_dut1 = "MJD;Year;Month;Day;x_pole;y_pole\n61262;2026;08;10;0.2;0.3\n";
        assert!(
            parse_finals(no_dut1)
                .unwrap_err()
                .to_string()
                .contains("UT1-UTC")
        );
        let no_flag = "MJD;Year;Month;Day;x_pole;y_pole;UT1-UTC\n";
        assert!(
            parse_finals(no_flag)
                .unwrap_err()
                .to_string()
                .contains("Type")
        );
        let text = format!("{header}\n61262;2026;08;10;final;abc;0;0.3;0;final;0.01\n");
        assert!(
            parse_finals(&text)
                .unwrap_err()
                .to_string()
                .contains("x_pole")
        );
        assert!(matches!(parse_finals(header), Err(EopError::Empty)));
    }
}
