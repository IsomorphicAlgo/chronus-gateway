//! Shared application state for ingestion, HTTP/WebSocket distribution, and metrics.

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::config::StationConfig;
use crate::ingest::{IngestStats, RawFrame};
use crate::metrics::GatewayMetrics;
use crate::propagator::TrackingProvider;

/// Process-wide handles shared between the UDP ingest task and the Axum server.
pub struct SharedGateway {
    /// Fan-out of raw datagrams to WebSocket subscribers (and any other consumers).
    pub frame_tx: broadcast::Sender<RawFrame>,
    /// UDP ingest counters.
    pub ingest_stats: Arc<IngestStats>,
    /// Telemetry / WebSocket counters (Milestone 6).
    pub gateway_metrics: Arc<GatewayMetrics>,
    /// Station + validation thresholds (immutable at runtime for the MVP).
    pub station: Arc<StationConfig>,
    /// Optional SGP4-backed tracking (throttled).
    pub tracking: Option<Arc<TrackingProvider>>,
}

impl SharedGateway {
    /// Builds shared state; does **not** spawn tasks.
    pub fn new(
        frame_tx: broadcast::Sender<RawFrame>,
        ingest_stats: Arc<IngestStats>,
        gateway_metrics: Arc<GatewayMetrics>,
        station: Arc<StationConfig>,
        tracking: Option<Arc<TrackingProvider>>,
    ) -> Self {
        Self {
            frame_tx,
            ingest_stats,
            gateway_metrics,
            station,
            tracking,
        }
    }
}
