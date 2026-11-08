//! The whole per-packet path: parse, spacecraft time, receiver data, HIL checks, JSON, history,
//! exactly as the gateway's pipeline runs it (physics off: propagation is not untrusted input).
#![no_main]

use std::sync::{Arc, OnceLock};

use chronus_gateway::config::StationConfig;
use chronus_gateway::ingest::IngestStats;
use chronus_gateway::metrics::GatewayMetrics;
use chronus_gateway::state::SharedGateway;
use chronus_gateway::time::TimeFormat;
use chronus_gateway::ingest::RawFrame;
use chronus_gateway::pipeline::handle_datagram;
use libfuzzer_sys::fuzz_target;

fn gateway() -> &'static SharedGateway {
    static G: OnceLock<SharedGateway> = OnceLock::new();
    G.get_or_init(|| {
        let mut station = StationConfig::default();
        station.time.format = TimeFormat::Cds;
        station.history.capacity = 64;
        let (tx, _rx) = tokio::sync::broadcast::channel(4);
        SharedGateway::new(
            tx,
            Arc::new(IngestStats::default()),
            Arc::new(GatewayMetrics::default()),
            Arc::new(station),
            None,
        )
    })
}

fuzz_target!(|data: &[u8]| {
    let g = gateway();
    let raw = RawFrame::new(data, chrono::Utc::now(), ([127, 0, 0, 1], 9).into());
    handle_datagram(g, None, &raw);
    assert!(g.history.len() <= 64);
});
