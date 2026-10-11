//! CCSDS **Orbit Ephemeris Message** (OEM) backend: tracking from an operator-supplied ephemeris.
//!
//! A TLE is a compressed, mean-element *prediction* good to roughly a kilometre. Operators who
//! fly a spacecraft usually have something much better — a precise ephemeris from orbit
//! determination or on-board GPS — and exchange it as a CCSDS OEM (CCSDS 502.0-B-3): a list of
//! timestamped position/velocity states in a named reference frame. This module reads the
//! **KVN** (keyword = value) form, interpolates it, rotates it to the Earth-fixed frame with
//! Ephemerust's IAU 2006/2000B frame chain and the station's Earth orientation parameters, and
//! serves it through the same [`OrbitalPropagator`] seam as the TLE backend (`Methodology.md`
//! D-004, D-045).
//!
//! ## Supported content
//!
//! | Keyword | Accepted values |
//! |---------|-----------------|
//! | `CCSDS_OEM_VERS` | `1.0`, `2.0`, `3.0` |
//! | `CENTER_NAME` | `EARTH` |
//! | `REF_FRAME` | `GCRF`, `ICRF` (Earth-centred, treated as GCRF), `EME2000` / `J2000`, `TEME`, `ITRF` and dated realizations (`ITRF2014`, `ITRF2020`, `ITRF-93`, …) |
//! | `TIME_SYSTEM` | `UTC`, `TAI`, `TT`, `GPS` |
//! | `INTERPOLATION` | `LAGRANGE`, `HERMITE`, `LINEAR` (all evaluated as Lagrange of the given degree) |
//!
//! Data lines carry `epoch x y z vx vy vz` in km and km/s, optionally followed by three
//! accelerations (ignored). Multiple segments are allowed; `COVARIANCE_START … COVARIANCE_STOP`
//! blocks are skipped. Requests outside a segment's `USEABLE_START_TIME`/`USEABLE_STOP_TIME`
//! (or its first/last state) are refused rather than extrapolated.
//!
//! Interpolation uses the `INTERPOLATION_DEGREE + 1` states nearest the request time (default
//! degree 7). Position and velocity are interpolated independently; for a LEO ephemeris at
//! 60 s spacing that is millimetre-level.

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use ephemerust::frames::{InertialFrame, inertial_to_itrf, tai_minus_utc_s};

use crate::config::{ConfigError, StationConfig};
use crate::propagator::{
    OrbitalPropagator, StationModel, TrackingState, itrf_to_teme_position,
    nadir_sun_illumination_cos_teme,
};

/// Reference frame of an OEM segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OemFrame {
    /// An inertial frame rotated to ITRF with Earth orientation.
    Inertial(InertialFrame),
    /// Already Earth-fixed (any ITRF realization).
    Itrf,
}

/// Time scale of OEM epochs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OemTimeSystem {
    /// Coordinated Universal Time.
    Utc,
    /// International Atomic Time.
    Tai,
    /// Terrestrial Time (TAI + 32.184 s).
    Tt,
    /// GPS time (TAI − 19 s).
    Gps,
}

