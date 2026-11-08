//! `chronus-doppler` — judges real-RF carrier measurements against the gateway's Doppler
//! prediction and **T-DOPPLER** (Tranche 5.3, Methodology D-056).
//!
//! ```text
//! chronus-doppler --config pass.toml --rf pass.rf.jsonl --csv pass.csv --markdown pass.md
//! ```
//!
//! The configuration is an ordinary gateway TOML: the station position, the TLE (or OEM) of the
//! satellite and `site.nominal_carrier_hz` are the ones the gateway itself would use. See
//! `docs/RF_VALIDATION.md` for the capture procedure.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use chronus_evidence::doppler::{
    FitModel, Summary, evaluate, fit, judge, read_rf_jsonl, summarize,
};
use chronus_gateway::config::load_gateway_from_path;
use chronus_gateway::diagnose::Explain as _;
use chronus_gateway::propagator::propagator_from_station;
use chronus_gateway::validate::ValidationContext;
use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Model {
    /// Judge raw residuals (calibrated receiver, known transmitter frequency).
    None,
    /// Remove a constant offset first (default).
    Offset,
    /// Remove a constant offset and a linear drift first.
    OffsetDrift,
}

#[derive(Parser, Debug)]
#[command(version, about = "Doppler residuals of a real pass against T-DOPPLER")]
struct Args {
    /// Gateway TOML with the station, ephemeris and nominal carrier of the pass.
    #[arg(long, short)]
    config: PathBuf,
    /// Receiver measurements, one `chronus.rf.v1` JSON object per line.
    #[arg(long, required_unless_present = "predict_start")]
    rf: Option<PathBuf>,
    /// Instead of judging: print the predicted carrier from this UTC time as `chronus.rf.v1`
    /// lines (lab tooling: the synthetic-IQ rehearsal in docs/RF_VALIDATION.md, pass planning).
    #[arg(long)]
    predict_start: Option<chrono::DateTime<chrono::Utc>>,
    /// Prediction length, seconds.
    #[arg(long, default_value_t = 600)]
    predict_seconds: u32,
    /// Prediction step, seconds.
    #[arg(long, default_value_t = 1.0)]
    predict_step: f64,
    /// Hardware terms removed before judging.
    #[arg(long, value_enum, default_value_t = Model::Offset)]
    fit: Model,
    /// Ignore samples below this elevation (deg); low passes are multipath and noise.
    #[arg(long, default_value_t = 5.0)]
    min_elevation: f64,
    /// Fraction of fitted residuals that must lie inside the band.
    #[arg(long, default_value_t = 0.95)]
    require_within: f64,
    /// Allowed |shape gain − 1|.
    #[arg(long, default_value_t = 0.05)]
    gain_tolerance: f64,
    /// Minimum samples above the mask for a verdict.
    #[arg(long, default_value_t = 30)]
    min_samples: usize,
    /// Per-sample CSV output.
    #[arg(long)]
    csv: Option<PathBuf>,
    /// JSON summary output.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Markdown summary output.
    #[arg(long)]
    markdown: Option<PathBuf>,
}

fn main() {
    if let Err(e) = run(Args::parse()) {
        eprintln!("error: {e:#}");
        std::process::exit(2);
    }
}

