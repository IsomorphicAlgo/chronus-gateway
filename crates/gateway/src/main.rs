//! ChronusGateway-RS entrypoint.
//!
//! Milestone 1: binds the UDP downlink socket and runs the asynchronous ingestion loop,
//! logging received frames and final statistics. Later milestones extend this into the full
//! pipeline: CCSDS parse → physics co-validation → Open MCT WebSocket fan-out.

use std::sync::Arc;

use anyhow::Context;
use chronus_gateway::config::IngestConfig;
use chronus_gateway::ingest::{self, IngestStats, RawFrame};
use tokio::sync::broadcast;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = IngestConfig::default();
    let socket = ingest::bind(&config)
        .await
        .with_context(|| format!("failed to bind UDP socket on {}", config.bind_addr))?;
    let local = socket.local_addr()?;
    tracing::info!(%local, "ChronusGateway-RS listening for telemetry");

    let (tx, mut rx) = broadcast::channel::<RawFrame>(config.channel_capacity);
    let stats = Arc::new(IngestStats::default());

    // Demonstration consumer: log a one-line summary of each received frame. Real downstream
    // stages (CCSDS parse → validate → distribute) replace this in later milestones.
    let logger = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(frame) => tracing::info!(
                    bytes = frame.bytes.len(),
                    source = %frame.source,
                    "frame received"
                ),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "consumer lagged; dropped frames")
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Run until Ctrl-C.
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    ingest::run(socket, tx, config, Arc::clone(&stats), shutdown).await?;

    logger.abort();
    let (frames, bytes, oversized, errors) = stats.snapshot();
    tracing::info!(frames, bytes, oversized, errors, "shutdown complete");
    Ok(())
}