/// Errors from reading or using an OEM. Every variant can explain itself (see
/// [`crate::diagnose`], codes `CG-32x`).
#[derive(Debug, thiserror::Error)]
pub enum OemError {
    /// The text does not start with a `CCSDS_OEM_VERS` line.
    #[error("not a CCSDS OEM in KVN form: the first keyword must be CCSDS_OEM_VERS")]
    NotOem,
    /// A KVN line could not be understood.
    #[error("line {line}: {reason}")]
    Syntax {
        /// 1-based line number.
        line: usize,
        /// What was wrong.
        reason: String,
    },
    /// A required metadata keyword is missing from a segment.
    #[error("segment {segment}: required keyword {keyword} is missing")]
    MissingKeyword {
        /// 1-based segment number.
        segment: usize,
        /// The keyword.
        keyword: &'static str,
    },
    /// The ephemeris is not Earth-centred.
    #[error("segment {segment}: CENTER_NAME = {center} is not supported (only EARTH)")]
    UnsupportedCenter {
        /// 1-based segment number.
        segment: usize,
        /// The value found.
        center: String,
    },
    /// The reference frame is not one this backend can rotate to ITRF.
    #[error("segment {segment}: REF_FRAME = {frame} is not supported")]
    UnsupportedFrame {
        /// 1-based segment number.
        segment: usize,
        /// The value found.
        frame: String,
    },
    /// The time system is not supported.
    #[error("segment {segment}: TIME_SYSTEM = {system} is not supported")]
    UnsupportedTimeSystem {
        /// 1-based segment number.
        segment: usize,
        /// The value found.
        system: String,
    },
    /// A segment has fewer than two states or epochs that do not increase.
    #[error("segment {segment}: {reason}")]
    BadSegment {
        /// 1-based segment number.
        segment: usize,
        /// What was wrong.
        reason: String,
    },
    /// The file contains no data segments.
    #[error("the OEM contains no ephemeris data")]
    Empty,
    /// The requested time is outside every segment.
    #[error("{time} is outside the ephemeris coverage ({start} to {stop})")]
    OutOfCoverage {
        /// Requested time (UTC).
        time: DateTime<Utc>,
        /// Earliest covered time (UTC).
        start: DateTime<Utc>,
        /// Latest covered time (UTC).
        stop: DateTime<Utc>,
    },
}

/// One OEM segment: metadata plus its states (epochs converted to UTC).
#[derive(Debug, Clone)]
pub struct OemSegment {
    /// `OBJECT_NAME`.
    pub object_name: String,
    /// `OBJECT_ID`.
    pub object_id: String,
    /// Reference frame.
    pub frame: OemFrame,
    /// Time system the file used (epochs here are already UTC).
    pub time_system: OemTimeSystem,
    /// Interpolation degree used (`INTERPOLATION_DEGREE`, default 7).
    pub degree: usize,
    /// Usable coverage start (UTC).
    pub start: DateTime<Utc>,
    /// Usable coverage stop (UTC).
    pub stop: DateTime<Utc>,
    epochs: Vec<DateTime<Utc>>,
    states: Vec<[f64; 6]>,
}

impl OemSegment {
    /// Number of states in the segment.
    #[must_use]
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// `true` if the segment has no states (never true for a parsed segment).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Interpolated state (km, km/s) in the segment's own frame.
    fn interpolate(&self, time: DateTime<Utc>) -> ([f64; 3], [f64; 3]) {
        let n = self.states.len();
        let k = (self.degree + 1).min(n);
        // Index of the first state after `time`, then centre a k-point window on it.
        let after = self.epochs.partition_point(|e| *e <= time);
        let first = after.saturating_sub(k / 2).min(n - k);
        let t0 = self.epochs[first];
        let x = |i: usize| secs(self.epochs[i] - t0);
        let tq = secs(time - t0);
        let mut out = [0.0; 6];
        for j in first..first + k {
            let mut w = 1.0;
            for m in first..first + k {
                if m != j {
                    w *= (tq - x(m)) / (x(j) - x(m));
                }
            }
            for (o, s) in out.iter_mut().zip(self.states[j]) {
                *o += w * s;
            }
        }
        ([out[0], out[1], out[2]], [out[3], out[4], out[5]])
    }
}

fn secs(d: Duration) -> f64 {
    d.num_nanoseconds()
        .map_or_else(|| d.num_milliseconds() as f64 / 1000.0, |n| n as f64 * 1e-9)
}

