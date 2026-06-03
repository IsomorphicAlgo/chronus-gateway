//! Milestone 1 integration tests for the asynchronous UDP ingestion loop.
//!
//! All tests run over loopback UDP (no hardware), bind to an ephemeral port, and use a oneshot
//! channel as the shutdown signal so each test stops the loop deterministically.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use chronus_gateway::config::IngestConfig;
use chronus_gateway::ingest::{self, IngestStats, RawFrame};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

/// Binds the ingestion socket on loopback:0 and returns it plus the resolved local address.
async fn bind_loopback(max_datagram_size: usize) -> (UdpSocket, IngestConfig, SocketAddr) {
    let config = IngestConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        channel_capacity: 1024,
        max_datagram_size,
        ..Default::default()
    };
    let socket = ingest::bind(&config).await.expect("bind loopback socket");
    let local = socket.local_addr().expect("local addr");
    (socket, config, local)
}

/// A UDP client bound to an ephemeral loopback port, ready to send to `server`.
async fn client_to(server: SocketAddr) -> UdpSocket {
    let client = UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind client");
    client.connect(server).await.expect("connect client");
    client
}

async fn recv_timeout(
    rx: &mut broadcast::Receiver<RawFrame>,
) -> Result<RawFrame, broadcast::error::RecvError> {
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("timed out waiting for a frame")
}

/// Spawns the ingestion loop; returns its join handle and the shutdown trigger.
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
async fn receives_all_datagrams_in_order() {
    let (socket, config, local) = bind_loopback(2048).await;
    let (tx, mut rx) = broadcast::channel(1024);
    let stats = Arc::new(IngestStats::default());
    let (handle, sd_tx) = spawn_run(socket, tx, config, Arc::clone(&stats));

    let client = client_to(local).await;
    const N: u8 = 16;
    for i in 0..N {
        client.send(&[i]).await.expect("send datagram");
    }

    for i in 0..N {
        let frame = recv_timeout(&mut rx).await.expect("frame");
        assert_eq!(frame.bytes.as_ref(), &[i], "frame {i} arrived out of order");
        assert_eq!(frame.source.ip().to_string(), "127.0.0.1");
    }

    assert_eq!(stats.frames_received.load(Ordering::Relaxed), N as u64);

    sd_tx.send(()).ok();
    handle.await.expect("join").expect("clean shutdown");
}

#[tokio::test]
async fn shutdown_stops_loop_promptly() {
    let (socket, config, _local) = bind_loopback(2048).await;
    let (tx, _rx) = broadcast::channel(16);
    let stats = Arc::new(IngestStats::default());
    let (handle, sd_tx) = spawn_run(socket, tx, config, stats);

    // Trigger shutdown and confirm the loop returns Ok quickly.
    sd_tx.send(()).expect("send shutdown");
    let result = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("loop did not stop promptly")
        .expect("join");
    assert!(result.is_ok(), "ingest::run should return Ok on shutdown");
}

#[tokio::test]
async fn oversized_datagram_does_not_break_loop() {
    // Tiny ceiling so a 64-byte datagram is "oversized".
    let (socket, config, local) = bind_loopback(8).await;
    let (tx, mut rx) = broadcast::channel(1024);
    let stats = Arc::new(IngestStats::default());
    let (handle, sd_tx) = spawn_run(socket, tx, config, Arc::clone(&stats));

    let client = client_to(local).await;
    client.send(&[0xAAu8; 64]).await.expect("send oversized");
    // A valid, in-spec datagram must still come through after the oversized one.
    let valid = [0x42u8; 4];
    client.send(&valid).await.expect("send valid");

    // Drain until we observe the valid payload (on Unix the oversized one is truncated and may
    // appear first; on Windows it is dropped via WSAEMSGSIZE). Either way the loop survives.
    let mut saw_valid = false;
    for _ in 0..4 {
        let frame = recv_timeout(&mut rx).await.expect("frame");
        if frame.bytes.as_ref() == valid {
            saw_valid = true;
            break;
        }
    }
    assert!(saw_valid, "loop must keep delivering valid frames after an oversized datagram");

    sd_tx.send(()).ok();
    handle.await.expect("join").expect("clean shutdown");
}

#[tokio::test]
async fn lagging_subscriber_never_blocks_socket() {
    // Capacity 2 with many sends forces the broadcast channel to drop for a slow subscriber.
    let (socket, config, local) = bind_loopback(2048).await;
    let (tx, mut slow_rx) = broadcast::channel(2);
    let stats = Arc::new(IngestStats::default());
    let (handle, sd_tx) = spawn_run(socket, tx, config, Arc::clone(&stats));

    let client = client_to(local).await;
    const N: u64 = 32;
    for i in 0..N as u8 {
        client.send(&[i]).await.expect("send");
    }

    // The socket loop must receive all datagrams regardless of the stalled subscriber.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while stats.frames_received.load(Ordering::Relaxed) < N {
        assert!(tokio::time::Instant::now() < deadline, "socket loop stalled on slow subscriber");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(stats.frames_received.load(Ordering::Relaxed), N);

    // The slow subscriber observes a lag rather than a block or a clean stream.
    match slow_rx.recv().await {
        Err(broadcast::error::RecvError::Lagged(skipped)) => {
            assert!(skipped > 0, "expected a positive lag count");
        }
        other => panic!("expected Lagged, got {other:?}"),
    }

    sd_tx.send(()).ok();
    handle.await.expect("join").expect("clean shutdown");
}
