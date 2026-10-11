//! Spacecraft time codes (Tranche 2, item 2.3).
//!
//! Decodes the CCSDS time field that leads a packet's **secondary header** (CCSDS 301.0-B-4) so
//! physics can be evaluated at the instant the *spacecraft* says the data was produced, not only at
//! the instant the datagram reached the gateway.
//!
//! Two fixed-length, **implicit-P-field** (agency-defined, not transmitted) formats are supported,
//! both 6 bytes and both counting from the CCSDS epoch **1958-01-01**:
//!
//! | Format | Layout | Time scale |
//! |--------|--------|------------|
//! | [`TimeFormat::Cds`] | `u16` days, `u32` milliseconds of day (big-endian) | UTC |
//! | [`TimeFormat::Cuc`] | `u32` coarse seconds, `u16` fine (1/65536 s) (big-endian) | TAI |
//!
//! CUC counts TAI, so decoding subtracts a configured TAI−UTC offset
//! ([`crate::config::TimeConfig::cuc_tai_minus_utc_s`], 37 s since 2017). The offset is applied as
//! one constant — fine for present-day data, not for historical epochs.
//!
//! Everything here is pure and total: untrusted bytes yield a [`TimeError`], never a panic.

use chrono::{DateTime, Utc};

use crate::ccsds::TelemetryFrame;
use crate::config::TimeConfig;
use crate::validate::{FLAG_V2_TIME_CODE_INVALID, FLAG_V2_TIME_TAG_SKEW};

/// Length in bytes of every supported time field.
pub const TIME_FIELD_LEN: usize = 6;

/// Unix timestamp (seconds) of the CCSDS epoch, 1958-01-01T00:00:00.
pub const CCSDS_EPOCH_UNIX_S: i64 = -378_691_200;

const MS_PER_DAY: u64 = 86_400_000;

/// Which time code (if any) leads the packet data field of secondary-header packets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimeFormat {
    /// No spacecraft time is decoded; packet data is passed through untouched (default).
    #[default]
    None,
    /// CCSDS Day Segmented time: 16-bit days + 32-bit ms of day, UTC.
    Cds,
    /// CCSDS Unsegmented time: 32-bit seconds + 16-bit fraction, TAI.
    Cuc,
}

impl TimeFormat {
    /// Bytes this format occupies at the start of the packet data field (`0` for `None`).
    #[must_use]
    pub const fn field_len(self) -> usize {
        match self {
            TimeFormat::None => 0,
            TimeFormat::Cds | TimeFormat::Cuc => TIME_FIELD_LEN,
        }
    }
}

/// Why a time field could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TimeError {
    /// Fewer than [`TIME_FIELD_LEN`] bytes were available.
    #[error("time field needs {TIME_FIELD_LEN} byte(s) but the packet data field has {len}")]
    TooShort {
        /// Bytes actually present.
        len: usize,
    },
    /// The encoded value is not a valid instant (e.g. milliseconds-of-day beyond one day).
    #[error("time field value is out of range")]
    OutOfRange,
}

/// Decodes the leading time field of `data` according to `format`.
///
/// Returns `Ok(None)` for [`TimeFormat::None`]. `cuc_tai_minus_utc_s` only affects CUC.
///
/// ```
/// use chrono::{TimeZone, Utc};
/// use chronus_gateway::time::{TimeFormat, decode_time, encode_cds};
///
/// let t = Utc.with_ymd_and_hms(2026, 10, 11, 12, 30, 15).unwrap();
/// let bytes = encode_cds(t).unwrap();
/// assert_eq!(decode_time(TimeFormat::Cds, &bytes, 37).unwrap(), Some(t));
/// assert_eq!(decode_time(TimeFormat::None, &bytes, 37).unwrap(), None);
/// assert!(decode_time(TimeFormat::Cds, &bytes[..3], 37).is_err());
/// ```
pub fn decode_time(
    format: TimeFormat,
    data: &[u8],
    cuc_tai_minus_utc_s: i32,
) -> Result<Option<DateTime<Utc>>, TimeError> {
    if format == TimeFormat::None {
        return Ok(None);
    }
    let Some(field) = data.get(..TIME_FIELD_LEN) else {
        return Err(TimeError::TooShort { len: data.len() });
    };
    match format {
        TimeFormat::Cds => {
            let days = u64::from(u16::from_be_bytes([field[0], field[1]]));
            let ms = u64::from(u32::from_be_bytes([field[2], field[3], field[4], field[5]]));
            // One extra second tolerated for a UTC leap second (23:59:60).
            if ms >= MS_PER_DAY + 1000 {
                return Err(TimeError::OutOfRange);
            }
            let total_ms = CCSDS_EPOCH_UNIX_S
                .checked_mul(1000)
                .and_then(|e| e.checked_add((days * MS_PER_DAY + ms) as i64))
                .ok_or(TimeError::OutOfRange)?;
            DateTime::from_timestamp_millis(total_ms)
                .map(Some)
                .ok_or(TimeError::OutOfRange)
        }
        TimeFormat::Cuc => {
            let coarse = i64::from(u32::from_be_bytes([field[0], field[1], field[2], field[3]]));
            let fine = u64::from(u16::from_be_bytes([field[4], field[5]]));
            let nanos = ((fine * 1_000_000_000) >> 16) as u32;
            let secs = CCSDS_EPOCH_UNIX_S + coarse - i64::from(cuc_tai_minus_utc_s);
            DateTime::from_timestamp(secs, nanos)
                .map(Some)
                .ok_or(TimeError::OutOfRange)
        }
        // `TimeFormat::None` returned above; future variants default to "undecodable".
        _ => Err(TimeError::OutOfRange),
    }
}