/// A parsed CCSDS OEM.
///
/// ```
/// use chronus_gateway::oem::OemEphemeris;
///
/// let text = "CCSDS_OEM_VERS = 2.0
/// CREATION_DATE = 2026-10-11T00:00:00
/// ORIGINATOR = EXAMPLE
/// META_START
/// OBJECT_NAME = DEMOSAT
/// OBJECT_ID = 2026-999A
/// CENTER_NAME = EARTH
/// REF_FRAME = ITRF2014
/// TIME_SYSTEM = UTC
/// START_TIME = 2026-10-11T00:00:00
/// STOP_TIME = 2026-10-11T00:02:00
/// META_STOP
/// 2026-10-11T00:00:00 7000.0 0.0 0.0 0.0 7.5 0.0
/// 2026-10-11T00:01:00 6996.0 450.0 0.0 -0.13 7.49 0.0
/// 2026-10-11T00:02:00 6984.0 899.0 0.0 -0.26 7.47 0.0
/// ";
/// let eph = OemEphemeris::parse(text)?;
/// assert_eq!(eph.segments().len(), 1);
/// assert_eq!(eph.segments()[0].object_name, "DEMOSAT");
/// # Ok::<(), chronus_gateway::oem::OemError>(())
/// ```
#[derive(Debug, Clone)]
pub struct OemEphemeris {
    segments: Vec<OemSegment>,
}

impl OemEphemeris {
    /// Reads and parses an OEM file.
    ///
    /// # Errors
    ///
    /// [`ConfigError::OemRead`] for I/O problems (wrapped in `anyhow`), otherwise [`OemError`].
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::OemRead {
            path: path.display().to_string(),
            source,
        })?;
        Ok(Self::parse(&text)?)
    }

    /// Parses OEM text (KVN).
    ///
    /// # Errors
    ///
    /// [`OemError`] describing the first problem, with a line number where applicable.
    pub fn parse(text: &str) -> Result<Self, OemError> {
        let mut lines = text
            .lines()
            .enumerate()
            .map(|(i, l)| (i + 1, l.trim()))
            .filter(|(_, l)| !l.is_empty() && !l.starts_with("COMMENT"));

        match lines.next() {
            Some((_, l)) if kvn(l).is_some_and(|(k, _)| k == "CCSDS_OEM_VERS") => {}
            _ => return Err(OemError::NotOem),
        }

        let mut segments = Vec::new();
        let mut meta: Option<Vec<(String, String)>> = None;
        let mut current: Option<SegmentBuilder> = None;
        let mut in_covariance = false;

        for (ln, line) in lines {
            if in_covariance {
                in_covariance = line != "COVARIANCE_STOP";
                continue;
            }
            match line {
                "META_START" => {
                    if let Some(b) = current.take() {
                        segments.push(b.finish(segments.len() + 1)?);
                    }
                    meta = Some(Vec::new());
                }
                "META_STOP" => {
                    let m = meta.take().ok_or_else(|| OemError::Syntax {
                        line: ln,
                        reason: "META_STOP without META_START".into(),
                    })?;
                    current = Some(SegmentBuilder::from_meta(&m, segments.len() + 1)?);
                }
                "COVARIANCE_START" => in_covariance = true,
                _ => {
                    if let Some(m) = meta.as_mut() {
                        let (k, v) = kvn(line).ok_or_else(|| OemError::Syntax {
                            line: ln,
                            reason: format!("expected KEYWORD = value in metadata, found `{line}`"),
                        })?;
                        m.push((k.to_string(), v.to_string()));
                    } else if let Some(b) = current.as_mut() {
                        b.push_data(ln, line)?;
                    } else if kvn(line).is_none() {
                        return Err(OemError::Syntax {
                            line: ln,
                            reason: "data line before the first META_START/META_STOP block".into(),
                        });
                    }
                    // Other header keywords (CREATION_DATE, ORIGINATOR, MESSAGE_ID) are informational.
                }
            }
        }
        if meta.is_some() {
            return Err(OemError::Syntax {
                line: text.lines().count(),
                reason: "META_START without META_STOP".into(),
            });
        }
        if let Some(b) = current.take() {
            segments.push(b.finish(segments.len() + 1)?);
        }
        if segments.is_empty() {
            return Err(OemError::Empty);
        }
        Ok(Self { segments })
    }

    /// The parsed segments, in file order.
    #[must_use]
    pub fn segments(&self) -> &[OemSegment] {
        &self.segments
    }

    /// Overall coverage `(start, stop)` in UTC.
    #[must_use]
    pub fn coverage(&self) -> (DateTime<Utc>, DateTime<Utc>) {
        let start = self
            .segments
            .iter()
            .map(|s| s.start)
            .min()
            .expect("non-empty");
        let stop = self
            .segments
            .iter()
            .map(|s| s.stop)
            .max()
            .expect("non-empty");
        (start, stop)
    }

    /// Interpolated Earth-fixed (ITRF) state (km, km/s) at `time`.
    ///
    /// # Errors
    ///
    /// [`OemError::OutOfCoverage`] if no segment covers `time`.
    pub fn state_itrf(
        &self,
        time: DateTime<Utc>,
        eop: &ephemerust::EarthOrientation,
    ) -> Result<([f64; 3], [f64; 3]), OemError> {
        // Later segments win where segments overlap (CCSDS: newer data follows older).
        let seg = self
            .segments
            .iter()
            .rev()
            .find(|s| s.start <= time && time <= s.stop)
            .ok_or_else(|| {
                let (start, stop) = self.coverage();
                OemError::OutOfCoverage { time, start, stop }
            })?;
        let (r, v) = seg.interpolate(time);
        Ok(match seg.frame {
            OemFrame::Itrf => (r, v),
            OemFrame::Inertial(f) => inertial_to_itrf(f, time, r, v, eop),
        })
    }
}

