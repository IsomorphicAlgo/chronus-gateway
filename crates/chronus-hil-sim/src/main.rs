//! CLI entry for the NeXosim HIL driver (`chronus-hil-sim`).
//!
//! Positional: `[DEST] [FRAMES]` — UDP destination and packet count (defaults `127.0.0.1:7301`, `100`).
//! Optional **scripted anomaly** (Showcase S3): inject synthetic CV-4/CV-5 faults for a bounded frame window.

use std::net::SocketAddr;

use anyhow::Context;
use clap::{Parser, ValueEnum};
use chronus_hil_sim::{
    HilScriptedAnomaly, HilScriptedAnomalyKind, run_nexosim_udp_hil_with_script,
};

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

#[derive(Parser, Debug)]
#[command(name = "chronus-hil-sim", version, about = "NeXosim synthetic HIL → UDP CCSDS TM")]
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

    tracing::info!(
        %dest,
        frames,
        apid = cli.apid,
        ?script,
        "starting NeXosim HIL downlink"
    );
    run_nexosim_udp_hil_with_script(dest, frames, cli.apid, script)
        .context("NeXosim UDP HIL")?;
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
