//! ChronusGateway-RS entrypoint.
//!
//! Runs the UDP ingest loop and the Axum HTTP/WebSocket server until Ctrl-C (Milestones 5–6).

use std::sync::Arc;

use anyhow::Context;
use chronus_gateway::config::{IngestConfig, StationConfig};
use chronus_gateway::http;
use chronus_gateway::ingest::{self, IngestStats};
use chronus_gateway::metrics::GatewayMetrics;
use chronus_gateway::propagator::{EphemerustPropagator, TrackingProvider};
use chronus_gateway::state::SharedGateway;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let ingest_cfg = IngestConfig::default();
    let socket = ingest::bind(&ingest_cfg)
        .await
        .with_context(|| format!("failed to bind UDP socket on {}", ingest_cfg.bind_addr))?;
    let udp_local = socket.local_addr()?;
    tracing::info!(%udp_local, "UDP telemetry ingest listening");

    let (frame_tx, _rx) = broadcast::channel(ingest_cfg.channel_capacity);
    let ingest_stats = Arc::new(IngestStats::default());
    let gateway_metrics = Arc::new(GatewayMetrics::default());
    let station = Arc::new(StationConfig::default());

    let tracking = match EphemerustPropagator::from_station(station.as_ref()) {
        Ok(prop) => {
            tracing::info!(
                lat = station.latitude_deg,
                lon = station.longitude_deg,
                carrier_hz = station.nominal_carrier_hz,
                "orbital tracking provider ready"
            );
            Some(Arc::new(TrackingProvider::new(
                Arc::new(prop),
                station.min_recompute_interval_ms,
            )))
        }
        Err(e) => {
            tracing::warn!(error = %e, "no orbital propagator; WebSocket payloads omit physics fields");
            None
        }
    };

    let state = Arc::new(SharedGateway::new(
        frame_tx.clone(),
        Arc::clone(&ingest_stats),
        Arc::clone(&gateway_metrics),
        Arc::clone(&station),
        tracking,
    ));

    let cancel = CancellationToken::new();
    let ingest_cancel = cancel.child_token();
    let ingest_handle = tokio::spawn({
        let ingest_stats = Arc::clone(&state.ingest_stats);
        let ingest_cfg_clone = ingest_cfg.clone();
        async move {
            ingest::run(
                socket,
                frame_tx,
                ingest_cfg_clone,
                ingest_stats,
                ingest_cancel.cancelled(),
            )
            .await
        }
    });

    let listener = TcpListener::bind(ingest_cfg.http_bind)
        .await
        .with_context(|| format!("failed to bind HTTP on {}", ingest_cfg.http_bind))?;
    let http_local = listener.local_addr()?;
    tracing::info!(%http_local, "HTTP + WebSocket (GET /telemetry/openmct) listening");

    let app = http::router(Arc::clone(&state));
    let cancel_serve = cancel.clone();
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        cancel_serve.cancel();
    });

    let server_res = server.await;
    server_res.context("HTTP server terminated with error")?;

    ingest_handle.await.context("ingest task join")??;

    let (frames, bytes, oversized, errors) = state.ingest_stats.snapshot();
    tracing::info!(frames, bytes, oversized, errors, "shutdown complete");
    Ok(())
}