/// Splits `KEY = value`.
fn kvn(line: &str) -> Option<(&str, &str)> {
    let (k, v) = line.split_once('=')?;
    let k = k.trim();
    (!k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
    .then_some((k, v.trim()))
}

struct SegmentBuilder {
    object_name: String,
    object_id: String,
    frame: OemFrame,
    time_system: OemTimeSystem,
    degree: usize,
    start: DateTime<Utc>,
    stop: DateTime<Utc>,
    useable_start: Option<DateTime<Utc>>,
    useable_stop: Option<DateTime<Utc>>,
    epochs: Vec<DateTime<Utc>>,
    states: Vec<[f64; 6]>,
}

impl SegmentBuilder {
    fn from_meta(meta: &[(String, String)], segment: usize) -> Result<Self, OemError> {
        let get = |k: &'static str| -> Result<&str, OemError> {
            meta.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
                .ok_or(OemError::MissingKeyword {
                    segment,
                    keyword: k,
                })
        };
        let opt = |k: &str| {
            meta.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };

        let center = get("CENTER_NAME")?;
        if !center.eq_ignore_ascii_case("EARTH") {
            return Err(OemError::UnsupportedCenter {
                segment,
                center: center.to_string(),
            });
        }
        let frame_name = get("REF_FRAME")?.to_ascii_uppercase();
        let frame = match frame_name.as_str() {
            "GCRF" | "ICRF" => OemFrame::Inertial(InertialFrame::Gcrf),
            "EME2000" | "J2000" => OemFrame::Inertial(InertialFrame::Eme2000),
            "TEME" => OemFrame::Inertial(InertialFrame::Teme),
            f if f.starts_with("ITRF") => OemFrame::Itrf,
            _ => {
                return Err(OemError::UnsupportedFrame {
                    segment,
                    frame: frame_name,
                });
            }
        };
        let ts_name = get("TIME_SYSTEM")?.to_ascii_uppercase();
        let time_system = match ts_name.as_str() {
            "UTC" => OemTimeSystem::Utc,
            "TAI" => OemTimeSystem::Tai,
            "TT" => OemTimeSystem::Tt,
            "GPS" => OemTimeSystem::Gps,
            _ => {
                return Err(OemError::UnsupportedTimeSystem {
                    segment,
                    system: ts_name,
                });
            }
        };
        let time = |k: &'static str, v: &str| {
            parse_epoch(v, time_system).ok_or_else(|| OemError::BadSegment {
                segment,
                reason: format!(
                    "{k} = {v} is not a CCSDS date (YYYY-MM-DDThh:mm:ss or YYYY-DDDThh:mm:ss)"
                ),
            })
        };
        let start = time("START_TIME", get("START_TIME")?)?;
        let stop = time("STOP_TIME", get("STOP_TIME")?)?;
        let useable_start = opt("USEABLE_START_TIME")
            .map(|v| time("USEABLE_START_TIME", v))
            .transpose()?;
        let useable_stop = opt("USEABLE_STOP_TIME")
            .map(|v| time("USEABLE_STOP_TIME", v))
            .transpose()?;
        let degree = match opt("INTERPOLATION_DEGREE") {
            Some(v) => v
                .parse::<usize>()
                .ok()
                .filter(|d| (1..=15).contains(d))
                .ok_or_else(|| OemError::BadSegment {
                    segment,
                    reason: format!("INTERPOLATION_DEGREE = {v} (expected an integer 1..=15)"),
                })?,
            None if opt("INTERPOLATION").is_some_and(|i| i.eq_ignore_ascii_case("LINEAR")) => 1,
            None => 7,
        };
        Ok(Self {
            object_name: opt("OBJECT_NAME").unwrap_or("UNKNOWN").to_string(),
            object_id: opt("OBJECT_ID").unwrap_or("UNKNOWN").to_string(),
            frame,
            time_system,
            degree,
            start,
            stop,
            useable_start,
            useable_stop,
            epochs: Vec::new(),
            states: Vec::new(),
        })
    }

    fn push_data(&mut self, line_no: usize, line: &str) -> Result<(), OemError> {
        let mut tok = line.split_whitespace();
        let syntax = |reason: String| OemError::Syntax {
            line: line_no,
            reason,
        };
        let epoch_text = tok.next().ok_or_else(|| syntax("empty data line".into()))?;
        let epoch = parse_epoch(epoch_text, self.time_system)
            .ok_or_else(|| syntax(format!("`{epoch_text}` is not a CCSDS epoch")))?;
        let nums: Vec<f64> = tok
            .map(|t| {
                t.parse::<f64>()
                    .ok()
                    .filter(|x| x.is_finite())
                    .ok_or_else(|| syntax(format!("`{t}` is not a number")))
            })
            .collect::<Result<_, _>>()?;
        if nums.len() != 6 && nums.len() != 9 {
            return Err(syntax(format!(
                "expected 6 values (x y z vx vy vz) or 9 (with accelerations) after the epoch, found {}",
                nums.len()
            )));
        }
        if let Some(prev) = self.epochs.last()
            && epoch <= *prev
        {
            return Err(syntax(format!("epoch {epoch_text} does not increase")));
        }
        self.epochs.push(epoch);
        self.states
            .push([nums[0], nums[1], nums[2], nums[3], nums[4], nums[5]]);
        Ok(())
    }

    fn finish(self, segment: usize) -> Result<OemSegment, OemError> {
        if self.states.len() < 2 {
            return Err(OemError::BadSegment {
                segment,
                reason: format!("needs at least 2 states, found {}", self.states.len()),
            });
        }
        let first = self.epochs[0];
        let last = *self.epochs.last().expect("len >= 2");
        // Usable window: the declared useable (or start/stop) times, clipped to the data.
        let start = self.useable_start.unwrap_or(self.start).max(first);
        let stop = self.useable_stop.unwrap_or(self.stop).min(last);
        if start >= stop {
            return Err(OemError::BadSegment {
                segment,
                reason: "the usable time window is empty".into(),
            });
        }
        Ok(OemSegment {
            object_name: self.object_name,
            object_id: self.object_id,
            frame: self.frame,
            time_system: self.time_system,
            degree: self.degree,
            start,
            stop,
            epochs: self.epochs,
            states: self.states,
        })
    }
}

