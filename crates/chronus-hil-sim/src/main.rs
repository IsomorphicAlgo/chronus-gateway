//! CLI entry for the NeXosim HIL driver (`chronus-hil-sim`).
//!
//! Usage: `chronus-hil-sim [DEST] [FRAMES]`
//! - `DEST`: `HOST:PORT` for the gateway UDP ingest (default `127.0.0.1:7301`).
//! - `FRAMES`: number of synthetic TM packets to emit (default `100`).

use std::env;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut args = env::args().skip(1);
    let dest = args
        .next()
        .unwrap_or_else(|| "127.0.0.1:7301".to_string())
        .parse()?;
    let frames: u32 = args
        .next()
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(100);
    let apid = 0x7B0u16;

    tracing::info!(%dest, frames, apid, "starting NeXosim HIL downlink");
    chronus_hil_sim::run_nexosim_udp_hil(dest, frames, apid)?;
    tracing::info!("NeXosim HIL run complete");
    Ok(())
}
