//! CLI entry for the NeXosim HIL driver (`chronus-hil-sim`).
//!
//! Positional: `[DEST] [FRAMES]` — UDP destination and packet count (defaults `127.0.0.1:7301`, `100`).
//! Optional **scripted anomaly** (Showcase S3): inject synthetic CV-4/CV-5 faults for a bounded frame window.

#![forbid(unsafe_code)]

use std::net::SocketAddr;

use anyhow::Context;
use chronus_gateway::TimeFormat;
use chronus_gateway::config::StationConfig;
use chronus_hil_sim::rf::{RfFaultKind, RfModel, RfScriptedAnomaly};
use chronus_hil_sim::{
    HilRunOptions, HilScriptedAnomaly, HilScriptedAnomalyKind, default_hil_epoch_utc,
    run_nexosim_hil,
};
use clap::{Parser, ValueEnum};

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, ValueEnum)]
enum ScriptedAnomalyCli {
    /// Nominal self-consistent HIL only.
    #[default]
    None,
    /// Force EPS voltage out of band vs illumination model (`physics_flags` bit 4).
    EpsVoltage,
    /// Force panel temperature out of band (bit 5).
    Thermal,
    /// Force body rate above default ceiling (bit 6).
    BodyRate,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, ValueEnum)]
enum TimeCodeCli {
    /// Legacy packets with no secondary header.
    #[default]
    None,
    /// CCSDS Day Segmented time code (UTC); gateway `time.format = "cds"`.
    Cds,
    /// CCSDS Unsegmented time code (TAI); gateway `time.format = "cuc"`.
    Cuc,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, ValueEnum)]
enum RfAnomalyCli {
    /// Nominal, self-consistent receiver measurements.
    #[default]
    None,
    /// Carrier offset beyond T-DOPPLER (`physics_flags` bit 0).
    Doppler,
    /// Received power beyond the link-budget tolerance (bit 2).
    LinkBudget,
    /// Antenna pointing beyond T-POINT (bit 3).
    Pointing,
}

#[derive(Parser, Debug)]
#[command(
    name = "chronus-hil-sim",
    version,
    about = "NeXosim synthetic HIL → UDP CCSDS TM"
)]
struct Cli {
    /// Gateway UDP ingest `HOST:PORT` (positional **1**; default `127.0.0.1:7301`).
    #[arg(index = 1)]
    dest: Option<SocketAddr>,
    /// Number of synthetic TM packets (positional **2**; default `100`).
    #[arg(index = 2)]
    frames: Option<u32>,
    /// CCSDS APID (must be in gateway HIL band, default **0x7B0**).
    #[arg(long, default_value_t = 0x7B0)]
    apid: u16,
    /// Scripted subsystem fault kind (synthetic CV-4 / CV-5 only).
    #[arg(long, value_enum, default_value_t = ScriptedAnomalyCli::None)]
    scripted_anomaly: ScriptedAnomalyCli,
    /// First 0-based frame index at which to inject the fault (inclusive).
    #[arg(long, default_value_t = 50)]
    anomaly_after_frame: u32,
    /// How many consecutive frames carry the fault; **0** disables injection even if kind ≠ none.
    #[arg(long, default_value_t = 20)]
    anomaly_frame_count: u32,
    /// Put this time code in every packet's secondary header (carries the simulated mission time).
    /// The gateway must run with a matching `[time] format`; use `max_skew_s = 0` (lab mode) when
    /// the mission time is not "now".
    #[arg(long, value_enum, default_value_t = TimeCodeCli::None)]
    time_code: TimeCodeCli,
    /// Mission time of frame 0 (RFC 3339 UTC). Default: the public ISS TLE epoch, 2020-07-12 21:00.
    #[arg(long)]
    epoch: Option<chrono::DateTime<chrono::Utc>>,
    /// Start on the first moment the ISS is >= 10 degrees above the default station's horizon
    /// (searching 3 days from `--epoch`), so the pass is realistic instead of below the horizon.
    #[arg(long)]
    epoch_visible: bool,
    /// Gateway `rf.bind_addr` (`HOST:PORT`): send a consistent `chronus.rf.v1` sample before every
    /// frame, enabling the Doppler / link-budget / pointing checks. Frames are paced (20 ms).
    #[arg(long)]
    rf_dest: Option<SocketAddr>,
    /// Scripted receiver fault (requires `--rf-dest`).
    #[arg(long, value_enum, default_value_t = RfAnomalyCli::None)]
    rf_anomaly: RfAnomalyCli,
    /// First 0-based frame index of the receiver fault (inclusive).
    #[arg(long, default_value_t = 50)]
    rf_anomaly_after_frame: u32,
    /// Consecutive frames carrying the receiver fault; **0** disables it.
    #[arg(long, default_value_t = 20)]
    rf_anomaly_frame_count: u32,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let dest = cli
        .dest
        .unwrap_or_else(|| "127.0.0.1:7301".parse().expect("default dest"));
    let frames = cli.frames.unwrap_or(100);
    let script = scripted_from_cli(&cli);