/// Encodes `t` as a CDS time field (millisecond resolution; sub-millisecond is truncated).
///
/// Returns `None` if `t` falls outside the 16-bit day range (1958 … ~2137). Used by the HIL sim,
/// replay tool and tests to produce packets the gateway decodes.
#[must_use]
pub fn encode_cds(t: DateTime<Utc>) -> Option<[u8; TIME_FIELD_LEN]> {
    let ms_since_epoch = t
        .timestamp_millis()
        .checked_sub(CCSDS_EPOCH_UNIX_S.checked_mul(1000)?)?;
    if ms_since_epoch < 0 {
        return None;
    }
    let ms_since_epoch = ms_since_epoch as u64;
    let days = u16::try_from(ms_since_epoch / MS_PER_DAY).ok()?;
    let ms = (ms_since_epoch % MS_PER_DAY) as u32;
    let mut out = [0u8; TIME_FIELD_LEN];
    out[..2].copy_from_slice(&days.to_be_bytes());
    out[2..].copy_from_slice(&ms.to_be_bytes());
    Some(out)
}

/// Encodes `t` as a CUC time field (1/65536 s resolution), adding `tai_minus_utc_s` to convert
/// the UTC instant to TAI. Returns `None` outside the 32-bit seconds range.
#[must_use]
pub fn encode_cuc(t: DateTime<Utc>, tai_minus_utc_s: i32) -> Option<[u8; TIME_FIELD_LEN]> {
    let coarse = t.timestamp() - CCSDS_EPOCH_UNIX_S + i64::from(tai_minus_utc_s);
    let coarse = u32::try_from(coarse).ok()?;
    let fine = ((u64::from(t.timestamp_subsec_nanos()) << 16) / 1_000_000_000) as u16;
    let mut out = [0u8; TIME_FIELD_LEN];
    out[..4].copy_from_slice(&coarse.to_be_bytes());
    out[4..].copy_from_slice(&fine.to_be_bytes());
    Some(out)
}

/// Encodes `t` in `format`, or `None` for [`TimeFormat::None`] / unrepresentable instants.
#[must_use]
pub fn encode_time(
    format: TimeFormat,
    t: DateTime<Utc>,
    cuc_tai_minus_utc_s: i32,
) -> Option<[u8; TIME_FIELD_LEN]> {
    match format {
        TimeFormat::Cds => encode_cds(t),
        TimeFormat::Cuc => encode_cuc(t, cuc_tai_minus_utc_s),
        _ => None,
    }
}

