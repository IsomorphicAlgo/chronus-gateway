//! The single telemetry processing pipeline (Tranche 3, `Methodology.md` D-048).
//!
//! Before 0.5.0 every WebSocket client parsed and validated every datagram itself. That was simple
//! but meant physics ran once **per client**, nothing was processed while no dashboard was
//! connected, and stateful steps were impossible. Transfer-frame reassembly and history both need
//! to see each datagram **exactly once**, so processing now happens in one task:
//!
//! ```text
//! UDP ingest ──RawFrame──► pipeline ──► [TM deframer] ──► parse + co-validate ──► JSON
//!                                                                                  ├─► history ring (+ file)
//!                                                                                  └─► telemetry_tx ──► every WebSocket client
//! ```
//!
//! Embedders that serve [`crate::http::router`] must start the pipeline with [`spawn`]; the
//! router's WebSocket only relays what the pipeline publishes.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use crate::config::FramingMode;
use crate::frames::TmDeframer;
use crate::ingest::RawFrame;
use crate::state::SharedGateway;

/// Capacity of the processed-telemetry broadcast channel feeding WebSocket clients.
pub const TELEMETRY_CHANNEL_CAPACITY: usize = 4_096;

/// Pipeline counters (exposed as `pipeline` in `GET /api/v1/chronus/metrics`).
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct PipelineStats {
    /// Datagrams the pipeline missed because it fell behind the ingest channel.
    pub datagrams_lagged: AtomicU64,
    /// Messages a slow WebSocket client missed because it fell behind.
    pub ws_messages_dropped: AtomicU64,
    /// Messages published to WebSocket clients and history.
    pub messages_published: AtomicU64,
}

/// Serializable [`PipelineStats`] snapshot.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[non_exhaustive]
pub struct PipelineSnapshot {
    /// Datagrams missed by the pipeline.
    pub datagrams_lagged: u64,
    /// Messages missed by slow WebSocket clients.
    pub ws_messages_dropped: u64,
    /// Messages published.
    pub messages_published: u64,
}

impl PipelineStats {
    /// A `Relaxed` snapshot.
    pub fn snapshot(&self) -> PipelineSnapshot {
        PipelineSnapshot {
            datagrams_lagged: self.datagrams_lagged.load(Ordering::Relaxed),
            ws_messages_dropped: self.ws_messages_dropped.load(Ordering::Relaxed),
            messages_published: self.messages_published.load(Ordering::Relaxed),
        }
    }
}

/// Subscribes to `state.frame_tx` **now** and runs [`run`] on a new task until `shutdown`.
///
/// Subscribing before spawning guarantees no datagram sent after this call is missed.
pub fn spawn(
    state: Arc<SharedGateway>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    let rx = state.frame_tx.subscribe();
    tokio::spawn(run(state, rx, shutdown))
}

/// Processes every datagram from `rx` until `shutdown` resolves or the channel closes.
pub async fn run(
    state: Arc<SharedGateway>,
    mut rx: broadcast::Receiver<RawFrame>,
    shutdown: impl Future<Output = ()>,
) {
    let mut deframer = (state.station.framing.mode == FramingMode::TmFrames)
        .then(|| TmDeframer::new(&state.station.framing, Arc::clone(&state.framing)));
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            recv = rx.recv() => match recv {
                Ok(raw) => handle_datagram(&state, deframer.as_mut(), &raw),
                Err(RecvError::Lagged(n)) => {
                    state.pipeline.datagrams_lagged.fetch_add(n, Ordering::Relaxed);
                    tracing::warn!(missed = n, "telemetry pipeline fell behind; datagrams skipped");
                    // A gap in the byte stream invalidates partial frames and packets.
                    if let Some(d) = deframer.as_mut() {
                        d.reset();
                    }
                }
                Err(RecvError::Closed) => break,
            },
        }
    }
}

/// Processes one datagram synchronously: deframes it (in TM mode) and publishes every resulting
/// message. Public so tests and embedders can drive the pipeline without a task.
pub fn handle_datagram(state: &SharedGateway, deframer: Option<&mut TmDeframer>, raw: &RawFrame) {
    match deframer {
        None => publish(state, raw, None),
        Some(d) => {
            let mut packets: Vec<(u8, Arc<[u8]>)> = Vec::new();
            d.push(&raw.bytes, |p| packets.push((p.vcid, Arc::from(p.bytes))));
            for (vcid, bytes) in packets {
                let packet = RawFrame {
                    bytes,
                    received_at: raw.received_at,
                    source: raw.source,
                };
                publish(state, &packet, Some(vcid));
            }
        }
    }
}

