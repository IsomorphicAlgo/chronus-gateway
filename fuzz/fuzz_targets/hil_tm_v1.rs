//! HIL TM v1 payload decoder (CV-3) on arbitrary bytes: anything accepted re-encodes to the same
//! 24 bytes (non-finite values are allowed through; the CV-4/CV-5 checks skip them, D-022).
#![no_main]

use chronus_gateway::hil_tm::{HIL_TM_V1_PAYLOAD_LEN, decode_hil_tm_v1, encode_hil_tm_v1_payload};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(d) = decode_hil_tm_v1(data) {
        let again = encode_hil_tm_v1_payload(
            d.seq,
            d.eps_bus_voltage_v,
            d.thermal_panel_c,
            d.body_rate_deg_s,
        );
        assert_eq!(&again[..], &data[..HIL_TM_V1_PAYLOAD_LEN]);
    }
});
