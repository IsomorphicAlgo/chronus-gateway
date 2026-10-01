//! Asynchronous UDP ingestion loop (Milestone 1).
//!
//! Binds a UDP socket and streams each received datagram, as a [`RawFrame`], onto a
//! [`tokio::sync::broadcast`] channel that downstream stages (CCSDS parsing, validation,
//! distribution) subscribe to. The loop is designed for a high-rate downlink:
//!
//! - **Bounded memory.** The receive buffer is fixed at [`IngestConfig::max_datagram_size`]; the
//!   loop never allocates based on attacker-controlled length. Each forwarded frame is an
//!   `Arc<[u8]>` so the broadcast clone handed to every subscriber is a cheap refcount bump, not
//!   a payload copy.
//! - **Lossy backpressure.** The broadcast channel drops the oldest frames when full; a slow
//!   subscriber observes [`tokio::sync::broadcast::error::RecvError::Lagged`] instead of blocking
//!   the socket. The receive loop is never throttled by consumers.
//! - **Graceful shutdown.** The loop runs until the supplied `shutdown` future resolves.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use tokio::net::UdpSocket;
use tokio::sync::broadcast;

use crate::config::IngestConfig;

/// `WSAEMSGSIZE` — Windows returns this from `recv_from` when a datagram is larger than the
/// supplied buffer. On Unix the kernel silently truncates instead, so this code path is
/// Windows-specific but harmless elsewhere.
const WSAEMSGSIZE: i32 = 10040;

/// A raw, unparsed datagram captured from the downlink socket.
///
/// `bytes` is reference-counted so cloning a frame for each broadcast subscriber is cheap.
#[derive(Debug, Clone)]
pub struct RawFrame {
    /// The datagram payload (`<= max_datagram_size` bytes).
    pub bytes: Arc<[u8]>,
    /// Capture timestamp (UTC), taken when the datagram was received.
    pub received_at: DateTime<Utc>,
    /// Remote address the datagram was received from.
    pub source: SocketAddr,
}

/// Cumulative ingestion counters, shareable across tasks for observability and tests.
#[derive(Debug, Default)]
pub struct IngestStats {
    /// Valid datagrams received and forwarded.
    pub frames_received: AtomicU64,
    /// Total payload bytes received (sum of forwarded frame lengths).
    pub bytes_received: AtomicU64,
    /// Datagrams dropped because they exceeded `max_datagram_size` (Windows `WSAEMSGSIZE`).
    pub oversized_dropped: AtomicU64,
    /// Non-fatal `recv_from` errors encountered.
    pub recv_errors: AtomicU64,
}

impl IngestStats {
    /// Convenience snapshot of `(frames, bytes, oversized, errors)` with `Relaxed` loads.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering;
    /// use chronus_gateway::ingest::IngestStats;
    ///
    /// let stats = IngestStats::default();
    /// stats.frames_received.fetch_add(2, Ordering::Relaxed);
    /// stats.bytes_received.fetch_add(64, Ordering::Relaxed);
    /// assert_eq!(stats.snapshot(), (2, 64, 0, 0));
    /// ```
    pub fn snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.frames_received.load(Ordering::Relaxed),
            self.bytes_received.load(Ordering::Relaxed),
            self.oversized_dropped.load(Ordering::Relaxed),
            self.recv_errors.load(Ordering::Relaxed),
        )
    }

    /// Same counters as [`Self::snapshot`], structured for JSON metrics export.
    pub fn snapshot_struct(&self) -> IngestSnapshot {
        let (frames_received, bytes_received, oversized_dropped, recv_errors) = self.snapshot();
        IngestSnapshot {
            frames_received,
            bytes_received,
            oversized_dropped,
            recv_errors,
        }
    }
}

/// Serializable UDP ingest counters (see `GET /api/v1/chronus/metrics`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct IngestSnapshot {
    pub frames_received: u64,
    pub bytes_received: u64,
    pub oversized_dropped: u64,
    pub recv_errors: u64,
}

/// Binds a UDP socket according to `config`.
///
/// Use [`UdpSocket::local_addr`] on the result to learn the actual port when binding to port 0.
pub async fn bind(config: &IngestConfig) -> std::io::Result<UdpSocket> {
    UdpSocket::bind(config.bind_addr).await
}

