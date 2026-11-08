//! Building blocks of the `chronus-soak` load driver (Tranche 5.2, Methodology D-055).
//!
//! - [`probe_packet`] / [`ProbeFrame::decode`]: a CCSDS TM space packet whose data field carries
//!   the send time, so a WebSocket client can measure end-to-end latency (UDP in → parse →
//!   co-validate → JSON → WebSocket out) against the same host clock.
//! - [`LatencyHistogram`]: a fixed-size log-linear histogram (≤ 1.6 % bucket error, any value up
//!   to hours) that merges across clients without allocation per sample.
//! - [`RssTrend`]: memory samples over the run and the least-squares growth rate that decides
//!   "bounded RSS".
//! - [`parse_duration`]: `90s`, `10m`, `24h`, or plain seconds.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// Marker at the start of every probe packet's data field.
pub const PROBE_MAGIC: &[u8; 4] = b"SOAK";
/// Probe data field: magic, sequence (u64 BE), send time in Unix nanoseconds (u64 BE).
pub const PROBE_DATA_LEN: usize = 4 + 8 + 8;

/// Nanoseconds since the Unix epoch on this host.
#[must_use]
pub fn unix_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// A CCSDS TM space packet (no secondary header) carrying probe `seq`, sent at `sent_ns`.
#[must_use]
pub fn probe_packet(apid: u16, seq: u64, sent_ns: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(6 + PROBE_DATA_LEN);
    v.extend_from_slice(&(apid & 0x07FF).to_be_bytes());
    // Sequence flags 0b11 (unsegmented) + the low 14 bits of the probe sequence.
    v.extend_from_slice(&(0xC000 | (seq as u16 & 0x3FFF)).to_be_bytes());
    v.extend_from_slice(&((PROBE_DATA_LEN - 1) as u16).to_be_bytes());
    v.extend_from_slice(PROBE_MAGIC);
    v.extend_from_slice(&seq.to_be_bytes());
    v.extend_from_slice(&sent_ns.to_be_bytes());
    v
}

/// The probe fields recovered from a gateway WebSocket message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeFrame {
    /// Probe sequence number (full 64 bits, unlike the 14-bit CCSDS count).
    pub seq: u64,
    /// Send time, Unix nanoseconds.
    pub sent_ns: u64,
}

#[derive(serde::Deserialize)]
struct Envelope<'a> {
    #[serde(borrow)]
    payload_base64: std::borrow::Cow<'a, str>,
}

impl ProbeFrame {
    /// Reads the probe out of one `openmct.realtime.v1` message; `None` for anything else.
    #[must_use]
    pub fn decode(message: &str) -> Option<Self> {
        use base64::Engine as _;
        let env: Envelope<'_> = serde_json::from_str(message).ok()?;
        let data = base64::engine::general_purpose::STANDARD
            .decode(env.payload_base64.as_bytes())
            .ok()?;
        if data.len() < PROBE_DATA_LEN || &data[..4] != PROBE_MAGIC {
            return None;
        }
        let seq = u64::from_be_bytes(data[4..12].try_into().ok()?);
        let sent_ns = u64::from_be_bytes(data[12..20].try_into().ok()?);
        Some(Self { seq, sent_ns })
    }
}

/// Sub-buckets per power of two (6 bits → worst-case relative bucket width 1/64 ≈ 1.6 %).
const SUB: u32 = 64;
// Shifts 1..=57 cover every u64 (p = 63); index = shift * SUB + sub, sub in SUB..2*SUB.
const BUCKETS: usize = (SUB as usize) * 2 + (SUB as usize) * 57;

/// Log-linear latency histogram in microseconds.
#[derive(Clone)]
pub struct LatencyHistogram {
    counts: Box<[u64; BUCKETS]>,
    total: u64,
    max_us: u64,
    sum_us: u128,
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self {
            counts: Box::new([0; BUCKETS]),
            total: 0,
            max_us: 0,
            sum_us: 0,
        }
    }
}

impl std::fmt::Debug for LatencyHistogram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LatencyHistogram")
            .field("total", &self.total)
            .field("max_us", &self.max_us)
            .finish_non_exhaustive()
    }
}

fn bucket(us: u64) -> usize {
    if us < u64::from(2 * SUB) {
        return us as usize;
    }
    let p = 63 - us.leading_zeros(); // ≥ 7
    let shift = p - SUB.trailing_zeros(); // ≥ 1
    let sub = us >> shift; // in SUB..2*SUB
    ((shift * SUB) as usize + sub as usize).min(BUCKETS - 1)
}

/// The upper edge (inclusive) of bucket `i`, so percentiles never under-report.
fn bucket_high(i: usize) -> u64 {
    let i = i as u64;
    let sub_n = u64::from(SUB);
    if i < 2 * sub_n {
        return i;
    }
    let shift = i / sub_n - 1;
    let sub = i - shift * sub_n;
    let high = (u128::from(sub + 1) << shift) - 1;
    u64::try_from(high).unwrap_or(u64::MAX)
}

