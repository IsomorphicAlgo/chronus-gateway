//! Showcase S4 — offline ingest of curated `demo/fixtures/` CCSDS TM bytes.
//!
//! Clean fixtures must parse; robustness fixtures must fail with structured errors (never panic).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use chronus_gateway::ccsds::{CCSDS_PRIMARY_HEADER_LEN, CcsdsError, parse_telemetry};
use chronus_gateway::ingest::RawFrame;

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../demo/fixtures")
}

fn parse_hex_line(line: &str) -> Option<Vec<u8>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    if !compact.len().is_multiple_of(2) {
        panic!("odd hex length in fixture line: {line}");
    }
    let mut out = Vec::with_capacity(compact.len() / 2);
    for chunk in compact.as_bytes().chunks(2) {
        let pair = std::str::from_utf8(chunk).expect("hex must be ASCII");
        out.push(u8::from_str_radix(pair, 16).unwrap_or_else(|_| panic!("bad hex `{pair}`")));
    }
    Some(out)
}

fn load_hex_fixture(rel: &str) -> Vec<Vec<u8>> {
    let path = fixtures_root().join(rel);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.lines().filter_map(parse_hex_line).collect::<Vec<_>>()
}

fn frame_from(bytes: Vec<u8>) -> RawFrame {
    RawFrame {
        bytes: Arc::from(bytes.into_boxed_slice()),
        received_at: Utc::now(),
        source: SocketAddr::from(([127, 0, 0, 1], 7301)),
    }
}

#[test]
fn s4_iss_clean_fixture_parses() {
    let datagrams = load_hex_fixture("iss/clean.hex");
    assert_eq!(
        datagrams.len(),
        1,
        "iss/clean.hex should contain one TM line"
    );
    let tm = parse_telemetry(&frame_from(datagrams[0].clone())).expect("ISS clean TM parses");
    assert_eq!(tm.apid, 0x155);
    assert_eq!(tm.seq_count, 1);
    assert_eq!(tm.payload(), b"ISS-EDU");
}

#[test]
fn s4_amsat_clean_fixture_parses() {
    let datagrams = load_hex_fixture("amsat/clean.hex");
    assert_eq!(
        datagrams.len(),
        1,
        "amsat/clean.hex should contain one TM line"
    );
    let tm = parse_telemetry(&frame_from(datagrams[0].clone())).expect("AMSAT clean TM parses");
    assert_eq!(tm.apid, 0x73);
    assert_eq!(tm.seq_count, 42);
    assert_eq!(tm.payload(), b"AO-73EDU");
}

#[test]
fn s4_iss_robustness_fixture_fails_gracefully() {
    let datagrams = load_hex_fixture("iss/robustness.hex");
    assert_eq!(
        datagrams.len(),
        4,
        "iss/robustness.hex should contain four cases"
    );
    let results: Vec<_> = datagrams
        .into_iter()
        .map(|b| parse_telemetry(&frame_from(b)))
        .collect();
    assert!(matches!(results[0], Err(CcsdsError::Truncated { .. })));
    assert!(matches!(
        results[1],
        Err(CcsdsError::NotTelemetry { apid: 0x155 })
    ));
    assert!(matches!(results[2], Err(CcsdsError::TooShort { .. })));
    assert!(matches!(results[3], Err(CcsdsError::Truncated { .. })));
    assert!(results[2].as_ref().err().is_some_and(
        |e| matches!(e, CcsdsError::TooShort { len } if *len < CCSDS_PRIMARY_HEADER_LEN)
    ));
}

#[test]
fn s4_amsat_robustness_fixture_fails_gracefully() {
    let datagrams = load_hex_fixture("amsat/robustness.hex");
    assert_eq!(
        datagrams.len(),
        4,
        "amsat/robustness.hex should contain four cases"
    );
    let results: Vec<_> = datagrams
        .into_iter()
        .map(|b| parse_telemetry(&frame_from(b)))
        .collect();
    assert!(matches!(results[0], Err(CcsdsError::Truncated { .. })));
    assert!(matches!(
        results[1],
        Err(CcsdsError::NotTelemetry { apid: 0x73 })
    ));
    assert!(matches!(results[2], Err(CcsdsError::TooShort { .. })));
    assert!(matches!(results[3], Err(CcsdsError::Truncated { .. })));
}
