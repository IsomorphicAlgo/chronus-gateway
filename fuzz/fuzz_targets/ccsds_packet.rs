//! CCSDS Space Packet parser + spacecraft time + payload views on arbitrary datagrams.
#![no_main]

use std::sync::Arc;

use chronus_gateway::config::TimeConfig;
use chronus_gateway::time::{TimeFormat, resolve_frame_time};
use chronus_gateway::ccsds::parse_telemetry;
use chronus_gateway::ingest::RawFrame;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let raw = RawFrame::new(data, chrono::DateTime::UNIX_EPOCH, ([127, 0, 0, 1], 9).into());
    let Ok(mut frame) = parse_telemetry(&raw) else {
        return;
    };
    // Invariants: the views never exceed the datagram, the APID fits 11 bits.
    assert!(frame.payload().len() <= data.len());
    assert!(frame.apid <= 0x7FF);
    for format in [TimeFormat::Cds, TimeFormat::Cuc] {
        let mut cfg = TimeConfig::default();
        cfg.format = format;
        let _ = resolve_frame_time(&cfg, &mut frame);
        assert!(frame.user_data().len() <= frame.payload().len());
    }
});
