//! The probe survives the gateway's real processing path: a probe packet, processed exactly as
//! a UDP datagram would be, comes out as an `openmct.realtime.v1` message the soak client decodes.

use std::sync::Arc;

use chronus_evidence::soak::{ProbeFrame, probe_packet};
use chronus_gateway::config::StationConfig;
use chronus_gateway::http::process_frame;
use chronus_gateway::ingest::{IngestStats, RawFrame};
use chronus_gateway::metrics::GatewayMetrics;
use chronus_gateway::state::SharedGateway;

#[test]
fn gateway_output_carries_the_probe() {
    let (tx, _rx) = tokio::sync::broadcast::channel(4);
    let g = SharedGateway::new(
        tx,
        Arc::new(IngestStats::default()),
        Arc::new(GatewayMetrics::default()),
        Arc::new(StationConfig::default()),
        None,
    );
    let frame = RawFrame::new(
        probe_packet(0x0AB, 123_456, 42),
        chrono::Utc::now(),
        "127.0.0.1:5000".parse().unwrap(),
    );
    let json = process_frame(&g, &frame).expect("probe packets are valid TM");
    let p = ProbeFrame::decode(&json).expect("probe recovered");
    assert_eq!((p.seq, p.sent_ns), (123_456, 42));
}
