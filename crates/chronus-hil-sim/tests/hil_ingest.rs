//! Milestone 7: NeXosim drives the real gateway UDP ingest path (smoke + light soak).

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use chronus_gateway::ccsds::{self, CCSDS_PRIMARY_HEADER_LEN};
use chronus_gateway::config::{IngestConfig, StationConfig};
use chronus_gateway::hil_tm::{decode_hil_tm_v1, HIL_TM_V1_PAYLOAD_LEN};
use chronus_gateway::ingest::{self, IngestStats, RawFrame};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

use chronus_hil_sim::{
    HilScriptedAnomaly, HilScriptedAnomalyKind, run_nexosim_udp_hil_with_script,
};

async fn bind_loopback(max_datagram_size: usize) -> (UdpSocket, IngestConfig, SocketAddr) {
    let config = IngestConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        channel_capacity: 4096,
        max_datagram_size,
        ..Default::default()
    };
    let socket = ingest::bind(&config).await.expect("bind loopback");
    let local = socket.local_addr().expect("local");
    (socket, config, local)
}

fn spawn_run(
    socket: UdpSocket,
    tx: broadcast::Sender<RawFrame>,
    config: IngestConfig,
    stats: Arc<IngestStats>,
) -> (JoinHandle<std::io::Result<()>>, oneshot::Sender<()>) {
    let (sd_tx, sd_rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        ingest::run(socket, tx, config, stats, async move {
            let _ = sd_rx.await;
        })
        .await
    });
    (handle, sd_tx)
}

#[tokio::test]
async fn nexosim_smoke_reaches_ingest_and_parse() {
    let (socket, config, local) = bind_loopback(2048).await;
    let (tx, mut rx) = broadcast::channel(4096);
    let stats = Arc::new(IngestStats::default());
    let (ingest_handle, sd_tx) = spawn_run(socket, tx, config, Arc::clone(&stats));

    const N: u32 = 24;
    let apid = 0x7B0u16;
    let hil =
        tokio::task::spawn_blocking(move || chronus_hil_sim::run_nexosim_udp_hil(local, N, apid));
    hil.await.expect("hil join").expect("hil run");

    let mut parsed = 0u32;
    for _ in 0..(N as usize + 8) {
        match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Ok(frame)) => {
                if let Ok(tm) = ccsds::parse_telemetry(&frame) {
                    let station = StationConfig::default();
                    if station.apid_allows_hil_tm_v1(tm.apid) {
                        let _ = decode_hil_tm_v1(tm.payload()).expect("HIL v1 payload");
                    }
                    parsed += 1;
                    if parsed >= N {
                        break;
                    }
                }
            }
            Ok(Err(_)) => break,
            Err(_) => panic!("timed out waiting for HIL frames"),
        }
    }

    assert_eq!(parsed, N, "expected every HIL datagram to parse as TM");
    assert_eq!(stats.frames_received.load(Ordering::Relaxed), N as u64);
    assert_eq!(stats.recv_errors.load(Ordering::Relaxed), 0);

    sd_tx.send(()).ok();
    ingest_handle
        .await
        .expect("ingest join")
        .expect("ingest ok");
}

#[tokio::test]
async fn nexosim_soak_bounded_recv_errors() {
    let (socket, config, local) = bind_loopback(65_542).await;
    let (tx, mut rx) = broadcast::channel(8192);
    let stats = Arc::new(IngestStats::default());
    let (ingest_handle, sd_tx) = spawn_run(socket, tx, config, Arc::clone(&stats));

    const N: u32 = 400;
    let apid = 0x7B1u16;
    let hil =
        tokio::task::spawn_blocking(move || chronus_hil_sim::run_nexosim_udp_hil(local, N, apid));
    hil.await.expect("hil join").expect("hil run");

    let mut parsed = 0u32;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while parsed < N && tokio::time::Instant::now() < deadline {
        let frame = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("chunk wait")
            .expect("recv");
        let tm = ccsds::parse_telemetry(&frame).expect("valid TM");
        assert_eq!(tm.apid, apid);
        assert_eq!(tm.payload_len(), HIL_TM_V1_PAYLOAD_LEN);
        assert_eq!(frame.bytes.len(), CCSDS_PRIMARY_HEADER_LEN + HIL_TM_V1_PAYLOAD_LEN);
        let decoded = decode_hil_tm_v1(tm.payload()).expect("v1 decode");
        assert_eq!(decoded.seq, parsed);
        parsed += 1;
    }

    assert_eq!(parsed, N);
    assert_eq!(stats.frames_received.load(Ordering::Relaxed), N as u64);
    assert_eq!(stats.recv_errors.load(Ordering::Relaxed), 0);

    sd_tx.send(()).ok();
    ingest_handle.await.expect("join").expect("ingest");
}

#[tokio::test]
async fn nexosim_scripted_body_rate_window_on_wire() {
    let (socket, config, local) = bind_loopback(2048).await;
    let (tx, mut rx) = broadcast::channel(4096);
    let stats = Arc::new(IngestStats::default());
    let (ingest_handle, sd_tx) = spawn_run(socket, tx, config, Arc::clone(&stats));

    const N: u32 = 16;
    const START: u32 = 5;
    const DUR: u32 = 3;
    let apid = 0x7B2u16;
    let script = HilScriptedAnomaly {
        kind: HilScriptedAnomalyKind::BodyRate,
        start_frame: START,
        duration_frames: DUR,
    };
    let hil = tokio::task::spawn_blocking(move || {
        run_nexosim_udp_hil_with_script(local, N, apid, Some(script))
    });
    hil.await.expect("hil join").expect("hil run");

    let mut by_seq: Vec<Option<chronus_gateway::hil_tm::DecodedHilTmV1>> = vec![None; N as usize];
    for _ in 0..N {
        let frame = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("recv wait")
            .expect("frame");
        let tm = ccsds::parse_telemetry(&frame).expect("TM");
        let decoded = decode_hil_tm_v1(tm.payload()).expect("HIL v1");
        let s = decoded.seq as usize;
        assert!(s < by_seq.len(), "unexpected seq {}", decoded.seq);
        by_seq[s] = Some(decoded);
    }

    for i in 0..N {
        let d = by_seq[i as usize].expect("every seq received");
        assert_eq!(d.seq, i);
        if (START..START + DUR).contains(&i) {
            assert_eq!(d.body_rate_deg_s, 99.0, "seq {i} should be in scripted window");
        } else {
            assert!(
                (d.body_rate_deg_s - 0.001 * i as f32).abs() < 1e-4,
                "seq {i} nominal ramp"
            );
        }
    }

    assert_eq!(stats.frames_received.load(Ordering::Relaxed), N as u64);
    sd_tx.send(()).ok();
    ingest_handle.await.expect("join").expect("ingest");
}
