//! End-to-end: real server on an ephemeral port, real WebSocket clients.

use std::net::SocketAddr;
use std::time::Duration;

use chronus_demo_server::server::{AppState, ServerConfig, router};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

async fn spawn(cfg: ServerConfig) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(AppState::new(cfg));
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    addr
}

fn fast_cfg() -> ServerConfig {
    ServerConfig {
        tick_hz: 20,
        session_secs: 30,
        ..ServerConfig::default()
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: SocketAddr) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/sandbox/ws"))
        .await
        .expect("connect");
    ws
}

async fn next_json(ws: &mut Ws) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for message")
            .expect("stream ended")
            .expect("ws error");
        if let Message::Text(t) = msg {
            return serde_json::from_str(&t).expect("json");
        }
    }
}

/// Reads until an `openmct.realtime.v1` frame satisfying `pred` appears (max 100 messages).
async fn frame_where(ws: &mut Ws, pred: impl Fn(&Value) -> bool) -> Option<Value> {
    for _ in 0..100 {
        let v = next_json(ws).await;
        if v["chronus_schema"] == "openmct.realtime.v1" && pred(&v) {
            return Some(v);
        }
    }
    None
}

#[tokio::test]
async fn visitor_gets_hello_then_frames_and_can_inject_a_fault() {
    let addr = spawn(fast_cfg()).await;
    let mut ws = connect(addr).await;

    let hello = next_json(&mut ws).await;
    assert_eq!(hello["kind"], "hello");
    assert!(hello["scenarios"].as_array().unwrap().len() >= 8);

    let nominal = frame_where(&mut ws, |_| true).await.expect("a frame");
    assert_eq!(nominal["physics_flags"], 0);

    ws.send(Message::Text(
        r#"{"cmd":"scenario","value":"body_rate"}"#.into(),
    ))
    .await
    .unwrap();
    let faulty = frame_where(&mut ws, |v| {
        v["physics_flags"].as_u64().unwrap_or(0) & 0x40 != 0
    })
    .await;
    assert!(
        faulty.is_some(),
        "bit 6 should light up after injecting body_rate"
    );

    ws.send(Message::Text(
        r#"{"cmd":"scenario","value":"nominal"}"#.into(),
    ))
    .await
    .unwrap();
    // Drain a few in-flight frames, then expect it to settle back to clean.
    let clean = frame_where(&mut ws, |v| v["physics_flags"] == 0).await;
    assert!(clean.is_some(), "alarms clear after returning to nominal");
}

#[tokio::test]
async fn one_visitors_fault_does_not_leak_to_another() {
    let addr = spawn(fast_cfg()).await;
    let mut a = connect(addr).await;
    let mut b = connect(addr).await;
    next_json(&mut a).await;
    next_json(&mut b).await;

    a.send(Message::Text(
        r#"{"cmd":"scenario","value":"thermal"}"#.into(),
    ))
    .await
    .unwrap();
    assert!(
        frame_where(&mut a, |v| v["physics_flags"].as_u64().unwrap_or(0) & 0x20
            != 0)
        .await
        .is_some()
    );
    for _ in 0..20 {
        let v = frame_where(&mut b, |_| true).await.unwrap();
        assert_eq!(v["physics_flags"], 0, "B must stay nominal");
    }
}

#[tokio::test]
async fn per_ip_cap_rejects_extra_sessions_and_frees_slot_on_close() {
    let cfg = ServerConfig {
        max_per_ip: 2,
        ..fast_cfg()
    };
    let addr = spawn(cfg).await;
    let mut s1 = connect(addr).await;
    let s2 = connect(addr).await;
    let third = tokio_tungstenite::connect_async(format!("ws://{addr}/sandbox/ws")).await;
    assert!(third.is_err(), "third session from the same IP is refused");

    s1.close(None).await.ok();
    drop(s1);
    drop(s2);
    // Slot becomes available again once the server notices the close.
    let mut ok = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if tokio_tungstenite::connect_async(format!("ws://{addr}/sandbox/ws"))
            .await
            .is_ok()
        {
            ok = true;
            break;
        }
    }
    assert!(ok, "slot should free after sessions close");
}

#[tokio::test]
async fn wrong_origin_is_refused_and_right_origin_accepted() {
    let cfg = ServerConfig {
        allowed_origins: vec!["https://demo.chronusgateway.com".into()],
        ..fast_cfg()
    };
    let addr = spawn(cfg).await;

    let mut bad = format!("ws://{addr}/sandbox/ws")
        .into_client_request()
        .unwrap();
    bad.headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(bad).await.is_err());

    let mut good = format!("ws://{addr}/sandbox/ws")
        .into_client_request()
        .unwrap();
    good.headers_mut()
        .insert("origin", "https://demo.chronusgateway.com".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(good).await.is_ok());
}

#[tokio::test]
async fn session_ends_at_its_time_limit() {
    let cfg = ServerConfig {
        session_secs: 1,
        ..fast_cfg()
    };
    let addr = spawn(cfg).await;
    let mut ws = connect(addr).await;
    let mut saw_ended = false;
    for _ in 0..200 {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v["kind"] == "ended" {
                    saw_ended = true;
                    break;
                }
            }
            Ok(Some(Ok(_))) => {}
            _ => break,
        }
    }
    assert!(saw_ended, "server announces the end of the session");
}

#[tokio::test]
async fn health_and_scenarios_endpoints_respond() {
    let addr = spawn(fast_cfg()).await;
    // Minimal HTTP/1.0 GETs over a raw socket keep the test dependency-free.
    async fn get(addr: SocketAddr, path: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(format!("GET {path} HTTP/1.0\r\nHost: x\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    }
    let h = get(addr, "/health").await;
    assert!(h.starts_with("HTTP/1.0 200") || h.starts_with("HTTP/1.1 200"));
    assert!(h.contains("\"status\":\"ok\""));
    let s = get(addr, "/api/v1/sandbox/scenarios").await;
    assert!(s.contains("doppler_spoof"));
}
