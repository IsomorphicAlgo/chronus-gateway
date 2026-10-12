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
    /// Per-frame propagator failures: the frame was emitted **without** physics fields (F-4).
    /// A rising rate with tracking configured usually means a stale TLE outside SGP4 validity.
    pub tracking_errors: AtomicU64,
    /// Frames dropped because JSON serialization failed (F-3; expected to stay at zero).
    pub serialize_errors: AtomicU64,
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
            tracking_errors: self.tracking_errors.load(Ordering::Relaxed),
            serialize_errors: self.serialize_errors.load(Ordering::Relaxed),
            ws_messages_sent: self.ws_messages_sent.load(Ordering::Relaxed),
            processing_latency_ms_sum: self.processing_latency_ms_sum.load(Ordering::Relaxed),
            processing_latency_ms_count: self.processing_latency_ms_count.load(Ordering::Relaxed),
            ws_clients_connected: self.ws_clients_connected.load(Ordering::Relaxed),
        }
    }
}

/// Serializable metrics snapshot (see `GET /api/v1/chronus/metrics`).
#[derive(Debug, Clone, serde::Serialize)]
#[non_exhaustive]
pub struct GatewayMetricsSnapshot {
    /// Frames parsed, validated and serialized for WebSocket clients.
    pub telemetry_frames_emitted: u64,
    /// Datagrams rejected as invalid CCSDS packets.
    pub telemetry_parse_errors: u64,
    /// Frames emitted with a non-zero `physics_flags`.
    pub anomaly_frames: u64,
    /// Frames emitted without physics because the propagator failed.
    pub tracking_errors: u64,
    /// JSON serialization failures.
    pub serialize_errors: u64,
    /// WebSocket messages sent across all clients.
    pub ws_messages_sent: u64,
    /// Sum of per-frame processing latency (ms).
    pub processing_latency_ms_sum: u64,
    /// Number of latency samples in the sum.
    pub processing_latency_ms_count: u64,
    /// WebSocket clients connected right now.
    pub ws_clients_connected: u64,
}

impl GatewayMetricsSnapshot {
    /// Average processing latency in ms, or `None` if no samples.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering;
    /// use chronus_gateway::metrics::GatewayMetrics;
    ///
    /// let m = GatewayMetrics::default();
    /// assert_eq!(m.snapshot().avg_processing_latency_ms(), None);
    /// m.processing_latency_ms_sum.store(9, Ordering::Relaxed);
    /// m.processing_latency_ms_count.store(3, Ordering::Relaxed);
    /// assert_eq!(m.snapshot().avg_processing_latency_ms(), Some(3.0));
    /// ```
    pub fn avg_processing_latency_ms(&self) -> Option<f64> {
        if self.processing_latency_ms_count == 0 {
            None
        } else {
            Some(self.processing_latency_ms_sum as f64 / self.processing_latency_ms_count as f64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_snapshot_is_all_zero() {
        let s = GatewayMetrics::default().snapshot();
        let v = serde_json::to_value(&s).expect("json");
        for (k, val) in v.as_object().expect("object") {
            assert_eq!(val, 0, "{k} should start at zero");
        }
    }

    #[test]
    fn snapshot_reflects_each_counter() {
        let m = GatewayMetrics::default();
        m.telemetry_frames_emitted.store(1, Ordering::Relaxed);
        m.telemetry_parse_errors.store(2, Ordering::Relaxed);
        m.anomaly_frames.store(3, Ordering::Relaxed);
        m.tracking_errors.store(4, Ordering::Relaxed);
        m.serialize_errors.store(5, Ordering::Relaxed);
        m.ws_messages_sent.store(6, Ordering::Relaxed);
        m.processing_latency_ms_sum.store(7, Ordering::Relaxed);
        m.processing_latency_ms_count.store(8, Ordering::Relaxed);
        m.ws_clients_connected.store(9, Ordering::Relaxed);
        let s = m.snapshot();
        assert_eq!(
            [
                s.telemetry_frames_emitted,
                s.telemetry_parse_errors,
                s.anomaly_frames,
                s.tracking_errors,
                s.serialize_errors,
                s.ws_messages_sent,
                s.processing_latency_ms_sum,
                s.processing_latency_ms_count,
                s.ws_clients_connected,
            ],
            [1, 2, 3, 4, 5, 6, 7, 8, 9]
        );
    }

    #[test]
    fn average_latency_is_none_without_samples() {
        assert_eq!(
            GatewayMetrics::default()
                .snapshot()
                .avg_processing_latency_ms(),
            None
        );
    }

    #[test]
    fn average_latency_divides_sum_by_count() {
        let m = GatewayMetrics::default();
        m.processing_latency_ms_sum.store(30, Ordering::Relaxed);
        m.processing_latency_ms_count.store(4, Ordering::Relaxed);
        assert_eq!(m.snapshot().avg_processing_latency_ms(), Some(7.5));
    }

    #[test]
    fn counters_are_safe_to_update_from_many_threads() {
        let m = std::sync::Arc::new(GatewayMetrics::default());
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let m = std::sync::Arc::clone(&m);
                std::thread::spawn(move || {
                    for _ in 0..1000 {
                        m.ws_messages_sent.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread");
        }
        assert_eq!(m.snapshot().ws_messages_sent, 8000);
    }
}
