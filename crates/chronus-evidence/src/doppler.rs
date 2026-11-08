//! Real-RF Doppler residuals (Tranche 5.3, Methodology D-056).
//!
//! Takes receiver carrier measurements (`chronus.rf.v1` lines, e.g. from `tools/rf/iq_carrier.py`
//! run over an SDR recording), predicts each one with the gateway's own propagator and Doppler
//! arithmetic ([`expected_carrier_hz`], [`effective_doppler_tolerance_hz`]), and reports the
//! residuals against **T-DOPPLER**.
//!
//! ## Why a fit
//!
//! A real pass has two constant-ish errors that are not Doppler: the spacecraft's transmitter is
//! rarely exactly on its published frequency, and a consumer SDR's oscillator is off by a few
//! ppm (1 ppm = 437 Hz at 437 MHz), drifting slowly as it warms. Both are a property of the
//! hardware, not of the physics the gateway checks, so the tool removes a constant offset (and
//! optionally a linear drift) by least squares, reports it, and judges the *remaining* residual
//! against T-DOPPLER. The fitted offset is what an operator would enter as the corrected nominal
//! carrier.
//!
//! The **shape gain** `k` (least squares of the measured shift, fitted hardware terms removed,
//! on the predicted shift) tests the physics independently of the offset: the measured S-curve must have the predicted amplitude,
//! so `k ≈ 1`. A wrong TLE, wrong station position or a time-tag error shows up here first.

use chrono::{DateTime, Utc};
use chronus_gateway::propagator::OrbitalPropagator;
use chronus_gateway::rf::RfSampleV1;
use chronus_gateway::validate::{
    ValidationContext, effective_doppler_tolerance_hz, expected_carrier_hz,
};
use serde::Serialize;

/// One receiver carrier measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CarrierSample {
    /// Measurement time (UTC).
    pub t: DateTime<Utc>,
    /// Measured carrier (Hz).
    pub carrier_hz: f64,
}

/// Reads `chronus.rf.v1` lines (blank and `#` lines skipped). Samples without `t` or
/// `carrier_hz` are skipped; anything that is not a valid sample is an error naming the line.
///
/// # Errors
///
/// The first line that does not parse.
pub fn read_rf_jsonl(text: &str) -> Result<Vec<CarrierSample>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let s = RfSampleV1::parse(line.as_bytes()).map_err(|e| format!("line {}: {e}", i + 1))?;
        if let (Some(t), Some(carrier_hz)) = (s.t, s.carrier_hz) {
            out.push(CarrierSample { t, carrier_hz });
        }
    }
    out.sort_by_key(|s| s.t);
    Ok(out)
}

/// A measurement next to its prediction.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    /// Measurement time.
    pub t: DateTime<Utc>,
    /// Measured carrier (Hz).
    pub measured_hz: f64,
    /// Predicted carrier (Hz): nominal shifted by the propagated range rate.
    pub expected_hz: f64,
    /// Predicted Doppler shift (Hz), `expected − nominal`.
    pub shift_hz: f64,
    /// Geometric elevation (deg).
    pub elevation_deg: f64,
    /// Range rate (km/s, positive receding).
    pub range_rate_km_s: f64,
    /// The band the gateway would apply to this frame (T-DOPPLER plus any TLE-age margin).
    pub tolerance_hz: f64,
    /// `measured − expected` (Hz).
    pub residual_hz: f64,
    /// Residual after removing the fitted offset / drift (Hz).
    pub residual_fit_hz: f64,
}

/// What [`fit`] removes before judging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FitModel {
    /// Judge raw residuals (calibrated receiver and known transmitter frequency).
    None,
    /// Remove a constant offset (default).
    Offset,
    /// Remove a constant offset and a linear drift.
    OffsetDrift,
}