fn publish(state: &SharedGateway, packet: &RawFrame, vcid: Option<u8>) {
    if let Some(json) = crate::http::process_packet(state, packet, vcid) {
        let json: Arc<str> = Arc::from(json);
        state.history.record(Arc::clone(&json));
        state
            .pipeline
            .messages_published
            .fetch_add(1, Ordering::Relaxed);
        // No subscribers is normal (no dashboard connected); history still has it.
        let _ = state.telemetry_tx.send(json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StationConfig;
    use crate::frames::{TmFrameBuilder, to_cadu};
    use crate::ingest::IngestStats;
    use crate::metrics::GatewayMetrics;
    use crate::{encode_synthetic_tm, history::HistoryQuery};
    use std::time::Duration;

    fn gateway(configure: impl FnOnce(&mut StationConfig)) -> Arc<SharedGateway> {
        let mut station = StationConfig::default();
        configure(&mut station);
        let (tx, _rx) = broadcast::channel(64);
        Arc::new(SharedGateway::new(
            tx,
            Arc::new(IngestStats::default()),
            Arc::new(GatewayMetrics::default()),
            Arc::new(station),
            None,
        ))
    }

    fn raw(bytes: &[u8]) -> RawFrame {
        RawFrame {
            bytes: Arc::from(bytes),
            received_at: chrono::Utc::now(),
            source: "127.0.0.1:9".parse().unwrap(),
        }
    }

    #[test]
    fn packets_mode_publishes_each_datagram_once_to_history_and_clients() {
        let g = gateway(|_| {});
        let mut client_a = g.telemetry_tx.subscribe();
        let mut client_b = g.telemetry_tx.subscribe();
        handle_datagram(&g, None, &raw(&encode_synthetic_tm(5, 1, b"x")));
        handle_datagram(&g, None, &raw(&[1, 2, 3])); // garbage: counted, not published
        assert_eq!(client_a.try_recv().unwrap(), client_b.try_recv().unwrap());
        assert!(client_a.try_recv().is_err());
        assert_eq!(g.history.len(), 1);
        let m = g.gateway_metrics.snapshot();
        assert_eq!(
            (m.telemetry_frames_emitted, m.telemetry_parse_errors),
            (1, 1)
        );
        assert_eq!(g.pipeline.snapshot().messages_published, 1);
    }

    #[test]
    fn tm_mode_deframes_and_tags_the_virtual_channel() {
        let g = gateway(|s| {
            s.framing.mode = FramingMode::TmFrames;
            s.framing.frame_length = Some(64);
            s.framing.asm = true;
            s.framing.derandomize = true;
        });
        let mut d = TmDeframer::new(&g.station.framing, Arc::clone(&g.framing));
        let p1 = encode_synthetic_tm(0x10, 1, &[1; 80]);
        let p2 = encode_synthetic_tm(0x11, 2, b"hi");
        let mut b = TmFrameBuilder::new(9, 64);
        let mut frames = b.pack(3, &[&p1, &p2]);
        frames.extend(b.flush(3));
        for f in &frames {
            handle_datagram(&g, Some(&mut d), &raw(&to_cadu(f, true)));
        }
        let page = g.history.query(&HistoryQuery::new());
        assert_eq!(page.entries.len(), 2);
        let v: serde_json::Value = serde_json::from_str(&page.entries[1].json).unwrap();
        assert_eq!(v["apid"], 0x11);
        assert_eq!(v["vcid"], 3);
        assert_eq!(
            g.framing.snapshot(FramingMode::TmFrames).packets_extracted,
            2
        );
    }

    #[tokio::test]
    async fn spawned_pipeline_relays_until_shutdown() {
        let g = gateway(|_| {});
        let mut client = g.telemetry_tx.subscribe();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let task = spawn(Arc::clone(&g), async {
            let _ = stop_rx.await;
        });
        g.frame_tx
            .send(raw(&encode_synthetic_tm(5, 1, b"x")))
            .expect("pipeline subscribed before spawn returned");
        let msg = tokio::time::timeout(Duration::from_secs(2), client.recv())
            .await
            .expect("relayed")
            .expect("msg");
        assert!(msg.contains("openmct.realtime.v1"));
        stop_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("stops")
            .expect("join");
    }

    #[tokio::test]
    async fn lag_is_counted_and_resets_the_deframer() {
        let (tx, rx) = broadcast::channel(2);
        let g = Arc::new(SharedGateway::new(
            tx.clone(),
            Arc::new(IngestStats::default()),
            Arc::new(GatewayMetrics::default()),
            Arc::new(StationConfig::default()),
            None,
        ));
        for i in 0..5 {
            let _ = tx.send(raw(&encode_synthetic_tm(5, i, b"x")));
        }
        drop(tx);
        // Keep the sender in `g` alive but stop after the backlog drains.
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(run(Arc::clone(&g), rx, async {
            let _ = stop_rx.await;
        }));
        tokio::time::sleep(Duration::from_millis(100)).await;
        stop_tx.send(()).unwrap();
        task.await.unwrap();
        assert_eq!(g.pipeline.snapshot().datagrams_lagged, 3);
        assert_eq!(g.history.len(), 2);
    }
}
