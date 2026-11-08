//! TM Transfer Frame deframer under every combination of link settings, fed arbitrarily split
//! datagrams. Memory must stay bounded and every packet emitted must be a whole CCSDS packet.
#![no_main]

use std::sync::Arc;

use arbitrary::Arbitrary;
use chronus_gateway::config::{FramingConfig, FramingMode};
use chronus_gateway::frames::{FramingStats, TmDeframer};
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Arbitrary)]
struct Input {
    frame_length: u16,
    unset_length: bool,
    asm: bool,
    derandomize: bool,
    fecf: bool,
    scid: Option<u16>,
    vcs: Option<u8>,
    datagrams: Vec<Vec<u8>>,
}

fuzz_target!(|input: Input| {
    let mut cfg = FramingConfig::default();
    cfg.mode = FramingMode::TmFrames;
    cfg.frame_length = (!input.unset_length || input.asm)
        .then_some(usize::from(input.frame_length % 2041) + 8);
    cfg.asm = input.asm;
    cfg.derandomize = input.derandomize;
    cfg.fecf = input.fecf;
    cfg.spacecraft_id = input.scid.map(|s| s & 0x3FF);
    cfg.virtual_channels = input.vcs.map(|bits| (0..8).filter(|v| bits & (1 << v) != 0).collect());
    if cfg.virtual_channels.as_ref().is_some_and(Vec::is_empty) {
        cfg.virtual_channels = None;
    }
    let mut d = TmDeframer::new(&cfg, Arc::new(FramingStats::default()));
    for dg in input.datagrams.iter().take(64) {
        d.push(dg, |p| {
            assert!(p.vcid < 8);
            assert!(p.bytes.len() >= 7);
            let declared = usize::from(u16::from_be_bytes([p.bytes[4], p.bytes[5]])) + 7;
            assert_eq!(declared, p.bytes.len(), "emitted packets are complete");
        });
    }
});