/// Predicts every sample at or above `min_elevation_deg`.
///
/// # Errors
///
/// A propagation failure (bad TLE, time outside an OEM's coverage).
pub fn evaluate(
    samples: &[CarrierSample],
    propagator: &dyn OrbitalPropagator,
    ctx: &ValidationContext,
    min_elevation_deg: f64,
) -> Result<Vec<Row>, String> {
    let f0 = ctx.nominal_carrier_hz;
    let mut rows = Vec::with_capacity(samples.len());
    for s in samples {
        let state = propagator
            .tracking_state(s.t)
            .map_err(|e| format!("{}: {e}", s.t))?;
        // NaN elevation (failed geometry) is skipped along with low samples.
        if state.elevation_deg.is_nan() || state.elevation_deg < min_elevation_deg {
            continue;
        }
        let expected = expected_carrier_hz(f0, state.range_rate_km_s);
        rows.push(Row {
            t: s.t,
            measured_hz: s.carrier_hz,
            expected_hz: expected,
            shift_hz: expected - f0,
            elevation_deg: state.elevation_deg,
            range_rate_km_s: state.range_rate_km_s,
            tolerance_hz: effective_doppler_tolerance_hz(ctx.doppler_tolerance_hz, f0, &state),
            residual_hz: s.carrier_hz - expected,
            residual_fit_hz: s.carrier_hz - expected,
        });
    }
    Ok(rows)
}

/// Least-squares line `y = a + b·x`; returns `(a, b, r²)`.
fn line_fit(xs: &[f64], ys: &[f64]) -> Option<(f64, f64, f64)> {
    let n = xs.len() as f64;
    if xs.len() < 2 {
        return None;
    }
    let mx = xs.iter().sum::<f64>() / n;
    let my = ys.iter().sum::<f64>() / n;
    let sxx: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
    let sxy: f64 = xs.iter().zip(ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    let syy: f64 = ys.iter().map(|y| (y - my).powi(2)).sum();
    if sxx <= 0.0 {
        return None;
    }
    let b = sxy / sxx;
    let r2 = if syy > 0.0 {
        (sxy * sxy) / (sxx * syy)
    } else {
        1.0
    };
    Some((my - b * mx, b, r2))
}

/// The hardware terms removed before judging.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Fit {
    /// Model used.
    pub model: FitModel,
    /// Constant offset (Hz) at the first sample.
    pub offset_hz: f64,
    /// Offset in parts per million of the nominal carrier.
    pub offset_ppm: f64,
    /// Linear drift (Hz/s), `0` unless [`FitModel::OffsetDrift`].
    pub drift_hz_per_s: f64,
    /// Samples the fit used (the rest were more than three bands off the first guess).
    pub inliers: usize,
}

/// Fits and removes the hardware terms in place (`residual_fit_hz`).
///
/// The fit is robust to the odd bad estimate (a block where the peak finder locked onto noise or
/// an interferer): it starts from medians, keeps the samples within three bands of that first
/// guess, and refits by least squares on those, twice. Every sample, outliers included, is still
/// judged afterwards; only the *fit* ignores them.
pub fn fit(rows: &mut [Row], model: FitModel, nominal_hz: f64) -> Fit {
    let Some(t0) = rows.first().map(|r| r.t) else {
        return Fit {
            model,
            offset_hz: 0.0,
            offset_ppm: 0.0,
            drift_hz_per_s: 0.0,
            inliers: 0,
        };
    };
    let xs: Vec<f64> = rows.iter().map(|r| secs(r.t, t0)).collect();
    let ys: Vec<f64> = rows.iter().map(|r| r.residual_hz).collect();
    let band: Vec<f64> = rows.iter().map(|r| 3.0 * r.tolerance_hz.max(1.0)).collect();
    let (mut offset, mut drift) = match model {
        FitModel::None => (0.0, 0.0),
        FitModel::Offset => (median(&ys), 0.0),
        FitModel::OffsetDrift => {
            // Line through the medians of the two halves.
            let h = ys.len() / 2;
            if h == 0 {
                (ys[0], 0.0)
            } else {
                let (x1, y1) = (median(&xs[..h]), median(&ys[..h]));
                let (x2, y2) = (median(&xs[h..]), median(&ys[h..]));
                let b = if x2 > x1 { (y2 - y1) / (x2 - x1) } else { 0.0 };
                (y1 - b * x1, b)
            }
        }
    };
    let mut inliers = ys.len();
    if model != FitModel::None {
        for _ in 0..2 {
            let keep: Vec<usize> = (0..ys.len())
                .filter(|&i| (ys[i] - offset - drift * xs[i]).abs() <= band[i])
                .collect();
            if keep.len() < 2 {
                break;
            }
            inliers = keep.len();
            let kx: Vec<f64> = keep.iter().map(|&i| xs[i]).collect();
            let ky: Vec<f64> = keep.iter().map(|&i| ys[i]).collect();
            if model == FitModel::Offset {
                offset = ky.iter().sum::<f64>() / ky.len() as f64;
            } else if let Some((a, b, _)) = line_fit(&kx, &ky) {
                (offset, drift) = (a, b);
            }
        }
    }
    for (r, x) in rows.iter_mut().zip(&xs) {
        r.residual_fit_hz = r.residual_hz - offset - drift * x;
    }
    Fit {
        model,
        offset_hz: offset,
        offset_ppm: offset / nominal_hz * 1e6,
        drift_hz_per_s: drift,
        inliers,
    }
}

fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    match s.len() {
        0 => 0.0,
        n if n % 2 == 1 => s[n / 2],
        n => (s[n / 2 - 1] + s[n / 2]) / 2.0,
    }
}

fn secs(t: DateTime<Utc>, t0: DateTime<Utc>) -> f64 {
    (t - t0).num_microseconds().unwrap_or(0) as f64 / 1e6
}

/// Residual statistics (Hz).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Stats {
    /// Mean.
    pub mean_hz: f64,
    /// Root mean square.
    pub rms_hz: f64,
    /// 95th percentile of |residual|.
    pub p95_abs_hz: f64,
    /// Largest |residual|.
    pub max_abs_hz: f64,
    /// Fraction of samples inside the band the gateway would apply.
    pub within_tolerance: f64,
}

fn stats(rows: &[Row], pick: impl Fn(&Row) -> f64) -> Option<Stats> {
    if rows.is_empty() {
        return None;
    }
    let n = rows.len() as f64;
    let v: Vec<f64> = rows.iter().map(&pick).collect();
    let mut abs: Vec<f64> = v.iter().map(|x| x.abs()).collect();
    abs.sort_by(f64::total_cmp);
    let idx = ((0.95 * n).ceil() as usize).clamp(1, abs.len()) - 1;
    Some(Stats {
        mean_hz: v.iter().sum::<f64>() / n,
        rms_hz: (v.iter().map(|x| x * x).sum::<f64>() / n).sqrt(),
        p95_abs_hz: abs[idx],
        max_abs_hz: abs[abs.len() - 1],
        within_tolerance: rows
            .iter()
            .filter(|r| pick(r).abs() <= r.tolerance_hz)
            .count() as f64
            / n,
    })
}

/// Everything the report states about one pass.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    /// Samples judged (at or above the elevation mask).
    pub samples: usize,
    /// First and last judged sample.
    pub span: Option<(DateTime<Utc>, DateTime<Utc>)>,
    /// Highest elevation reached (deg).
    pub max_elevation_deg: f64,
    /// Peak-to-peak predicted Doppler over the judged samples (Hz).
    pub predicted_swing_hz: f64,
    /// Raw residuals.
    pub raw: Option<Stats>,
    /// Residuals after the fit.
    pub fitted: Option<Stats>,
    /// The removed hardware terms.
    pub fit: Fit,
    /// Least-squares slope of `measured − f₀` on the predicted shift (1 = physics right).
    pub shape_gain: Option<f64>,
    /// r² of that regression.
    pub shape_r2: Option<f64>,
}