/// Parses a CCSDS ASCII epoch (`YYYY-MM-DDThh:mm:ss[.fff][Z]` or `YYYY-DDDThh:mm:ss[.fff]`) in
/// the given time system and converts it to UTC.
fn parse_epoch(text: &str, system: OemTimeSystem) -> Option<DateTime<Utc>> {
    let text = text.trim_end_matches('Z');
    let (date, time) = text.split_once('T')?;
    let date = if date.len() == 8 {
        // Day-of-year form YYYY-DDD.
        let (y, doy) = date.split_once('-')?;
        NaiveDate::from_yo_opt(y.parse().ok()?, doy.parse().ok()?)?
    } else {
        NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?
    };
    let time = NaiveTime::parse_from_str(time, "%H:%M:%S%.f").ok()?;
    let naive = NaiveDateTime::new(date, time).and_utc();
    let offset = |s: f64| Duration::nanoseconds((s * 1e9).round() as i64);
    Some(match system {
        OemTimeSystem::Utc => naive,
        OemTimeSystem::Tai | OemTimeSystem::Tt | OemTimeSystem::Gps => {
            let tai = match system {
                OemTimeSystem::Tt => naive - offset(32.184),
                OemTimeSystem::Gps => naive + offset(19.0),
                _ => naive,
            };
            // TAI → UTC: subtract TAI−UTC, evaluated at the (approximate) UTC instant.
            let guess = tai - offset(tai_minus_utc_s(tai));
            tai - offset(tai_minus_utc_s(guess))
        }
    })
}