impl LatencyHistogram {
    /// Records one latency.
    pub fn record(&mut self, d: Duration) {
        let us = u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
        self.counts[bucket(us)] += 1;
        self.total += 1;
        self.max_us = self.max_us.max(us);
        self.sum_us += u128::from(us);
    }

    /// Adds every sample of `other`.
    pub fn merge(&mut self, other: &Self) {
        for (a, b) in self.counts.iter_mut().zip(other.counts.iter()) {
            *a += b;
        }
        self.total += other.total;
        self.max_us = self.max_us.max(other.max_us);
        self.sum_us += other.sum_us;
    }

    /// Number of samples.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.total
    }

    /// `true` before the first sample.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// The `q` quantile (0..=1) in microseconds, rounded up to its bucket edge; `None` if empty.
    #[must_use]
    pub fn quantile_us(&self, q: f64) -> Option<u64> {
        if self.total == 0 {
            return None;
        }
        let rank = ((q.clamp(0.0, 1.0) * self.total as f64).ceil() as u64).max(1);
        let mut seen = 0;
        for (i, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                return Some(bucket_high(i).min(self.max_us));
            }
        }
        Some(self.max_us)
    }

    /// Summary in milliseconds for the report.
    #[must_use]
    pub fn summary(&self) -> LatencySummary {
        let ms = |q: f64| self.quantile_us(q).map(|us| us as f64 / 1_000.0);
        LatencySummary {
            samples: self.total,
            mean_ms: (self.total > 0).then(|| self.sum_us as f64 / self.total as f64 / 1_000.0),
            p50_ms: ms(0.50),
            p90_ms: ms(0.90),
            p99_ms: ms(0.99),
            p999_ms: ms(0.999),
            max_ms: (self.total > 0).then(|| self.max_us as f64 / 1_000.0),
        }
    }
}

/// Latency percentiles in milliseconds.
#[derive(Debug, Clone, Serialize)]
pub struct LatencySummary {
    /// Samples behind the figures.
    pub samples: u64,
    /// Mean.
    pub mean_ms: Option<f64>,
    /// Median.
    pub p50_ms: Option<f64>,
    /// 90th percentile.
    pub p90_ms: Option<f64>,
    /// 99th percentile.
    pub p99_ms: Option<f64>,
    /// 99.9th percentile.
    pub p999_ms: Option<f64>,
    /// Largest sample.
    pub max_ms: Option<f64>,
}

/// Memory samples `(seconds since start, resident bytes)` and what they say about growth.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RssTrend {
    /// Every sample taken.
    pub samples: Vec<(f64, u64)>,
}

/// The verdict figures derived from an [`RssTrend`].
#[derive(Debug, Clone, Serialize)]
pub struct RssSummary {
    /// First sample after warm-up (MiB).
    pub after_warmup_mib: f64,
    /// Largest sample (MiB).
    pub peak_mib: f64,
    /// Last sample (MiB).
    pub last_mib: f64,
    /// Least-squares slope over the second half of the run (MiB per hour). A bounded process
    /// sits near zero once its buffers (history ring, broadcast channel) are full.
    pub second_half_slope_mib_per_h: f64,
}

impl RssTrend {
    /// Adds a sample.
    pub fn push(&mut self, t_s: f64, bytes: u64) {
        self.samples.push((t_s, bytes));
    }

    /// Summary ignoring samples before `warmup_s`; `None` without at least two samples.
    #[must_use]
    pub fn summary(&self, warmup_s: f64) -> Option<RssSummary> {
        const MIB: f64 = 1024.0 * 1024.0;
        let after: Vec<(f64, f64)> = self
            .samples
            .iter()
            .filter(|(t, _)| *t >= warmup_s)
            .map(|&(t, b)| (t, b as f64 / MIB))
            .collect();
        if after.len() < 2 {
            return None;
        }
        let half = &after[after.len() / 2..];
        let slope = if half.len() >= 2 {
            let n = half.len() as f64;
            let mt = half.iter().map(|p| p.0).sum::<f64>() / n;
            let mm = half.iter().map(|p| p.1).sum::<f64>() / n;
            let num: f64 = half.iter().map(|p| (p.0 - mt) * (p.1 - mm)).sum();
            let den: f64 = half.iter().map(|p| (p.0 - mt).powi(2)).sum();
            if den > 0.0 { num / den * 3_600.0 } else { 0.0 }
        } else {
            0.0
        };
        Some(RssSummary {
            after_warmup_mib: after[0].1,
            peak_mib: after.iter().map(|p| p.1).fold(f64::MIN, f64::max),
            last_mib: after[after.len() - 1].1,
            second_half_slope_mib_per_h: slope,
        })
    }
}