/// Runs the ingestion loop until `shutdown` resolves.
///
/// Received datagrams are forwarded on `tx`. A send error (no live subscribers) is **not** fatal:
/// ingestion continues and statistics are still updated, so the gateway can run before any client
/// connects. Returns `Ok(())` on a clean shutdown.
#[tracing::instrument(name = "ingest", skip_all, fields(local = %socket.local_addr().map(|a| a.to_string()).unwrap_or_default()))]
pub async fn run(
    socket: UdpSocket,
    tx: broadcast::Sender<RawFrame>,
    config: IngestConfig,
    stats: Arc<IngestStats>,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let mut buf = vec![0u8; config.max_datagram_size];
    tokio::pin!(shutdown);

    tracing::info!(
        max_datagram_size = config.max_datagram_size,
        channel_capacity = config.channel_capacity,
        "ingestion loop started"
    );

    loop {
        tokio::select! {
            // Prefer shutdown over draining a busy socket so cancellation is prompt.
            biased;

            _ = &mut shutdown => {
                tracing::info!("shutdown signal received; stopping ingestion loop");
                break;
            }

            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, source)) => {
                        // `len <= buf.len()` always; on Unix an oversized datagram is truncated
                        // to the buffer here (M2's CCSDS length check will reject partials).
                        let frame = RawFrame {
                            bytes: Arc::from(&buf[..len]),
                            received_at: Utc::now(),
                            source,
                        };
                        stats.frames_received.fetch_add(1, Ordering::Relaxed);
                        stats.bytes_received.fetch_add(len as u64, Ordering::Relaxed);

                        // Lossy by design: ignore "no subscribers" errors.
                        let _ = tx.send(frame);
                    }
                    Err(e) if e.raw_os_error() == Some(WSAEMSGSIZE) => {
                        stats.oversized_dropped.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            "datagram exceeded max_datagram_size ({} bytes); dropped",
                            config.max_datagram_size
                        );
                    }
                    Err(e) => {
                        stats.recv_errors.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(error = %e, "recv_from error; continuing");
                    }
                }
            }
        }
    }

    let (frames, bytes, oversized, errors) = stats.snapshot();
    tracing::info!(frames, bytes, oversized, errors, "ingestion loop stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn loopback_config() -> IngestConfig {
        IngestConfig {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            channel_capacity: 8,
            max_datagram_size: 64,
            ..IngestConfig::default()
        }
    }

    #[test]
    fn stats_snapshot_tuple_and_struct_agree() {
        let s = IngestStats::default();
        s.frames_received.store(4, Ordering::Relaxed);
        s.bytes_received.store(100, Ordering::Relaxed);
        s.oversized_dropped.store(1, Ordering::Relaxed);
        s.recv_errors.store(2, Ordering::Relaxed);
        assert_eq!(s.snapshot(), (4, 100, 1, 2));
        let st = s.snapshot_struct();
        assert_eq!(
            (
                st.frames_received,
                st.bytes_received,
                st.oversized_dropped,
                st.recv_errors
            ),
            (4, 100, 1, 2)
        );
    }

    #[test]
    fn ingest_snapshot_serializes_with_stable_field_names() {
        let v = serde_json::to_value(IngestStats::default().snapshot_struct()).expect("json");
        for key in [
            "frames_received",
            "bytes_received",
            "oversized_dropped",
            "recv_errors",
        ] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
    }

    #[tokio::test]
    async fn bind_port_zero_picks_an_ephemeral_port() {
        let sock = bind(&loopback_config()).await.expect("bind");
        assert_ne!(sock.local_addr().expect("addr").port(), 0);
    }

    #[tokio::test]
    async fn run_forwards_datagrams_and_counts_bytes() {
        let cfg = loopback_config();
        let sock = bind(&cfg).await.expect("bind");
        let target = sock.local_addr().expect("addr");
        let (tx, mut rx) = broadcast::channel(cfg.channel_capacity);
        let stats = Arc::new(IngestStats::default());
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(run(sock, tx, cfg, Arc::clone(&stats), async {
            let _ = stop_rx.await;
        }));

        let sender = UdpSocket::bind("127.0.0.1:0").await.expect("sender");
        sender.send_to(b"hello", target).await.expect("send");
        let frame = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("frame within 2s")
            .expect("recv");
        assert_eq!(&*frame.bytes, b"hello");
        assert_eq!(frame.source, sender.local_addr().expect("addr"));

        stop_tx.send(()).expect("stop");
        task.await.expect("join").expect("clean shutdown");
        assert_eq!(stats.snapshot().0, 1);
        assert_eq!(stats.snapshot().1, 5);
    }

    #[tokio::test]
    async fn run_keeps_counting_with_no_subscribers() {
        let cfg = loopback_config();
        let sock = bind(&cfg).await.expect("bind");
        let target = sock.local_addr().expect("addr");
        let (tx, rx) = broadcast::channel(cfg.channel_capacity);
        drop(rx); // nobody listening: sends fail, ingest must not care
        let stats = Arc::new(IngestStats::default());
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(run(sock, tx, cfg, Arc::clone(&stats), async {
            let _ = stop_rx.await;
        }));
        let sender = UdpSocket::bind("127.0.0.1:0").await.expect("sender");
        for _ in 0..3 {
            sender.send_to(b"x", target).await.expect("send");
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while stats.snapshot().0 < 3 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("3 frames counted");
        stop_tx.send(()).expect("stop");
        task.await.expect("join").expect("clean shutdown");
    }

    #[tokio::test]
    async fn run_returns_promptly_when_shutdown_is_already_ready() {
        let cfg = loopback_config();
        let sock = bind(&cfg).await.expect("bind");
        let (tx, _rx) = broadcast::channel(cfg.channel_capacity);
        let res = tokio::time::timeout(
            Duration::from_secs(1),
            run(sock, tx, cfg, Arc::new(IngestStats::default()), async {}),
        )
        .await
        .expect("shutdown should win the biased select");
        assert!(res.is_ok());
    }
}
