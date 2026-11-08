//! Gateway TOML loader + validation + startup checks on arbitrary text.
#![no_main]

use chronus_gateway::config::load_gateway_from_str;
use chronus_gateway::diagnose::{Explain, config_warnings};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    match load_gateway_from_str(text) {
        Ok((ingest, station)) => {
            let _ = config_warnings(&ingest, &station);
        }
        // Every error must produce an explanation without panicking.
        Err(e) => {
            let d = e.explain();
            assert!(d.code.starts_with("CG-"));
        }
    }
});
