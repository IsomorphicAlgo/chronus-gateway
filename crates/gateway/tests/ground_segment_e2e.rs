//! Tranche 3 exit test (Methodology D-048 / D-049), against the **real `chronus-gateway` binary**:
//!
//! 1. A recorded CCSDS TM Transfer Frame stream (`demo/replay/fixtures/tm_frames_lab.hex`: CADUs
//!    with sync markers, randomization, CRCs, three virtual channels, a corrupted frame, junk and
//!    split datagrams) decodes end to end: every expected packet reaches the WebSocket and the
//!    history API, in order, tagged with its virtual channel.
//! 2. History survives a restart: the gateway is killed (no graceful shutdown) and started again
//!    on the same history file, and serves the same messages — what an Open MCT page reload after
//!    a gateway restart needs.

use std::net::{SocketAddr, UdpSocket as StdUdp};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../demo/replay/fixtures")
}

fn datagrams() -> Vec<Vec<u8>> {
    std::fs::read_to_string(fixtures().join("tm_frames_lab.hex"))
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            (0..l.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&l[i..i + 2], 16).unwrap())
                .collect()
        })
        .collect()
}

fn expected() -> Vec<(u64, u64, u64)> {
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("tm_frames_lab.expected.json")).unwrap(),
    )
    .unwrap();
    v["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["vcid"].as_u64().unwrap(),
                p["apid"].as_u64().unwrap(),
                p["seq"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn free_udp_port() -> u16 {
    StdUdp::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn free_tcp_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Kills the child on drop so a failing assertion never leaks a gateway process.
struct Gateway {
    child: Child,
    udp: SocketAddr,
    http: SocketAddr,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts the binary configured for the lab CADU stream, with history persisted in `dir`.
async fn start_gateway(dir: &Path) -> Gateway {
    let (udp_p, http_p) = (free_udp_port(), free_tcp_port().await);
    let history = dir.join("history.jsonl");
    let cfg = dir.join("gateway.toml");
    std::fs::write(
        &cfg,
        format!(
            r#"
[ingest]
bind_addr = "127.0.0.1:{udp_p}"
channel_capacity = 1024
max_datagram_size = 65542
http_bind = "127.0.0.1:{http_p}"

[time]
format = "cds"
max_skew_s = 0.0

[framing]
mode = "tm_frames"
frame_length = 223
asm = true
derandomize = true
spacecraft_id = 42

[history]
capacity = 100
persist_path = {history:?}
"#
        ),
    )
    .unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_chronus-gateway"))
        .arg("--config")
        .arg(&cfg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn chronus-gateway");
    let gw = Gateway {
        child,
        udp: format!("127.0.0.1:{udp_p}").parse().unwrap(),
        http: format!("127.0.0.1:{http_p}").parse().unwrap(),
    };
    for _ in 0..150 {
        if TcpStream::connect(gw.http).await.is_ok() {
            return gw;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("gateway never started listening on {}", gw.http);
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

async fn connect_ws(gw: &Gateway) -> Ws {
    let url = format!("ws://{}/telemetry/openmct", gw.http);
    let (ws, _) = tokio_tungstenite::connect_async(&url).await.expect("ws");
    for _ in 0..100 {
        let m = http_get_json(gw.http, "/api/v1/chronus/metrics").await;
        if m["gateway"]["ws_clients_connected"] == 1 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            return ws;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("gateway never reported the WebSocket client");
}

async fn http_get_json(addr: SocketAddr, path: &str) -> serde_json::Value {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.0\r\nHost: x\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8(buf).unwrap();
    let body = text.split("\r\n\r\n").nth(1).expect("http body");
    serde_json::from_str(body).expect("json body")
}

async fn wait_for(gw: &Gateway, what: &str, pred: impl Fn(&serde_json::Value) -> bool) {
    for _ in 0..200 {
        let m = http_get_json(gw.http, "/api/v1/chronus/metrics").await;
        if pred(&m) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let m = http_get_json(gw.http, "/api/v1/chronus/metrics").await;
    panic!("timed out waiting for {what}: {m}");
}

fn ids(messages: &[serde_json::Value]) -> Vec<(u64, u64, u64)> {
    messages
        .iter()
        .map(|m| {
            (
                m["vcid"].as_u64().unwrap(),
                m["apid"].as_u64().unwrap(),
                m["seq_count"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn recorded_frame_stream_decodes_end_to_end_and_history_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let want = expected();
    let gw = start_gateway(dir.path()).await;
    let mut ws = connect_ws(&gw).await;

    let tx = StdUdp::bind("127.0.0.1:0").unwrap();
    for dg in datagrams() {
        tx.send_to(&dg, gw.udp).unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // 1. Realtime: every packet, in delivery order, with its virtual channel.
    let mut live = Vec::new();
    while live.len() < want.len() {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap_or_else(|_| panic!("only {} of {} messages arrived", live.len(), want.len()))
            .expect("stream ended")
            .expect("ws error");
        if let WsMessage::Text(text) = msg {
            live.push(serde_json::from_str::<serde_json::Value>(&text).unwrap());
        }
    }
    assert_eq!(ids(&live), want);
    for m in &live {
        assert_eq!(m["physics_flags"], 0, "{m}");
        assert!(
            m["onboard_time"]
                .as_str()
                .unwrap()
                .starts_with("2020-07-13T05:0")
        );
    }

    // 2. Deframing statistics match what the encoder injected.
    let m = http_get_json(gw.http, "/api/v1/chronus/metrics").await;
    assert_eq!(m["framing"]["mode"], "tm_frames");
    assert_eq!(m["framing"]["packets_extracted"], want.len());
    assert_eq!(m["framing"]["frames_crc_error"], 1);
    assert_eq!(m["framing"]["frames_idle"], 2);
    assert_eq!(
        m["gateway"]["telemetry_frames_emitted"],
        want.len(),
        "processed once, not per client"
    );

    // 3. Status explains the lost frame as information, not a fault.
    let status = http_get_json(gw.http, "/api/v1/chronus/status").await;
    let codes: Vec<&str> = status["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"CG-513"), "{status}");
    assert_ne!(status["status"], "problem", "{status}");

    // 4. History and the Open MCT endpoints.
    let h = http_get_json(gw.http, "/api/v1/chronus/history").await;
    assert_eq!(h["chronus_schema"], "chronus.history.v1");
    let messages = h["messages"].as_array().unwrap().clone();
    assert_eq!(ids(&messages), want);
    let science = http_get_json(gw.http, "/api/v1/chronus/history?apid=0x200").await;
    assert_eq!(science["count"], 2);
    let dict = http_get_json(gw.http, "/api/v1/chronus/openmct/dictionary").await;
    assert_eq!(dict["name"], "ISS (ZARYA)");
    let el = http_get_json(
        gw.http,
        "/api/v1/chronus/openmct/history/chronus.physics.elevation_deg",
    )
    .await;
    assert_eq!(el.as_array().unwrap().len(), want.len());

    // 5. Kill the process outright (no graceful shutdown) once the file has everything...
    wait_for(&gw, "history persisted", |m| {
        m["history"]["persisted"] == want.len()
    })
    .await;
    drop(ws);
    drop(gw);

    // ...and history is back after a restart on the same file.
    let gw = start_gateway(dir.path()).await;
    let m = http_get_json(gw.http, "/api/v1/chronus/metrics").await;
    assert_eq!(m["history"]["loaded"], want.len());
    assert_eq!(m["history"]["load_skipped"], 0);
    let h = http_get_json(gw.http, "/api/v1/chronus/history").await;
    assert_eq!(
        h["messages"].as_array().unwrap(),
        &messages,
        "identical after restart"
    );
}