/// Decides the instant physics is evaluated at for `frame`, and records time findings on it.
///
/// Sets [`TelemetryFrame::physics_flags_v2`], [`TelemetryFrame::onboard_time`] and the length of
/// the time field (so [`TelemetryFrame::user_data`] skips it). Returns:
///
/// - the **onboard time** when `cfg.format` is set, the packet has a secondary header with a
///   decodable time code, and (unless `cfg.max_skew_s == 0`, lab mode) it is within
///   `cfg.max_skew_s` of the receive time;
/// - otherwise the **receive time**, with [`FLAG_V2_TIME_TAG_SKEW`] or
///   [`FLAG_V2_TIME_CODE_INVALID`] set when the packet *claimed* a time that could not be trusted
///   or read. Packets with no secondary header are not flagged: they simply have no time.
pub fn resolve_frame_time(cfg: &TimeConfig, frame: &mut TelemetryFrame) -> DateTime<Utc> {
    frame.physics_flags_v2 = 0;
    frame.onboard_time = None;
    frame.time_field_len = 0;
    if cfg.format == TimeFormat::None || !frame.has_secondary_header {
        return frame.received_at;
    }
    match decode_time(cfg.format, frame.payload(), cfg.cuc_tai_minus_utc_s) {
        Ok(Some(t)) => {
            frame.onboard_time = Some(t);
            frame.time_field_len = cfg.format.field_len();
            let skew_s = (t - frame.received_at).num_milliseconds().unsigned_abs() as f64 / 1000.0;
            if cfg.max_skew_s > 0.0 && skew_s > cfg.max_skew_s {
                frame.physics_flags_v2 |= FLAG_V2_TIME_TAG_SKEW;
                frame.received_at
            } else {
                t
            }
        }
        Ok(None) => frame.received_at,
        Err(_) => {
            frame.physics_flags_v2 |= FLAG_V2_TIME_CODE_INVALID;
            frame.received_at
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).single().unwrap()
    }

    #[test]
    fn ccsds_epoch_constant_is_1958_01_01() {
        assert_eq!(utc(1958, 1, 1, 0, 0, 0).timestamp(), CCSDS_EPOCH_UNIX_S);
    }

    #[test]
    fn cds_epoch_encodes_to_zero_and_known_vector() {
        assert_eq!(encode_cds(utc(1958, 1, 1, 0, 0, 0)).unwrap(), [0; 6]);
        // 1970-01-01 is 4383 days after the epoch (= 0x111F).
        assert_eq!(
            encode_cds(utc(1970, 1, 1, 0, 0, 1)).unwrap(),
            [0x11, 0x1F, 0, 0, 0x03, 0xE8]
        );
    }

    #[test]
    fn cds_round_trips_to_the_millisecond() {
        let t = utc(2026, 10, 11, 23, 59, 59) + chrono::Duration::milliseconds(789);
        let b = encode_cds(t).unwrap();
        assert_eq!(decode_time(TimeFormat::Cds, &b, 37).unwrap(), Some(t));
    }

    #[test]
    fn cuc_round_trips_within_fine_resolution() {
        let t = utc(2026, 10, 11, 12, 0, 0) + chrono::Duration::microseconds(250_000);
        let b = encode_cuc(t, 37).unwrap();
        let back = decode_time(TimeFormat::Cuc, &b, 37).unwrap().unwrap();
        assert!(
            (back - t).num_microseconds().unwrap().abs() < 20,
            "{back} vs {t}"
        );
    }

    #[test]
    fn cuc_applies_the_tai_offset() {
        let t = utc(2026, 1, 1, 0, 0, 0);
        let b = encode_cuc(t, 37).unwrap();
        let wrong = decode_time(TimeFormat::Cuc, &b, 0).unwrap().unwrap();
        assert_eq!((wrong - t).num_seconds(), 37);
    }

    #[test]
    fn none_format_never_decodes() {
        assert_eq!(decode_time(TimeFormat::None, &[], 37).unwrap(), None);
        assert_eq!(TimeFormat::None.field_len(), 0);
        assert_eq!(TimeFormat::Cds.field_len(), TIME_FIELD_LEN);
    }

    #[test]
    fn short_field_is_a_typed_error() {
        assert_eq!(
            decode_time(TimeFormat::Cuc, &[1, 2, 3], 37),
            Err(TimeError::TooShort { len: 3 })
        );
    }

    #[test]
    fn cds_ms_of_day_out_of_range_is_rejected() {
        let mut b = [0u8; 6];
        b[2..].copy_from_slice(&(86_401_000u32).to_be_bytes());
        assert_eq!(
            decode_time(TimeFormat::Cds, &b, 37),
            Err(TimeError::OutOfRange)
        );
        // A leap second (23:59:60.xxx) is tolerated.
        b[2..].copy_from_slice(&(86_400_500u32).to_be_bytes());
        assert!(decode_time(TimeFormat::Cds, &b, 37).unwrap().is_some());
    }

    #[test]
    fn unrepresentable_instants_do_not_encode() {
        assert!(encode_cds(utc(1950, 1, 1, 0, 0, 0)).is_none());
        assert!(encode_cds(utc(2200, 1, 1, 0, 0, 0)).is_none());
        assert!(encode_cuc(utc(1950, 1, 1, 0, 0, 0), 37).is_none());
        assert!(encode_time(TimeFormat::None, utc(2026, 1, 1, 0, 0, 0), 37).is_none());
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        for a in [0u8, 1, 0x7F, 0xFF] {
            let b = [a; 6];
            let _ = decode_time(TimeFormat::Cds, &b, 37);
            let _ = decode_time(TimeFormat::Cuc, &b, 37);
        }
    }
}
