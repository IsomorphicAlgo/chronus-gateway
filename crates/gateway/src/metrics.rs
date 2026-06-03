//! Gateway-wide counters for observability (Milestone 6).
//!
//! Complements [`crate::ingest::IngestStats`] (UDP path) with telemetry-path and WebSocket
//! counters. Exposed as JSON via [`crate::http::router`] at `GET /api/v1/chronus/metrics`.

use std::sync::atomic::{AtomicU64, Ordering};

/// Counters updated by the HTTP / WebSocket distribution path and telemetry processing.
#[derive(Debug, Default)]
pub struct GatewayMetrics {
    /// Successfully parsed TM frames emitted on WebSocket (one JSON message per frame).
    pub telemetry_frames_emitted: AtomicU64,
    /// Datagrams that failed CCSDS TM parse on the distribution path.
    pub telemetry_parse_errors: AtomicU64,
    /// Frames with non-zero [`crate::ccsds::TelemetryFrame::physics_flags`].
    pub anomaly_frames: AtomicU64,
    /// WebSocket text messages successfully sent to clients.
    pub ws_messages_sent: AtomicU64,
    /// Sum of processing latency (receive → JSON ready) in milliseconds.
    pub processing_latency_ms_sum: AtomicU64,
    /// Count of latency samples (for average = sum / count).
    pub processing_latency_ms_count: AtomicU64,
    /// Current WebSocket client connections (best-effort; decremented on disconnect).
    pub ws_clients_connected: AtomicU64,
}

impl GatewayMetrics {
    /// Snapshot for JSON serialization (all `Relaxed` loads).
    pub fn snapshot(&self) -> GatewayMetricsSnapshot {
        GatewayMetricsSnapshot {
            telemetry_frames_emitted: self.telemetry_frames_emitted.load(Ordering::Relaxed),
            telemetry_parse_errors: self.telemetry_parse_errors.load(Ordering::Relaxed),
            anomaly_frames: self.anomaly_frames.load(Ordering::Relaxed),
            ws_messages_sent: self.ws_messages_sent.load(Ordering::Relaxed),
            processing_latency_ms_sum: self.processing_latency_ms_sum.load(Ordering::Relaxed),
            processing_latency_ms_count: self.processing_latency_ms_count.load(Ordering::Relaxed),
            ws_clients_connected: self.ws_clients_connected.load(Ordering::Relaxed),
        }
    }
}

/// Serializable metrics snapshot (see `GET /api/v1/chronus/metrics`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct GatewayMetricsSnapshot {
    pub telemetry_frames_emitted: u64,
    pub telemetry_parse_errors: u64,
    pub anomaly_frames: u64,
    pub ws_messages_sent: u64,
    pub processing_latency_ms_sum: u64,
    pub processing_latency_ms_count: u64,
    pub ws_clients_connected: u64,
}

impl GatewayMetricsSnapshot {
    /// Average processing latency in ms, or `None` if no samples.
    pub fn avg_processing_latency_ms(&self) -> Option<f64> {
        if self.processing_latency_ms_count == 0 {
            None
        } else {
            Some(self.processing_latency_ms_sum as f64 / self.processing_latency_ms_count as f64)
        }
    }
}