    let mut epoch = cli.epoch.unwrap_or_else(default_hil_epoch_utc);
    if cli.epoch_visible {
        let model = RfModel::from_station(&StationConfig::default()).context("RF model")?;
        epoch = model
            .next_visible(epoch, chrono::Duration::days(3), 10.0)
            .context("no pass >= 10 deg within 3 days of the epoch")?;
        tracing::info!(%epoch, "starting on a visible pass");
    }
    let time_format = match cli.time_code {
        TimeCodeCli::None => TimeFormat::None,
        TimeCodeCli::Cds => TimeFormat::Cds,
        TimeCodeCli::Cuc => TimeFormat::Cuc,
    };
    if time_format == TimeFormat::None && (cli.epoch.is_some() || cli.epoch_visible) {
        tracing::warn!(
            "--epoch/--epoch-visible only affect the RF model and HIL geometry unless --time-code is set; \
             the gateway will still evaluate physics at its own receive time"
        );
    }
    let mut opts = HilRunOptions::new(dest, frames, cli.apid)
        .with_script(script)
        .with_time(time_format, epoch);
    if let Some(rf_dest) = cli.rf_dest {
        opts = opts.with_rf(rf_dest, rf_script_from_cli(&cli));
    } else if cli.rf_anomaly != RfAnomalyCli::None {
        anyhow::bail!("--rf-anomaly requires --rf-dest");
    }

    tracing::info!(
        %dest,
        frames,
        apid = cli.apid,
        ?script,
        "starting NeXosim HIL downlink"
    );
    run_nexosim_hil(opts).context("NeXosim UDP HIL")?;
    tracing::info!("NeXosim HIL run complete");
    Ok(())
}

fn scripted_from_cli(cli: &Cli) -> Option<HilScriptedAnomaly> {
    let kind = match cli.scripted_anomaly {
        ScriptedAnomalyCli::None => return None,
        ScriptedAnomalyCli::EpsVoltage => HilScriptedAnomalyKind::EpsVoltage,
        ScriptedAnomalyCli::Thermal => HilScriptedAnomalyKind::Thermal,
        ScriptedAnomalyCli::BodyRate => HilScriptedAnomalyKind::BodyRate,
    };
    if cli.anomaly_frame_count == 0 {
        tracing::warn!("scripted_anomaly is set but anomaly_frame_count is 0; no fault injected");
        return None;
    }
    Some(HilScriptedAnomaly {
        kind,
        start_frame: cli.anomaly_after_frame,
        duration_frames: cli.anomaly_frame_count,
    })
}

fn rf_script_from_cli(cli: &Cli) -> Option<RfScriptedAnomaly> {
    let kind = match cli.rf_anomaly {
        RfAnomalyCli::None => return None,
        RfAnomalyCli::Doppler => RfFaultKind::Doppler,
        RfAnomalyCli::LinkBudget => RfFaultKind::LinkBudget,
        RfAnomalyCli::Pointing => RfFaultKind::Pointing,
    };
    if cli.rf_anomaly_frame_count == 0 {
        tracing::warn!("rf_anomaly is set but rf_anomaly_frame_count is 0; no fault injected");
        return None;
    }
    Some(RfScriptedAnomaly {
        kind,
        start_frame: cli.rf_anomaly_after_frame,
        duration_frames: cli.rf_anomaly_frame_count,
    })
}