/// Tracking backend driven by a CCSDS OEM (see the module docs).
pub struct OemPropagator {
    ephemeris: OemEphemeris,
    station: StationModel,
}

impl OemPropagator {
    /// Wraps a parsed ephemeris and a station model.
    #[must_use]
    pub fn new(ephemeris: OemEphemeris, station: StationModel) -> Self {
        Self { ephemeris, station }
    }

    /// Builds the backend from a [`StationConfig`] whose `oem_file` is set.
    ///
    /// # Errors
    ///
    /// Configuration, I/O or [`OemError`] parse failures.
    pub fn from_station(config: &StationConfig) -> Result<Self> {
        let station = StationModel::from_station(config)?;
        let path = config
            .oem_file
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("station.oem_file is not set"))?;
        Ok(Self::new(OemEphemeris::from_file(path)?, station))
    }

    /// The loaded ephemeris.
    #[must_use]
    pub fn ephemeris(&self) -> &OemEphemeris {
        &self.ephemeris
    }
}

impl OrbitalPropagator for OemPropagator {
    fn tracking_state(&self, time: DateTime<Utc>) -> Result<TrackingState> {
        let eop = &self.station.earth_orientation;
        let (r, v) = self.ephemeris.state_itrf(time, eop)?;
        let la = ephemerust::topocentric_look_angles(r, v, self.station.observer())?;
        let illum = nadir_sun_illumination_cos_teme(itrf_to_teme_position(r, time, eop), time)
            .filter(|x| x.is_finite())
            .unwrap_or(f64::NAN);
        Ok(self
            .station
            .tracking_state(&la)
            .with_sun_illumination(illum))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn minimal(frame: &str, ts: &str, extra_meta: &str, data: &str) -> String {
        format!(
            "CCSDS_OEM_VERS = 2.0\nCREATION_DATE = 2026-10-11T00:00:00\nORIGINATOR = TEST\n\
             META_START\nOBJECT_NAME = T\nOBJECT_ID = 2026-001A\nCENTER_NAME = EARTH\n\
             REF_FRAME = {frame}\nTIME_SYSTEM = {ts}\nSTART_TIME = 2026-10-11T00:00:00\n\
             STOP_TIME = 2026-10-11T00:03:00\n{extra_meta}META_STOP\n{data}"
        )
    }

    const LINEAR_DATA: &str = "2026-10-11T00:00:00 0 0 7000 1 2 3\n\
                               2026-10-11T00:01:00 60 120 7000 1 2 3\n\
                               2026-10-11T00:02:00 120 240 7000 1 2 3\n\
                               2026-10-11T00:03:00 180 360 7000 1 2 3\n";

    #[test]
    fn interpolation_reproduces_a_straight_line_exactly() {
        let eph = OemEphemeris::parse(&minimal("ITRF2020", "UTC", "", LINEAR_DATA)).unwrap();
        let t = Utc.with_ymd_and_hms(2026, 10, 11, 0, 1, 30).unwrap();
        let (r, v) = eph.state_itrf(t, &Default::default()).unwrap();
        assert!(
            (r[0] - 90.0).abs() < 1e-9 && (r[1] - 180.0).abs() < 1e-9,
            "{r:?}"
        );
        assert!((v[2] - 3.0).abs() < 1e-12);
    }

    #[test]
    fn refuses_to_extrapolate() {
        let eph = OemEphemeris::parse(&minimal("ITRF", "UTC", "", LINEAR_DATA)).unwrap();
        let t = Utc.with_ymd_and_hms(2026, 10, 11, 0, 4, 0).unwrap();
        let err = eph.state_itrf(t, &Default::default()).unwrap_err();
        assert!(matches!(err, OemError::OutOfCoverage { .. }), "{err}");
    }

    #[test]
    fn useable_window_restricts_coverage() {
        let meta =
            "USEABLE_START_TIME = 2026-10-11T00:01:00\nUSEABLE_STOP_TIME = 2026-10-11T00:02:00\n";
        let eph = OemEphemeris::parse(&minimal("ITRF", "UTC", meta, LINEAR_DATA)).unwrap();
        let (s, e) = eph.coverage();
        assert_eq!(
            (s.format("%M").to_string(), e.format("%M").to_string()),
            ("01".into(), "02".into())
        );
    }

    #[test]
    fn time_systems_convert_to_utc() {
        let t = |s| parse_epoch("2026-10-11T00:00:00", s).unwrap();
        let utc = t(OemTimeSystem::Utc);
        assert_eq!((utc - t(OemTimeSystem::Tai)).num_seconds(), 37);
        assert_eq!((utc - t(OemTimeSystem::Gps)).num_seconds(), 18);
        assert_eq!((utc - t(OemTimeSystem::Tt)).num_milliseconds(), 69_184);
        // Day-of-year form.
        assert_eq!(
            parse_epoch("2026-284T00:00:00", OemTimeSystem::Utc).unwrap(),
            utc
        );
    }

    #[test]
    fn errors_name_the_problem() {
        let err = |text: String| OemEphemeris::parse(&text).unwrap_err().to_string();
        assert!(err("hello".into()).contains("CCSDS_OEM_VERS"));
        assert!(err(minimal("TOD", "UTC", "", LINEAR_DATA)).contains("REF_FRAME = TOD"));
        assert!(err(minimal("GCRF", "UT1", "", LINEAR_DATA)).contains("TIME_SYSTEM = UT1"));
        assert!(
            err(minimal("GCRF", "UTC", "", "2026-10-11T00:00:00 1 2 3\n"))
                .contains("expected 6 values")
        );
        let backwards = "2026-10-11T00:01:00 0 0 7000 0 0 0\n2026-10-11T00:00:00 0 0 7000 0 0 0\n";
        assert!(err(minimal("GCRF", "UTC", "", backwards)).contains("does not increase"));
        assert!(
            err(minimal(
                "GCRF",
                "UTC",
                "",
                "2026-10-11T00:00:00 0 0 7000 0 0 0\n"
            ))
            .contains("at least 2")
        );
        let mars = minimal("GCRF", "UTC", "", LINEAR_DATA)
            .replace("CENTER_NAME = EARTH", "CENTER_NAME = MARS");
        assert!(err(mars).contains("CENTER_NAME = MARS"));
    }

    #[test]
    fn comments_covariance_and_accelerations_are_tolerated() {
        let data = format!(
            "COMMENT generated for a unit test\n{}COVARIANCE_START\nEPOCH = 2026-10-11T00:00:00\n\
             COV_REF_FRAME = RTN\n1.0\n0.0 1.0\nCOVARIANCE_STOP\n",
            LINEAR_DATA.replace(" 1 2 3\n", " 1 2 3 0.001 0.002 0.003\n")
        );
        let eph = OemEphemeris::parse(&minimal("EME2000", "UTC", "", &data)).unwrap();
        assert_eq!(eph.segments()[0].len(), 4);
        assert_eq!(
            eph.segments()[0].frame,
            OemFrame::Inertial(InertialFrame::Eme2000)
        );
    }
}
