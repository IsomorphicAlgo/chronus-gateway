//! Tranche 2: the NeXosim HIL sim drives **physics flag bits through real UDP sockets** into the
//! gateway's ingest, `chronus.rf.v1` listener and processing path.
//!
//! Each case runs the sim with an onboard time code (so physics is evaluated at the simulated
//! mission time, in lab mode) and a paced RF sidecar, then checks that exactly the scripted
//! frames carry exactly the expected bit. The real-binary counterpart is
//! `crates/gateway/tests/live_rf_e2e.rs`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use chronus_gateway::TimeFormat;
use chronus_gateway::config::{IngestConfig, StationConfig};
use chronus_gateway::http::process_frame;
use chronus_gateway::ingest::{self, IngestStats};
use chronus_gateway::metrics::GatewayMetrics;
use chronus_gateway::propagator::{TrackingProvider, propagator_from_station};
use chronus_gateway::rf::run_listener;
use chronus_gateway::state::SharedGateway;
use chronus_hil_sim::rf::{RfFaultKind, RfModel, RfScriptedAnomaly};
use chronus_hil_sim::{
    HilRunOptions, HilScriptedAnomaly, HilScriptedAnomalyKind, default_hil_epoch_utc,
    run_nexosim_hil,
};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, oneshot};

const N: u32 = 14;

/// Runs the sim against an in-process gateway; returns `(seq, physics_flags)` per frame.
async fn run_case(
    epoch: DateTime<Utc>,
    script: Option<HilScriptedAnomaly>,
    rf_script: Option<RfScriptedAnomaly>,
) -> Vec<(u16, u64)> {
    let mut station = StationConfig::default();
    station.rf.bind_addr = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
    station.rf.window_ms = 100;
    station.time.format = TimeFormat::Cds;
    station.time.max_skew_s = 0.0; // lab mode: simulated mission time is not "now"

    let tracking = Arc::new(TrackingProvider::new(
        propagator_from_station(&station).unwrap(),
        0,
    ));
    let (tx, mut rx) = broadcast::channel(256);
    let state = Arc::new(SharedGateway::new(
        tx.clone(),
        Arc::new(IngestStats::default()),
        Arc::new(GatewayMetrics::default()),
        Arc::new(station),
        Some(tracking),
    ));

    let ingest_cfg = IngestConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        ..Default::default()
    };
    let udp = ingest::bind(&ingest_cfg).await.unwrap();
    let udp_addr = udp.local_addr().unwrap();
    let rf_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let rf_addr = rf_sock.local_addr().unwrap();

    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let (stop2_tx, stop2_rx) = oneshot::channel::<()>();
    let stats = Arc::clone(&state.ingest_stats);
    let ingest_task = tokio::spawn(ingest::run(udp, tx, ingest_cfg, stats, async move {
        let _ = stop_rx.await;
    }));
    let rf_task = tokio::spawn(run_listener(rf_sock, Arc::clone(&state.rf), async move {
        let _ = stop2_rx.await;
    }));

    let apid = 0x7B0;
    let mut opts = HilRunOptions::new(udp_addr, N, apid)
        .with_script(script)
        .with_time(TimeFormat::Cds, epoch)
        .with_rf(rf_addr, rf_script);
    // Short frames keep the test quick; 20 ms pacing still separates neighbouring RF samples.
    opts.frame_interval_ms = 20;
    tokio::task::spawn_blocking(move || run_nexosim_hil(opts))
        .await
        .unwrap()
        .unwrap();

    let mut out = Vec::new();
    for _ in 0..N {
        let frame = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("frame")
            .expect("open");
        let json = process_frame(&state, &frame).expect("valid frame");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        out.push((
            v["seq_count"].as_u64().unwrap() as u16,
            v["physics_flags"].as_u64().unwrap(),
        ));
    }
    let _ = stop_tx.send(());
    let _ = stop2_tx.send(());
    ingest_task.await.unwrap().unwrap();
    rf_task.await.unwrap().unwrap();
    out
}

fn expect_window(out: &[(u16, u64)], start: u16, len: u16, bit: u64) {
    assert_eq!(out.len(), N as usize);
    for (seq, flags) in out {
        let want = if (start..start + len).contains(seq) {
            bit
        } else {
            0
        };
        assert_eq!(
            *flags, want,
            "frame {seq}: flags {flags:#04x}, want {want:#04x}"
        );
    }
}

fn visible_epoch() -> DateTime<Utc> {
    RfModel::from_station(&StationConfig::default())
        .unwrap()
        .next_visible(default_hil_epoch_utc(), chrono::Duration::days(3), 15.0)
        .expect("a pass within three days")
}

#[tokio::test]
async fn nominal_hil_and_rf_raise_nothing() {
    let out = run_case(visible_epoch(), None, None).await;
    expect_window(&out, 0, 0, 0);
}

#[tokio::test]
async fn rf_faults_raise_bits_0_2_3_on_exactly_the_scripted_frames() {
    for (kind, bit) in [
        (RfFaultKind::Doppler, 0x01),
        (RfFaultKind::LinkBudget, 0x04),
        (RfFaultKind::Pointing, 0x08),
    ] {
        let rf = RfScriptedAnomaly {
            kind,
            start_frame: 4,
            duration_frames: 3,
        };
        let out = run_case(visible_epoch(), None, Some(rf)).await;
        expect_window(&out, 4, 3, bit);
    }
}

#[tokio::test]
async fn subsystem_faults_raise_bits_4_5_6_on_exactly_the_scripted_frames() {
    for (kind, bit) in [
        (HilScriptedAnomalyKind::EpsVoltage, 0x10),
        (HilScriptedAnomalyKind::Thermal, 0x20),
        (HilScriptedAnomalyKind::BodyRate, 0x40),
    ] {
        let script = HilScriptedAnomaly {
            kind,
            start_frame: 6,
            duration_frames: 2,
        };
        let out = run_case(visible_epoch(), Some(script), None).await;
        expect_window(&out, 6, 2, bit);
    }
}

#[tokio::test]
async fn a_run_below_the_horizon_raises_bit_1_on_every_frame() {
    let model = RfModel::from_station(&StationConfig::default()).unwrap();
    let mut t = default_hil_epoch_utc();
    while model.elevation_deg(t).is_some_and(|e| e > -20.0) {
        t += chrono::Duration::seconds(30);
    }
    let out = run_case(t, None, None).await;
    assert!(out.iter().all(|(_, f)| *f == 0x02), "{out:?}");
}

#[tokio::test]
async fn rf_and_subsystem_faults_combine_in_the_same_frames() {
    let script = HilScriptedAnomaly {
        kind: HilScriptedAnomalyKind::BodyRate,
        start_frame: 5,
        duration_frames: 4,
    };
    let rf = RfScriptedAnomaly {
        kind: RfFaultKind::Doppler,
        start_frame: 7,
        duration_frames: 4,
    };
    let out = run_case(visible_epoch(), Some(script), Some(rf)).await;
    for (seq, flags) in &out {
        let mut want = 0;
        if (5..9).contains(seq) {
            want |= 0x40;
        }
        if (7..11).contains(seq) {
            want |= 0x01;
        }
        assert_eq!(*flags, want, "frame {seq}");
    }
}