fn run(a: Args) -> Result<()> {
    let (_ingest, station) = load_gateway_from_path(&a.config)
        .map_err(|e| anyhow!("{}", e.explain()))
        .with_context(|| format!("loading {}", a.config.display()))?;
    let prop = propagator_from_station(&station).map_err(|e| anyhow!("{e}"))?;
    let ctx = ValidationContext::from_station(&station);
    if let Some(start) = a.predict_start {
        return predict(
            prop.as_ref(),
            &ctx,
            start,
            a.predict_seconds,
            a.predict_step,
        );
    }
    let rf = a.rf.clone().context("--rf is required")?;
    let text = std::fs::read_to_string(&rf).with_context(|| format!("reading {}", rf.display()))?;
    let samples = read_rf_jsonl(&text).map_err(|e| anyhow!("{}: {e}", rf.display()))?;
    let mut rows =
        evaluate(&samples, prop.as_ref(), &ctx, a.min_elevation).map_err(|e| anyhow!("{e}"))?;
    let model = match a.fit {
        Model::None => FitModel::None,
        Model::Offset => FitModel::Offset,
        Model::OffsetDrift => FitModel::OffsetDrift,
    };
    let f = fit(&mut rows, model, ctx.nominal_carrier_hz);
    let summary = summarize(&rows, f);
    let fails = judge(&summary, a.require_within, a.gain_tolerance, a.min_samples);

    if let Some(p) = &a.csv {
        let mut s = String::from(
            "t,measured_hz,expected_hz,shift_hz,elevation_deg,range_rate_km_s,tolerance_hz,residual_hz,residual_fit_hz\n",
        );
        for r in &rows {
            s += &format!(
                "{},{:.3},{:.3},{:.3},{:.4},{:.6},{:.3},{:.3},{:.3}\n",
                r.t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                r.measured_hz,
                r.expected_hz,
                r.shift_hz,
                r.elevation_deg,
                r.range_rate_km_s,
                r.tolerance_hz,
                r.residual_hz,
                r.residual_fit_hz
            );
        }
        std::fs::write(p, s).with_context(|| format!("writing {}", p.display()))?;
    }
    if let Some(p) = &a.report {
        let j = serde_json::json!({
            "tool": concat!("chronus-doppler ", env!("CARGO_PKG_VERSION")),
            "rf": rf.display().to_string(),
            "nominal_carrier_hz": ctx.nominal_carrier_hz,
            "t_doppler_hz": ctx.doppler_tolerance_hz,
            "min_elevation_deg": a.min_elevation,
            "summary": summary,
            "verdict": if fails.is_empty() { vec!["PASS".to_string()] } else { fails.clone() },
        });
        std::fs::write(p, serde_json::to_string_pretty(&j)?)
            .with_context(|| format!("writing {}", p.display()))?;
    }
    let md = markdown(
        &summary,
        &a,
        &rf,
        ctx.nominal_carrier_hz,
        ctx.doppler_tolerance_hz,
        &fails,
    );
    println!("{md}");
    if let Some(p) = &a.markdown {
        std::fs::write(p, &md).with_context(|| format!("writing {}", p.display()))?;
    }
    if !fails.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

/// Predicted carrier (no measurement errors) with elevation, as `chronus.rf.v1` lines.
fn predict(
    prop: &dyn chronus_gateway::propagator::OrbitalPropagator,
    ctx: &ValidationContext,
    start: chrono::DateTime<chrono::Utc>,
    seconds: u32,
    step: f64,
) -> Result<()> {
    use std::io::Write as _;
    anyhow::ensure!(
        step > 0.0 && step.is_finite(),
        "--predict-step must be positive"
    );
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    let n = (f64::from(seconds) / step).floor() as u64;
    for i in 0..=n {
        let t = start + chrono::Duration::microseconds((i as f64 * step * 1e6) as i64);
        let st = prop.tracking_state(t).map_err(|e| anyhow!("{t}: {e}"))?;
        let f = chronus_gateway::validate::expected_carrier_hz(
            ctx.nominal_carrier_hz,
            st.range_rate_km_s,
        );
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "chronus_schema": "chronus.rf.v1",
                "t": t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                "carrier_hz": (f * 1000.0).round() / 1000.0,
                "az_deg": (st.azimuth_deg * 1000.0).round() / 1000.0,
                "el_deg": (st.elevation_deg * 1000.0).round() / 1000.0,
            })
        )?;
    }
    Ok(())
}

fn markdown(
    s: &Summary,
    a: &Args,
    rf: &std::path::Path,
    f0: f64,
    tol: f64,
    fails: &[String],
) -> String {
    let st = |x: Option<chronus_evidence::doppler::Stats>| {
        x.map_or("–".into(), |x| {
            format!(
                "{:+.1} / {:.1} / {:.1} / {:.1} / {:.1} %",
                x.mean_hz,
                x.rms_hz,
                x.p95_abs_hz,
                x.max_abs_hz,
                x.within_tolerance * 100.0
            )
        })
    };
    let mut m = format!(
        "## Doppler residuals: {}\n\n| | |\n|---|---|\n",
        rf.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    );
    if let Some((t0, t1)) = s.span {
        m += &format!(
            "| span | {} → {} |\n",
            t0.format("%FT%TZ"),
            t1.format("%FT%TZ")
        );
    }
    m += &format!(
        "| samples ≥ {}° | {} (max elevation {:.1}°) |\n",
        a.min_elevation, s.samples, s.max_elevation_deg
    );
    m += &format!("| nominal carrier | {f0:.1} Hz, T-DOPPLER ±{tol} Hz |\n");
    m += &format!(
        "| predicted Doppler swing | {:.0} Hz |\n",
        s.predicted_swing_hz
    );
    m += &format!(
        "| fit ({:?}, {} of {} samples) | offset {:+.1} Hz ({:+.2} ppm), drift {:+.3} Hz/s |\n",
        s.fit.model,
        s.fit.inliers,
        s.samples,
        s.fit.offset_hz,
        s.fit.offset_ppm,
        s.fit.drift_hz_per_s
    );
    m += "| residual mean / RMS / p95 / max (Hz) / inside band | |\n";
    m += &format!("| — raw | {} |\n", st(s.raw));
    m += &format!("| — after fit | {} |\n", st(s.fitted));
    m += &format!(
        "| shape gain (1 = physics right) | {} (r² {}) |\n",
        s.shape_gain.map_or("–".into(), |k| format!("{k:.4}")),
        s.shape_r2.map_or("–".into(), |r| format!("{r:.5}"))
    );
    m += &format!(
        "| verdict | {} |\n",
        if fails.is_empty() {
            "PASS".to_string()
        } else {
            format!("FAIL: {}", fails.join("; "))
        }
    );
    m
}
