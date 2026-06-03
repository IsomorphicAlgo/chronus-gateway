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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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
