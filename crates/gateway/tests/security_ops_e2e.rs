//! Tranche 4 exit tests (Methodology D-051 / D-052 / D-053) against the **real binary**:
//!
//! - `--check-config` reports a usable configuration and refuses unsafe ones (`CG-137`,
//!   `CG-138`) with exit code 2, without opening sockets;
//! - API tokens, the WebSocket `Origin` check, the client cap and the UDP sender allow-list are
//!   enforced over real sockets;
//! - `/metrics` serves Prometheus text;
//! - a TLE file replaced while the gateway runs is reloaded, Earth orientation comes from an IERS
//!   file, and `/status` shows the data age.

use std::net::{SocketAddr, UdpSocket as StdUdp};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use chronus_gateway::config::DEFAULT_ISS_TLE;
use chronus_gateway::freshness::finals_line;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const TOKEN: &str = "e2e-token-0123456789abcdef0123456789abcdef";

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

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_chronus-gateway"));
    c.env_remove("CHRONUS_API_TOKENS")
        .env_remove("CHRONUS_GATEWAY_CONFIG");
    c
}

fn check(config: &Path) -> Output {
    bin()
        .arg("--check-config")
        .arg("--config")
        .arg(config)
        .output()
        .expect("run --check-config")
}

/// The ISS TLE with its epoch moved by `days`, checksum recomputed.
fn tle_with_epoch_shift(days: u32) -> String {
    let mut lines: Vec<String> = DEFAULT_ISS_TLE
        .lines()
        .map(|l| l.trim().to_string())
        .collect();
    let l1 = &mut lines[1];
    let day: f64 = l1[20..32].parse().unwrap();
    l1.replace_range(20..32, &format!("{:012.8}", day + f64::from(days)));
    let sum: u32 = l1[..68]
        .chars()
        .map(|c| match c {
            '0'..='9' => c.to_digit(10).unwrap(),
            '-' => 1,
            _ => 0,
        })
        .sum();
    l1.replace_range(68..69, &(sum % 10).to_string());
    lines.join("\n")
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

async fn start(dir: &Path, extra: &str) -> Gateway {
    let (udp_p, http_p) = (free_udp_port(), free_tcp_port().await);
    let cfg = dir.join("gateway.toml");
    std::fs::write(
        &cfg,
        format!(
            "[ingest]\nbind_addr = \"127.0.0.1:{udp_p}\"\nchannel_capacity = 1024\n\
             max_datagram_size = 65542\nhttp_bind = \"127.0.0.1:{http_p}\"\n{extra}"
        ),
    )
    .unwrap();
    let out = check(&cfg);
    assert!(
        out.status.success(),
        "config must pass --check-config:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let child = bin()
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

/// `GET path` with an optional bearer token: (status code, body).
async fn get(addr: SocketAddr, path: &str, token: Option<&str>) -> (u16, String) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    s.write_all(format!("GET {path} HTTP/1.0\r\nHost: x\r\n{auth}\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    let code = text.split(' ').nth(1).unwrap().parse().unwrap();
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (code, body)
}

async fn get_json(addr: SocketAddr, path: &str) -> serde_json::Value {
    let (code, body) = get(addr, path, Some(TOKEN)).await;
    assert_eq!(code, 200, "{path}: {body}");
    serde_json::from_str(&body).unwrap()
}

/// Opens the WebSocket; `Ok(stream)` or `Err(HTTP status)`.
async fn ws(
    gw: &Gateway,
    token: Option<&str>,
    origin: Option<&str>,
) -> Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>, u16> {
    let q = token.map_or(String::new(), |t| format!("?access_token={t}"));
    let mut req = format!("ws://{}/telemetry/openmct{q}", gw.http)
        .into_client_request()
        .unwrap();
    if let Some(o) = origin {
        req.headers_mut().insert("origin", o.parse().unwrap());
    }
    match tokio_tungstenite::connect_async(req).await {
        Ok((s, _)) => Ok(s),
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => Err(r.status().as_u16()),
        Err(e) => panic!("unexpected WebSocket error: {e}"),
    }
}

#[test]
fn check_config_refuses_unsafe_configurations_with_explanations() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("gateway.toml");
    let ingest = "[ingest]\nbind_addr = \"127.0.0.1:7301\"\nchannel_capacity = 1024\n\
                  max_datagram_size = 65542\nhttp_bind = \"0.0.0.0:8080\"\n";

    std::fs::write(&cfg, ingest).unwrap();
    let out = check(&cfg);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("CG-137"));

    let short = dir.path().join("short.txt");
    std::fs::write(&short, "tooshort\n").unwrap();
    std::fs::write(
        &cfg,
        format!("{ingest}[security]\napi_tokens_file = {short:?}\n"),
    )
    .unwrap();
    let out = check(&cfg);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("CG-138"));

    // A real token makes the same remote bind acceptable, and the check opens no socket.
    let good = dir.path().join("tokens.txt");
    std::fs::write(&good, format!("{TOKEN}\n")).unwrap();
    std::fs::write(
        &cfg,
        format!("{ingest}[security]\napi_tokens_file = {good:?}\n"),
    )
    .unwrap();
    let out = check(&cfg);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("required (1 configured)"), "{stdout}");
    assert!(stdout.contains("Ready"), "{stdout}");

    let out = bin().arg("--generate-token").output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim().len(), 43);
    let out = bin().arg("--version").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains(env!("CARGO_PKG_VERSION")));
}

