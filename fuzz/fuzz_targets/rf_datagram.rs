//! `chronus.rf.v1` datagram parser and the bounded receiver-data store.
#![no_main]

use chronus_gateway::config::RfConfig;
use chronus_gateway::rf::{RfSampleV1, RfStore};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = RfSampleV1::parse(data) {
        // Whatever parses must round-trip through the encoder.
        let again = RfSampleV1::parse(&s.to_datagram()).expect("re-encoded sample parses");
        assert_eq!(again.to_datagram(), s.to_datagram());
    }
    let mut cfg = RfConfig::default();
    cfg.bind_addr = Some(([127, 0, 0, 1], 0).into());
    cfg.capacity = 4;
    let store = RfStore::new(&cfg);
    let now = chrono::DateTime::UNIX_EPOCH;
    for chunk in data.chunks(64) {
        store.ingest_datagram(chunk, now);
    }
    assert!(store.len() <= 4);
    let _ = store.lookup(now);
});