/// Summarises evaluated, fitted rows.
#[must_use]
pub fn summarize(rows: &[Row], fit: Fit) -> Summary {
    // The shape regression uses the same inlier rule as the fit, so a handful of wild estimates
    // cannot fake or hide a slope error; the swing is over every judged sample.
    let inl: Vec<&Row> = rows
        .iter()
        .filter(|r| r.residual_fit_hz.abs() <= 3.0 * r.tolerance_hz.max(1.0))
        .collect();
    let shape = line_fit(
        &inl.iter().map(|r| r.shift_hz).collect::<Vec<_>>(),
        // Measured shift with the fitted hardware terms removed: residual_fit + predicted shift.
        &inl.iter()
            .map(|r| r.residual_fit_hz + r.shift_hz)
            .collect::<Vec<_>>(),
    );
    let xs: Vec<f64> = rows.iter().map(|r| r.shift_hz).collect();
    let lo = xs.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Summary {
        samples: rows.len(),
        span: rows.first().zip(rows.last()).map(|(a, b)| (a.t, b.t)),
        max_elevation_deg: rows
            .iter()
            .map(|r| r.elevation_deg)
            .fold(f64::NEG_INFINITY, f64::max),
        predicted_swing_hz: if rows.is_empty() { 0.0 } else { hi - lo },
        raw: stats(rows, |r| r.residual_hz),
        fitted: stats(rows, |r| r.residual_fit_hz),
        fit,
        shape_gain: shape.map(|s| s.1),
        shape_r2: shape.map(|s| s.2),
    }
}