#[tokio::test]
async fn tokens_origins_client_cap_and_udp_allow_list_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let tokens = dir.path().join("tokens.txt");
    std::fs::write(&tokens, format!("# ops\n{TOKEN}\n")).unwrap();
    let gw = start(
        dir.path(),
        &format!(
            "[security]\napi_tokens_file = {tokens:?}\nallowed_origins = [\"https://ops.example.org\"]\n\
             max_ws_clients = 1\nudp_allowed_sources = [\"10.0.0.0/8\"]\n"
        ),
    )
    .await;

    assert_eq!(get(gw.http, "/health", None).await.0, 200);
    assert_eq!(get(gw.http, "/api/v1/chronus/metrics", None).await.0, 401);
    assert_eq!(
        get(gw.http, "/api/v1/chronus/metrics", Some("wrong-token"))
            .await
            .0,
        401
    );
    assert_eq!(
        get(gw.http, "/api/v1/chronus/metrics", Some(TOKEN)).await.0,
        200
    );

    assert_eq!(ws(&gw, None, None).await.err(), Some(401), "no token");
    assert_eq!(
        ws(&gw, Some(TOKEN), Some("https://evil.example.com"))
            .await
            .err(),
        Some(403),
        "foreign origin"
    );
    let first = ws(&gw, Some(TOKEN), Some("https://ops.example.org"))
        .await
        .expect("listed origin connects");
    assert_eq!(
        ws(&gw, Some(TOKEN), None).await.err(),
        Some(503),
        "cap of one"
    );
    drop(first);
    // The slot is released when the first client goes away.
    let mut reconnected = false;
    for _ in 0..50 {
        if ws(&gw, Some(TOKEN), Some("http://127.0.0.1:5174"))
            .await
            .is_ok()
        {
            reconnected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(reconnected, "slot freed after disconnect");

    // Telemetry from 127.0.0.1 is outside 10.0.0.0/8: dropped, counted, never processed.
    let tx = StdUdp::bind("127.0.0.1:0").unwrap();
    tx.send_to(&chronus_gateway::encode_synthetic_tm(1, 1, b"x"), gw.udp)
        .unwrap();
    let mut m = serde_json::Value::Null;
    for _ in 0..100 {
        m = get_json(gw.http, "/api/v1/chronus/metrics").await;
        if m["security"]["udp_rejected"] == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(m["security"]["udp_rejected"], 1, "{m}");
    assert_eq!(m["ingest"]["frames_received"], 0);
    assert_eq!(m["security"]["auth_failures"], 3);
    assert_eq!(m["security"]["ws_rejected_origin"], 1);
    assert!(m["security"]["ws_rejected_capacity"].as_u64().unwrap() >= 1);

    let (code, prom) = get(gw.http, "/metrics", Some(TOKEN)).await;
    assert_eq!(code, 200);
    assert!(
        prom.contains("chronus_security_udp_rejected_total 1"),
        "{prom}"
    );
    assert!(prom.contains("chronus_security_auth_enabled 1"));

    let status = get_json(gw.http, "/api/v1/chronus/status").await;
    let codes: Vec<&str> = status["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"CG-515"), "{status}");
}

#[tokio::test]
async fn data_files_are_reloaded_and_their_age_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let tle = dir.path().join("iss.tle");
    std::fs::write(&tle, DEFAULT_ISS_TLE).unwrap();
    // An Earth orientation file around today, in IERS finals2000A layout.
    let today = chrono::Utc::now().date_naive();
    let finals: Vec<String> = (-2..=3)
        .map(|k| {
            let d = today + chrono::Duration::days(k);
            finals_line(d, 0.15, 0.35, -0.0321, k > 0)
        })
        .collect();
    let eop = dir.path().join("finals.daily");
    std::fs::write(&eop, finals.join("\n")).unwrap();

    let gw = start(
        dir.path(),
        &format!(
            "[station]\nlatitude_deg = 35.0\nlongitude_deg = -116.0\naltitude_m = 1000.0\n\
             nominal_carrier_hz = 437500000.0\nmin_recompute_interval_ms = 10\n\
             doppler_tolerance_hz = 150.0\nminimum_elevation_deg = 0.0\ntle_file = {tle:?}\n\
             [freshness]\nreload_interval_s = 1\neop_file = {eop:?}\n"
        ),
    )
    .await;

    let status = get_json(gw.http, "/api/v1/chronus/status").await;
    let data = &status["data"];
    assert_eq!(data["ephemeris_source"], "tle_file");
    assert_eq!(data["tle_name"], "ISS (ZARYA)");
    assert!(data["tle_age_days"].as_f64().unwrap() > 2000.0);
    assert_eq!(data["eop_source"], "file");
    assert!((data["ut1_minus_utc_s"].as_f64().unwrap() - -0.0321).abs() < 1e-9);
    assert_eq!(data["eop_observed_until"], today.to_string());
    let epoch_before = data["tle_epoch"].as_str().unwrap().to_string();

    // Replace the TLE with one whose epoch is 3 days later; the gateway picks it up by itself.
    std::fs::write(&tle, tle_with_epoch_shift(3)).unwrap();
    let mut after = serde_json::Value::Null;
    for _ in 0..60 {
        after = get_json(gw.http, "/api/v1/chronus/metrics").await;
        if after["data"]["reloads"].as_u64().unwrap_or(0) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(after["data"]["reloads"].as_u64().unwrap() >= 1, "{after}");
    assert_ne!(after["data"]["tle_epoch"].as_str().unwrap(), epoch_before);
    assert_eq!(after["data"]["reload_errors"], 0);

    // A broken file is reported and the old data kept.
    std::fs::write(&tle, "this is not a TLE, and it is a different length").unwrap();
    let mut broken = serde_json::Value::Null;
    for _ in 0..60 {
        broken = get_json(gw.http, "/api/v1/chronus/status").await;
        if broken["data"]["reload_errors"].as_u64().unwrap_or(0) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        broken["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["code"] == "CG-516"),
        "{broken}"
    );
    let (_, prom) = get(gw.http, "/metrics", None).await;
    assert!(prom.contains("chronus_data_tle_age_seconds"));
    assert!(prom.contains("chronus_data_reload_errors_total 1"));
}