/// Parses `90s`, `10m`, `24h`, `2d` or plain seconds.
///
/// ```
/// use chronus_evidence::soak::parse_duration;
/// assert_eq!(parse_duration("24h").unwrap().as_secs(), 86_400);
/// assert_eq!(parse_duration("45").unwrap().as_secs(), 45);
/// assert!(parse_duration("soon").is_err());
/// ```
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1.0),
        Some('m') => (&s[..s.len() - 1], 60.0),
        Some('h') => (&s[..s.len() - 1], 3_600.0),
        Some('d') => (&s[..s.len() - 1], 86_400.0),
        _ => (s, 1.0),
    };
    let v: f64 = num
        .trim()
        .parse()
        .map_err(|_| format!("`{s}` is not a duration (try 90s, 10m, 24h)"))?;
    if !v.is_finite() || v < 0.0 {
        return Err(format!("`{s}` is not a duration (try 90s, 10m, 24h)"));
    }
    Ok(Duration::from_secs_f64(v * mult))
}

/// Reads one sample's value from a Prometheus text exposition.
#[must_use]
pub fn prometheus_value(text: &str, name: &str) -> Option<f64> {
    text.lines().filter(|l| !l.starts_with('#')).find_map(|l| {
        let (n, v) = l.rsplit_once(' ')?;
        (n == name).then(|| v.parse().ok()).flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_round_trips_through_the_gateway_envelope() {
        use base64::Engine as _;
        let p = probe_packet(0x123, 70_000, 1_700_000_000_123_456_789);
        assert_eq!(p.len(), 6 + PROBE_DATA_LEN);
        assert_eq!(u16::from_be_bytes([p[0], p[1]]), 0x123);
        let msg = format!(
            r#"{{"chronus_schema":"openmct.realtime.v1","apid":291,"payload_base64":"{}"}}"#,
            base64::engine::general_purpose::STANDARD.encode(&p[6..])
        );
        let f = ProbeFrame::decode(&msg).unwrap();
        assert_eq!(f.seq, 70_000);
        assert_eq!(f.sent_ns, 1_700_000_000_123_456_789);
        assert!(ProbeFrame::decode(r#"{"payload_base64":"SGVsbG8="}"#).is_none());
        assert!(ProbeFrame::decode("not json").is_none());
    }

    #[test]
    fn histogram_buckets_are_contiguous_and_tight() {
        let mut prev_high = None;
        for i in 0..BUCKETS {
            let lo = prev_high.map_or(0, |h: u64| h + 1);
            assert_eq!(bucket(lo), i, "bucket {i} starts at {lo}");
            let hi = bucket_high(i);
            assert_eq!(bucket(hi), i);
            if lo >= 128 {
                assert!((hi - lo + 1) as f64 / lo as f64 <= 1.0 / 64.0 + 1e-9);
            }
            prev_high = Some(hi);
        }
    }

    #[test]
    fn quantiles_are_within_a_bucket_and_never_low() {
        let mut h = LatencyHistogram::default();
        for us in 1..=10_000u64 {
            h.record(Duration::from_micros(us));
        }
        let p99 = h.quantile_us(0.99).unwrap();
        assert!((9_900..=9_900 + 9_900 / 64 + 1).contains(&p99), "{p99}");
        assert_eq!(h.quantile_us(1.0).unwrap(), 10_000);
        assert_eq!(h.quantile_us(0.0).unwrap(), 1);
        let mut m = LatencyHistogram::default();
        m.merge(&h);
        m.merge(&h);
        assert_eq!(m.len(), 20_000);
        assert_eq!(m.quantile_us(0.99), h.quantile_us(0.99));
        let s = m.summary();
        assert!((s.mean_ms.unwrap() - 5.0005).abs() < 1e-9);
        assert!(LatencyHistogram::default().summary().p99_ms.is_none());
    }

    #[test]
    fn rss_slope_flags_growth_and_ignores_warmup() {
        let mut flat = RssTrend::default();
        let mut leak = RssTrend::default();
        for i in 0..100 {
            let t = f64::from(i) * 60.0;
            // Warm-up ramp then flat at 40 MiB.
            let mib = if i < 10 { 10 + 3 * i } else { 40 };
            flat.push(t, mib as u64 * 1024 * 1024);
            leak.push(t, (40 * 1024 * 1024) + i as u64 * 1024 * 1024);
        }
        let f = flat.summary(600.0).unwrap();
        assert!(f.second_half_slope_mib_per_h.abs() < 1e-9);
        assert_eq!(f.peak_mib, 40.0);
        let l = leak.summary(600.0).unwrap();
        assert!((l.second_half_slope_mib_per_h - 60.0).abs() < 1e-6);
        assert!(RssTrend::default().summary(0.0).is_none());
    }

    #[test]
    fn durations_and_metrics_parse() {
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("1.5m").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("2d").unwrap(), Duration::from_secs(172_800));
        assert!(parse_duration("-3s").is_err());
        let text =
            "# HELP x y\n# TYPE x gauge\nprocess_resident_memory_bytes 1.2e7\nchronus_a_total 3\n";
        assert_eq!(
            prometheus_value(text, "process_resident_memory_bytes"),
            Some(1.2e7)
        );
        assert_eq!(prometheus_value(text, "chronus_a_total"), Some(3.0));
        assert_eq!(prometheus_value(text, "missing"), None);
    }
}