/// The 1.0 acceptance rule for a real pass: at least `min_within` of the fitted residuals inside
/// the band, and the shape gain within `gain_tol` of 1. Returns the failures (empty = pass).
#[must_use]
pub fn judge(s: &Summary, min_within: f64, gain_tol: f64, min_samples: usize) -> Vec<String> {
    let mut fails = Vec::new();
    if s.samples < min_samples {
        fails.push(format!(
            "only {} samples above the mask (need {min_samples})",
            s.samples
        ));
    }
    match s.fitted {
        Some(f) if f.within_tolerance >= min_within => {}
        Some(f) => fails.push(format!(
            "{:.1} % of residuals inside T-DOPPLER (need {:.1} %)",
            f.within_tolerance * 100.0,
            min_within * 100.0
        )),
        None => fails.push("no samples".into()),
    }
    match s.shape_gain {
        Some(k) if (k - 1.0).abs() <= gain_tol => {}
        Some(k) => fails.push(format!(
            "shape gain {k:.4} is not within {gain_tol} of 1 (TLE, station or time tags?)"
        )),
        None => fails.push("shape gain undefined (no Doppler swing)".into()),
    }
    fails
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use chronus_gateway::config::StationConfig;
    use chronus_gateway::propagator::propagator_from_station;

    /// A synthetic receiver: the true Doppler curve plus the errors a real one has.
    fn synthetic_pass(offset_hz: f64, drift_hz_s: f64, noise_hz: f64) -> Vec<CarrierSample> {
        let station = StationConfig::default();
        let prop = propagator_from_station(&station).unwrap();
        let f0 = station.site.nominal_carrier_hz;
        let t0 = Utc.with_ymd_and_hms(2020, 7, 13, 5, 4, 0).unwrap();
        // Deterministic pseudo-noise (no RNG dependency): a fixed irrational-step sequence.
        let mut u = 0.123_f64;
        (0..600)
            .map(|i| {
                let t = t0 + chrono::Duration::seconds(i);
                let st = prop.tracking_state(t).unwrap();
                u = (u + 0.618_033_988_75) % 1.0;
                CarrierSample {
                    t,
                    carrier_hz: expected_carrier_hz(f0, st.range_rate_km_s)
                        + offset_hz
                        + drift_hz_s * i as f64
                        + noise_hz * (2.0 * u - 1.0),
                }
            })
            .collect()
    }

    fn run(samples: &[CarrierSample], model: FitModel) -> Summary {
        let station = StationConfig::default();
        let prop = propagator_from_station(&station).unwrap();
        let ctx = ValidationContext::from_station(&station);
        let mut rows = evaluate(samples, prop.as_ref(), &ctx, 0.0).unwrap();
        let f = fit(&mut rows, model, ctx.nominal_carrier_hz);
        summarize(&rows, f)
    }

    #[test]
    fn offset_receiver_passes_once_the_offset_is_fitted() {
        // 5 ppm SDR at 437.5 MHz ≈ +2.19 kHz: raw residuals fail, fitted ones pass.
        let s = run(&synthetic_pass(2_187.5, 0.0, 20.0), FitModel::Offset);
        assert!(s.samples > 100, "pass above the horizon: {}", s.samples);
        assert!(s.predicted_swing_hz > 10_000.0, "{}", s.predicted_swing_hz);
        assert!(s.raw.unwrap().within_tolerance < 0.01);
        let f = s.fitted.unwrap();
        assert_eq!(f.within_tolerance, 1.0);
        // Noise bound plus the fitted offset's own error (the noise mean over the pass).
        assert!(f.max_abs_hz <= 22.0, "{}", f.max_abs_hz);
        assert!(
            (s.fit.offset_hz - 2_187.5).abs() < 2.0,
            "{}",
            s.fit.offset_hz
        );
        assert!((s.fit.offset_ppm - 5.0).abs() < 0.01);
        assert!((s.shape_gain.unwrap() - 1.0).abs() < 1e-3);
        assert!(judge(&s, 0.95, 0.05, 30).is_empty());
    }

    #[test]
    fn drift_needs_the_drift_model() {
        let p = synthetic_pass(-900.0, 1.5, 5.0);
        let offset_only = run(&p, FitModel::Offset);
        let with_drift = run(&p, FitModel::OffsetDrift);
        assert!(with_drift.fitted.unwrap().max_abs_hz <= 6.0);
        assert!((with_drift.fit.drift_hz_per_s - 1.5).abs() < 0.01);
        assert!(offset_only.fitted.unwrap().max_abs_hz > 100.0);
    }

    #[test]
    fn a_few_wild_estimates_do_not_move_the_fit() {
        // Peak-finder slips (noise, an interferer) look like this: a handful of samples kHz away.
        let mut p = synthetic_pass(1_000.0, 0.0, 10.0);
        for i in [150, 200, 250, 300, 350] {
            p[i].carrier_hz += 5_000.0;
        }
        let s = run(&p, FitModel::Offset);
        assert!(
            (s.fit.offset_hz - 1_000.0).abs() < 2.0,
            "{}",
            s.fit.offset_hz
        );
        assert_eq!(s.fit.inliers, s.samples - 5);
        assert!((s.shape_gain.unwrap() - 1.0).abs() < 1e-3);
        let f = s.fitted.unwrap();
        assert!(f.within_tolerance < 1.0 && f.within_tolerance > 0.98);
        assert!(f.max_abs_hz > 4_900.0, "outliers are still judged");
    }

    #[test]
    fn wrong_physics_is_caught_by_the_shape_gain() {
        // A curve 20 % too steep (e.g. the wrong satellite or a bad TLE) cannot be fitted away.
        let mut p = synthetic_pass(0.0, 0.0, 0.0);
        let station = StationConfig::default();
        let f0 = station.site.nominal_carrier_hz;
        for s in &mut p {
            s.carrier_hz = f0 + (s.carrier_hz - f0) * 1.2;
        }
        let s = run(&p, FitModel::Offset);
        assert!((s.shape_gain.unwrap() - 1.2).abs() < 1e-3);
        let fails = judge(&s, 0.95, 0.05, 30);
        assert!(fails.iter().any(|f| f.contains("shape gain")), "{fails:?}");
    }

    #[test]
    fn reads_rf_lines_and_reports_bad_ones() {
        let text = "# capture\n\
            {\"chronus_schema\":\"chronus.rf.v1\",\"t\":\"2020-07-13T05:08:31Z\",\"carrier_hz\":437500100.0}\n\
            {\"chronus_schema\":\"chronus.rf.v1\",\"t\":\"2020-07-13T05:08:30Z\",\"carrier_hz\":437500000.0}\n\
            {\"chronus_schema\":\"chronus.rf.v1\",\"rx_power_dbm\":-110.0}\n";
        let s = read_rf_jsonl(text).unwrap();
        assert_eq!(s.len(), 2);
        assert!(s[0].t < s[1].t);
        let e = read_rf_jsonl("{\"chronus_schema\":\"other\"}").unwrap_err();
        assert!(e.starts_with("line 1"), "{e}");
    }
}
