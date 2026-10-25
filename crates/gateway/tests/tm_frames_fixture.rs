//! Tranche 3 cross-implementation check (Methodology D-048): the lab CADU stream in
//! `demo/replay/fixtures/tm_frames_lab.hex` was produced by an independent Python encoder
//! (`gen_tm_frames.py`). The Rust deframer must recover exactly the packets that encoder says it
//! put in, in delivery order, and account for the corrupted frame, idle frames and junk bytes.

use std::path::PathBuf;
use std::sync::Arc;

use chronus_gateway::config::{FramingConfig, FramingMode, StationConfig};
use chronus_gateway::frames::{FramingStats, TmDeframer};
use chronus_gateway::history::HistoryQuery;
use chronus_gateway::ingest::{IngestStats, RawFrame};
use chronus_gateway::metrics::GatewayMetrics;
use chronus_gateway::propagator::{TrackingProvider, propagator_from_station};
use chronus_gateway::state::SharedGateway;
use chronus_gateway::time::TimeFormat;
use tokio::sync::broadcast;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../demo/replay/fixtures")
}

fn datagrams() -> Vec<Vec<u8>> {
    let text = std::fs::read_to_string(fixture_dir().join("tm_frames_lab.hex")).expect("fixture");
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            (0..l.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&l[i..i + 2], 16).expect("hex"))
                .collect()
        })
        .collect()
}

fn expected() -> serde_json::Value {
    let text =
        std::fs::read_to_string(fixture_dir().join("tm_frames_lab.expected.json")).expect("json");
    serde_json::from_str(&text).expect("expected.json parses")
}

fn framing() -> FramingConfig {
    let mut f = FramingConfig::default();
    f.mode = FramingMode::TmFrames;
    f.frame_length = Some(223);
    f.asm = true;
    f.derandomize = true;
    f
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

#[test]
fn rust_deframer_recovers_exactly_what_the_python_encoder_sent() {
    let exp = expected();
    let stats = Arc::new(FramingStats::default());
    let mut d = TmDeframer::new(&framing(), Arc::clone(&stats));
    let mut got: Vec<(u8, String)> = Vec::new();
    let dgs = datagrams();
    assert_eq!(dgs.len() as u64, exp["datagrams"].as_u64().unwrap());
    for dg in &dgs {
        d.push(dg, |p| got.push((p.vcid, hex(p.bytes))));
    }
    let want: Vec<(u8, String)> = exp["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["vcid"].as_u64().unwrap() as u8,
                p["hex"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(got.len(), want.len(), "packet count");
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(g, w, "packet {i} differs");
    }

    let s = stats.snapshot(FramingMode::TmFrames);
    let frames = exp["frames"].as_u64().unwrap();
    assert_eq!(
        s.frames_crc_error,
        exp["frames_crc_error"].as_u64().unwrap()
    );
    assert_eq!(s.frames_idle, exp["frames_idle"].as_u64().unwrap());
    assert_eq!(s.frames_ok + s.frames_crc_error, frames, "{s:?}");
    assert_eq!(s.frames_invalid, 0);
    assert_eq!(s.vc_frame_gaps, 1, "the dropped frame shows as one VC1 gap");
    assert_eq!(s.packets_dropped, 1, "the science packet crossing it");
    // Skipped while hunting for sync: the 5 junk bytes before frame 3, plus the rejected frame
    // (the search restarts one byte after its marker: 3 marker bytes + 223 frame bytes).
    assert_eq!(s.bytes_discarded, 5 + 3 + 223);
    assert!(s.packets_idle >= 1);
}

#[test]
fn fixture_decodes_end_to_end_with_onboard_time_and_physics() {
    let mut station = StationConfig::default();
    station.framing = framing();
    station.time.format = TimeFormat::Cds;
    station.time.max_skew_s = 0.0; // a recording: trust onboard time
    let tracking = Arc::new(TrackingProvider::new(
        propagator_from_station(&station).expect("default ISS TLE"),
        0,
    ));
    let (tx, _rx) = broadcast::channel(16);
    let g = SharedGateway::new(
        tx,
        Arc::new(IngestStats::default()),
        Arc::new(GatewayMetrics::default()),
        Arc::new(station),
        Some(tracking),
    );
    let mut d = TmDeframer::new(&g.station.framing, Arc::clone(&g.framing));
    for dg in datagrams() {
        let raw = RawFrame {
            bytes: Arc::from(dg.as_slice()),
            received_at: chrono::Utc::now(),
            source: "127.0.0.1:9".parse().unwrap(),
        };
        chronus_gateway::pipeline::handle_datagram(&g, Some(&mut d), &raw);
    }
    let page = g.history.query(&HistoryQuery::new());
    let n = expected()["packets"].as_array().unwrap().len();
    assert_eq!(page.entries.len(), n);
    for e in &page.entries {
        let v: serde_json::Value = serde_json::from_str(&e.json).unwrap();
        assert_eq!(v["physics_flags_v2"], 0, "{v}");
        assert_eq!(v["physics_flags"], 0, "nominal pass, no receiver data: {v}");
        let t = v["onboard_time"].as_str().expect("CDS time decoded");
        assert!(t.starts_with("2020-07-13T05:0"), "{t}");
        let el = v["elevation_deg"].as_f64().unwrap();
        assert!(
            (20.0..45.0).contains(&el),
            "ISS is ~31 deg up during this pass: {el}"
        );
        assert!(v["vcid"] == 0 || v["vcid"] == 1, "{v}");
    }
    let first: serde_json::Value = serde_json::from_str(&page.entries[0].json).unwrap();
    assert_eq!(first["onboard_time"], "2020-07-13T05:08:30Z");
    assert_eq!(first["apid"], 0x120);
}
