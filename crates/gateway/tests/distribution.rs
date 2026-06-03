//! Milestone 5 integration tests: Axum HTTP + WebSocket distribution.
//!
//! Uses in-process `Router::oneshot` for `/health` and a short-lived `axum::serve` for WebSocket
//! tests (no external browser).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{TimeZone, Utc};
use chronus_gateway::config::StationConfig;
use chronus_gateway::http;
use chronus_gateway::ingest::{IngestStats, RawFrame};
use chronus_gateway::metrics::GatewayMetrics;
use chronus_gateway::state::SharedGateway;
use futures_util::StreamExt;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

/// Minimal valid TM packet (primary header + one-byte payload).
fn build_tm(apid: u16, seq_count: u16, payload: &[u8]) -> Vec<u8> {
    assert!(!payload.is_empty(), "CCSDS data field must be >= 1 byte");
    let version = 0u16;
    let ptype = 0u16; // TM
    let sec_hdr = 0u16;
    let word1 = (version << 13) | (ptype << 12) | (sec_hdr << 11) | (apid & 0x07FF);
    let seq_flags = 0b11u16;
    let word2 = (seq_flags << 14) | (seq_count & 0x3FFF);
    let data_len = (payload.len() - 1) as u16;
    let mut v = Vec::with_capacity(6 + payload.len());
    v.extend_from_slice(&word1.to_be_bytes());
    v.extend_from_slice(&word2.to_be_bytes());
    v.extend_from_slice(&data_len.to_be_bytes());
    v.extend_from_slice(payload);
    v
}

fn test_gateway() -> Arc<SharedGateway> {
    let (tx, _rx) = broadcast::channel(16);
    Arc::new(SharedGateway::new(
        tx,
        Arc::new(IngestStats::default()),
        Arc::new(GatewayMetrics::default()),
        Arc::new(StationConfig::default()),
        None,
    ))
}

#[tokio::test]
async fn health_returns_200() {
    let app = http::router(test_gateway());
    let res = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn websocket_receives_openmct_json() {
    let cancel = CancellationToken::new();
    let state = test_gateway();
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = http::router(Arc::clone(&state));
    let c = cancel.clone();
    let join = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move { c.cancelled().await })
            .await;
    });

    let ws_url = format!("ws://127.0.0.1:{}/telemetry/openmct", addr.port());
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url)
        .await
        .expect("websocket connect");

    let fixed = Utc.with_ymd_and_hms(2020, 7, 12, 12, 0, 0).unwrap();
    let bytes = build_tm(0x2A, 7, b"hello");
    let frame = RawFrame {
        bytes: Arc::from(bytes.into_boxed_slice()),
        received_at: fixed,
        source: "127.0.0.1:5000".parse().expect("addr"),
    };
    state.frame_tx.send(frame).expect("broadcast send");

    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("timed out waiting for WS message")
        .expect("stream ended")
        .expect("ws error");
    let text = match msg {
        WsMessage::Text(t) => t,
        other => panic!("expected Text, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(v["chronus_schema"], "openmct.realtime.v1");
    assert_eq!(v["apid"], 42);
    assert_eq!(v["seq_count"], 7);
    assert_eq!(v["physics_flags"], 0);

    cancel.cancel();
    join.await.expect("server join");
}

#[tokio::test]
async fn second_client_still_receives_after_first_disconnects() {
    let cancel = CancellationToken::new();
    let state = test_gateway();
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = http::router(Arc::clone(&state));
    let c = cancel.clone();
    let join = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move { c.cancelled().await })
            .await;
    });

    let url = |path: &str| format!("ws://127.0.0.1:{}{}", addr.port(), path);
    let (mut ws1, _) = tokio_tungstenite::connect_async(url("/telemetry/openmct"))
        .await
        .expect("ws1");
    let (mut ws2, _) = tokio_tungstenite::connect_async(url("/telemetry/openmct"))
        .await
        .expect("ws2");

    let t = Utc.with_ymd_and_hms(2020, 7, 12, 12, 0, 0).unwrap();
    let send_frame = |apid: u16, payload: &[u8]| {
        let bytes = build_tm(apid, 1, payload);
        RawFrame {
            bytes: Arc::from(bytes.into_boxed_slice()),
            received_at: t,
            source: "127.0.0.1:5001".parse().unwrap(),
        }
    };

    state.frame_tx.send(send_frame(0x10, b"a")).expect("send1");
    let _ = ws1.next().await.expect("ws1 msg").expect("ws1 ok");
    let _ = ws2.next().await.expect("ws2 msg").expect("ws2 ok");

    ws1.close(None).await.expect("close ws1");
    drop(ws1);

    state.frame_tx.send(send_frame(0x11, b"b")).expect("send2");
    let msg2 = tokio::time::timeout(Duration::from_secs(2), ws2.next())
        .await
        .expect("timeout")
        .expect("stream")
        .expect("ws2");
    let text = match msg2 {
        WsMessage::Text(t) => t,
        other => panic!("expected Text, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(v["apid"], 17);

    cancel.cancel();
    join.await.expect("join");
}
