//! Shared application state for ingestion, HTTP/WebSocket distribution, and metrics.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::broadcast;

use crate::config::StationConfig;
use crate::ingest::{IngestStats, RawFrame};
use crate::metrics::GatewayMetrics;
use crate::propagator::TrackingProvider;
use crate::validate::ValidationContext;

/// Process-wide handles shared between the UDP ingest task and the Axum server.
///
/// `#[non_exhaustive]`: construct it with [`SharedGateway::new`] so new shared handles can be
/// added in 1.x.
#[non_exhaustive]
pub struct SharedGateway {
    /// Fan-out of raw datagrams to WebSocket subscribers (and any other consumers).
    pub frame_tx: broadcast::Sender<RawFrame>,
    /// UDP ingest counters.
    pub ingest_stats: Arc<IngestStats>,
    /// Telemetry / WebSocket counters (Milestone 6).
    pub gateway_metrics: Arc<GatewayMetrics>,
    /// Station + validation thresholds (immutable at runtime for the MVP).
    pub station: Arc<StationConfig>,
    /// Physics-check tunables, built once from [`Self::station`] in [`SharedGateway::new`].
    pub validation: ValidationContext,
    /// Optional SGP4-backed tracking (throttled).
    pub tracking: Option<Arc<TrackingProvider>>,
    /// UDP telemetry address, when known (used in plain-English status messages).
    pub udp_addr: Option<SocketAddr>,
    /// HTTP/WebSocket address, when known (used in plain-English status messages).
    pub http_addr: Option<SocketAddr>,
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
        let validation = ValidationContext::from_station(&station);
        Self {
            frame_tx,
            ingest_stats,
            gateway_metrics,
            station,
            validation,
            tracking,
            udp_addr: None,
            http_addr: None,
        }
    }

    /// Records the actual listening addresses so `GET /api/v1/chronus/status` can tell operators
    /// exactly where to send telemetry and connect dashboards.
    #[must_use]
    pub fn with_listen_addrs(mut self, udp: SocketAddr, http: SocketAddr) -> Self {
        self.udp_addr = Some(udp);
        self.http_addr = Some(http);
        self
    }

    /// Plain-English health assessment of the live counters (see [`crate::diagnose`]).
    pub fn health_report(&self) -> crate::diagnose::HealthReport {
        let ingest = self.ingest_stats.snapshot_struct();
        let gateway = self.gateway_metrics.snapshot();
        crate::diagnose::assess_health(&crate::diagnose::HealthInputs {
            ingest: &ingest,
            gateway: &gateway,
            tracking_enabled: self.tracking.is_some(),
            udp_addr: self.udp_addr,
            http_addr: self.http_addr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnose::OverallStatus;
    use std::sync::atomic::Ordering;

    fn gateway() -> SharedGateway {
        let (tx, _rx) = broadcast::channel(4);
        SharedGateway::new(
            tx,
            Arc::new(IngestStats::default()),
            Arc::new(GatewayMetrics::default()),
            Arc::new(StationConfig::default()),
            None,
        )
    }

    #[test]
    fn new_has_no_listen_addrs() {
        let g = gateway();
        assert!(g.udp_addr.is_none() && g.http_addr.is_none());
        assert!(g.tracking.is_none());
    }

    #[test]
    fn with_listen_addrs_records_both() {
        let udp: SocketAddr = "127.0.0.1:7301".parse().unwrap();
        let http: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let g = gateway().with_listen_addrs(udp, http);
        assert_eq!(g.udp_addr, Some(udp));
        assert_eq!(g.http_addr, Some(http));
    }

    #[test]
    fn health_report_reflects_live_counters() {
        let g = gateway();
        // No tracking configured -> physics-off warning even before data arrives.
        let r = g.health_report();
        assert_eq!(r.status, OverallStatus::Attention);
        assert!(r.findings.iter().any(|d| d.code == "CG-501"));
        assert!(r.findings.iter().any(|d| d.code == "CG-506"));

        g.ingest_stats.frames_received.store(3, Ordering::Relaxed);
        let r = g.health_report();
        assert!(!r.findings.iter().any(|d| d.code == "CG-501"));
    }

    #[test]
    fn subscribers_receive_frames_sent_on_frame_tx() {
        let g = gateway();
        let mut rx = g.frame_tx.subscribe();
        let frame = RawFrame {
            bytes: Arc::from(&[1u8, 2, 3][..]),
            received_at: chrono::Utc::now(),
            source: "127.0.0.1:9".parse().unwrap(),
        };
        g.frame_tx.send(frame).expect("one live subscriber");
        let got = rx.try_recv().expect("frame delivered");
        assert_eq!(&*got.bytes, &[1, 2, 3]);
    }
}
