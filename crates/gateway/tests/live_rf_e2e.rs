//! Tranche 2 exit test: the **real `chronus-gateway` binary**, fed over real UDP sockets, raises
//! physics flag bits driven by the `chronus.rf.v1` side channel and by spacecraft time codes.
//!
//! Before Tranche 2 the Doppler (bit 0), link-budget (bit 2) and pointing (bit 3) checks only ran
//! in-process. Here a spawned binary receives telemetry on its ingest port, receiver measurements
//! on its RF port, and the Open MCT WebSocket reports the flags.
//!
//! Spacecraft time makes the test deterministic: each packet carries a CDS time field naming a
//! chosen mission instant, and `time.max_skew_s = 0` (lab mode) makes the gateway evaluate physics
//! there instead of at wall-clock "now". The expected measurements come from the library's own
//! propagator at that instant, so "nominal" really is consistent and each perturbation is the only
//! thing that can raise its bit.

use std::net::{SocketAddr, UdpSocket as StdUdp};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use chrono::{DateTime, Utc};
use chronus_gateway::ccsds::{encode_synthetic_tm, encode_synthetic_tm_with_time};
use chronus_gateway::config::StationConfig;
use chronus_gateway::propagator::{TrackingState, propagator_from_station};
use chronus_gateway::rf::RfSampleV1;
use chronus_gateway::time::{encode_cds, encode_cuc};
use chronus_gateway::validate::ValidationContext;
use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

/// Public ISS epoch used by the gateway's default TLE.
fn epoch() -> DateTime<Utc> {
    "2020-07-12T21:00:00Z".parse().unwrap()
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
    rf: SocketAddr,
    http: SocketAddr,
    _dir: tempfile::TempDir,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts the binary with `time_toml` as the body of its `[time]` table.
async fn start_gateway(time_toml: &str) -> Gateway {
    let (udp_p, rf_p, http_p) = (free_udp_port(), free_udp_port(), free_tcp_port().await);
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("gateway.toml");
    std::fs::write(
        &cfg,
        format!(
            r#"
[ingest]
bind_addr = "127.0.0.1:{udp_p}"
channel_capacity = 1024
max_datagram_size = 65542
http_bind = "127.0.0.1:{http_p}"

[rf]
bind_addr = "127.0.0.1:{rf_p}"
window_ms = 100

[time]
{time_toml}
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
    Gateway {
        child,
        udp: format!("127.0.0.1:{udp_p}").parse().unwrap(),
        rf: format!("127.0.0.1:{rf_p}").parse().unwrap(),
        http: format!("127.0.0.1:{http_p}").parse().unwrap(),
        _dir: dir,
    }
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

async fn connect_ws(gw: &Gateway) -> Ws {
    let url = format!("ws://{}/telemetry/openmct", gw.http);
    for _ in 0..150 {
        if let Ok((ws, _)) = tokio_tungstenite::connect_async(&url).await {
            // The server subscribes to the frame broadcast just after the upgrade completes; wait
            // until it reports the client, or an immediate first packet could be missed.
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
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("gateway never accepted a WebSocket connection at {url}");
}

/// One round trip: optional RF sample (stamped on arrival), then the packet; returns the JSON.
async fn exchange(
    gw: &Gateway,
    ws: &mut Ws,
    tx: &StdUdp,
    rf: Option<&RfSampleV1>,
    packet: &[u8],
) -> serde_json::Value {
    if let Some(rf) = rf {
        tx.send_to(&rf.to_datagram(), gw.rf).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tx.send_to(packet, gw.udp).unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("timed out waiting for a WebSocket message")
        .expect("stream ended")
        .expect("ws error");
    let WsMessage::Text(text) = msg else {
        panic!("expected a text message, got {msg:?}");
    };
    // Let the RF sample age out of the 100 ms window before the next scenario.
    tokio::time::sleep(Duration::from_millis(150)).await;
    serde_json::from_str(&text).expect("json")
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

/// First instant (30 s steps from the TLE epoch) satisfying `pred(elevation_deg)`.
fn find_time(
    station: &StationConfig,
    pred: impl Fn(f64) -> bool,
) -> (DateTime<Utc>, TrackingState) {
    let prop = propagator_from_station(station).unwrap();
    let mut t = epoch();
    for _ in 0..(3 * 24 * 120) {
        if let Ok(s) = prop.tracking_state(t)
            && pred(s.elevation_deg)
        {
            return (t, s);
        }
        t += chrono::Duration::seconds(30);
    }
    panic!("no matching geometry within three days of the TLE epoch");
}

fn cds_packet(apid: u16, seq: u16, t: DateTime<Utc>, user: &[u8]) -> Vec<u8> {
    encode_synthetic_tm_with_time(apid, seq, &encode_cds(t).unwrap(), user)
}

fn flags(v: &serde_json::Value) -> u64 {
    v["physics_flags"].as_u64().unwrap()
}

#[tokio::test]
async fn rf_side_channel_drives_doppler_link_budget_and_pointing_bits() {
    let station = StationConfig::default();
    let ctx = ValidationContext::from_station(&station);
    let (t, state) = find_time(&station, |el| el >= 20.0);
    let (t_low, _) = find_time(&station, |el| el <= -20.0);
    let nominal = RfSampleV1::nominal_for(&ctx, &state);

    let gw = start_gateway("format = \"cds\"\nmax_skew_s = 0.0").await;
    let mut ws = connect_ws(&gw).await;
    let tx = StdUdp::bind("127.0.0.1:0").unwrap();
    let mut seq = 0u16;
    let mut next = || {
        seq += 1;
        seq
    };

    // Nominal receiver data at a visible instant: nothing flagged, and the sample was used.
    let v = exchange(
        &gw,
        &mut ws,
        &tx,
        Some(&nominal),
        &cds_packet(0x100, next(), t, b"x"),
    )
    .await;
    assert_eq!(flags(&v), 0, "nominal RF must raise no alarm: {v}");
    assert_eq!(v["rf_matched"], true);
    assert_eq!(v["physics_flags_v2"], 0);
    assert_eq!(
        v["physics_time"], v["onboard_time"],
        "lab mode runs physics at onboard time"
    );
    assert!(
        (v["elevation_deg"].as_f64().unwrap() - state.elevation_deg).abs() < 1e-6,
        "physics must be evaluated at the onboard time: {v}"
    );

    // Each perturbation raises exactly its own bit.
    let mut dop = nominal;
    dop.carrier_hz = nominal.carrier_hz.map(|c| c + 1_000.0);
    let v = exchange(
        &gw,
        &mut ws,
        &tx,
        Some(&dop),
        &cds_packet(0x100, next(), t, b"x"),
    )
    .await;
    assert_eq!(flags(&v), 0x01, "Doppler: {v}");

    let mut link = nominal;
    link.rx_power_dbm = nominal.rx_power_dbm.map(|p| p + 20.0);
    let v = exchange(
        &gw,
        &mut ws,
        &tx,
        Some(&link),
        &cds_packet(0x100, next(), t, b"x"),
    )
    .await;
    assert_eq!(flags(&v), 0x04, "link budget: {v}");

    let mut point = nominal;
    point.az_deg = nominal.az_deg.map(|a| (a + 5.0) % 360.0);
    let v = exchange(
        &gw,
        &mut ws,
        &tx,
        Some(&point),
        &cds_packet(0x100, next(), t, b"x"),
    )
    .await;
    assert_eq!(flags(&v), 0x08, "pointing: {v}");

    let mut all3 = dop;
    all3.rx_power_dbm = link.rx_power_dbm;
    all3.az_deg = point.az_deg;
    let v = exchange(
        &gw,
        &mut ws,
        &tx,
        Some(&all3),
        &cds_packet(0x100, next(), t, b"x"),
    )
    .await;
    assert_eq!(flags(&v), 0x0D, "all three RF checks together: {v}");

    // No sample near the frame: those checks are skipped, not failed.
    let v = exchange(&gw, &mut ws, &tx, None, &cds_packet(0x100, next(), t, b"x")).await;
    assert_eq!(flags(&v), 0, "no RF -> RF checks skipped: {v}");
    assert_eq!(v["rf_matched"], false);

    // Below the horizon at the onboard time: bit 1, independent of RF.
    let v = exchange(
        &gw,
        &mut ws,
        &tx,
        None,
        &cds_packet(0x100, next(), t_low, b"x"),
    )
    .await;
    assert_eq!(flags(&v), 0x02, "horizon: {v}");

    // Counters are visible to operators.
    let m = http_get_json(gw.http, "/api/v1/chronus/metrics").await;
    assert_eq!(m["rf"]["enabled"], true);
    assert_eq!(m["rf"]["samples_received"], 5);
    assert_eq!(m["rf"]["frames_matched"], 5);
    assert_eq!(m["rf"]["frames_unmatched"], 2);
    assert_eq!(m["rf"]["samples_invalid"], 0);

    // Garbage on the RF port is counted, never fatal.
    tx.send_to(b"not json", gw.rf).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let m = http_get_json(gw.http, "/api/v1/chronus/metrics").await;
    assert_eq!(m["rf"]["samples_invalid"], 1);
    let status = http_get_json(gw.http, "/api/v1/chronus/status").await;
    let codes: Vec<_> = status["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["code"].as_str().unwrap().to_string())
        .collect();
    assert!(codes.contains(&"CG-512".to_string()), "{codes:?}");
}

#[cfg(feature = "hil")]
#[tokio::test]
async fn all_seven_physics_bits_fire_through_the_real_binary() {
    use chronus_gateway::hil_tm::encode_hil_tm_v1_payload;

    let station = StationConfig::default();
    let ctx = ValidationContext::from_station(&station);
    let h = &station.hil;
    let (t, state) = find_time(&station, |el| el >= 20.0);
    let (t_low, state_low) = find_time(&station, |el| el <= -20.0);
    let apid = h.tm_v1_apid_min;

    let expect = |s: &TrackingState| {
        assert!(
            s.nadir_sun_illum_cos.is_finite(),
            "hil feature supplies illumination"
        );
        let i = s.nadir_sun_illum_cos;
        (
            (h.eps_voltage_eclipse_v + (h.eps_voltage_full_sun_v - h.eps_voltage_eclipse_v) * i)
                as f32,
            (h.thermal_c_eclipse + (h.thermal_c_full_sun - h.thermal_c_eclipse) * i) as f32,
        )
    };
    let (v_ok, t_ok) = expect(&state);

    let gw = start_gateway("format = \"cds\"\nmax_skew_s = 0.0").await;
    let mut ws = connect_ws(&gw).await;
    let tx = StdUdp::bind("127.0.0.1:0").unwrap();
    let nominal = RfSampleV1::nominal_for(&ctx, &state);
    let pkt = |seq, when, v: f32, th: f32, w: f32| {
        cds_packet(
            apid,
            seq,
            when,
            &encode_hil_tm_v1_payload(seq as u32, v, th, w),
        )
    };

    let v = exchange(
        &gw,
        &mut ws,
        &tx,
        Some(&nominal),
        &pkt(1, t, v_ok, t_ok, 0.1),
    )
    .await;
    assert_eq!(flags(&v), 0, "self-consistent HIL + RF raises nothing: {v}");

    let v = exchange(
        &gw,
        &mut ws,
        &tx,
        Some(&nominal),
        &pkt(2, t, v_ok - 10.0, t_ok, 0.1),
    )
    .await;
    assert_eq!(flags(&v), 0x10, "EPS: {v}");
    let v = exchange(
        &gw,
        &mut ws,
        &tx,
        Some(&nominal),
        &pkt(3, t, v_ok, t_ok + 40.0, 0.1),
    )
    .await;
    assert_eq!(flags(&v), 0x20, "thermal: {v}");
    let v = exchange(
        &gw,
        &mut ws,
        &tx,
        Some(&nominal),
        &pkt(4, t, v_ok, t_ok, 50.0),
    )
    .await;
    assert_eq!(flags(&v), 0x40, "body rate: {v}");

    // Everything at once, below the horizon: bits 0..=6 all set (0x7F).
    let (v_low, t_low_c) = expect(&state_low);
    let mut bad = RfSampleV1::nominal_for(&ctx, &state_low);
    bad.carrier_hz = bad.carrier_hz.map(|c| c + 1_000.0);
    bad.rx_power_dbm = bad.rx_power_dbm.map(|p| p + 20.0);
    bad.az_deg = bad.az_deg.map(|a| (a + 5.0) % 360.0);
    let v = exchange(
        &gw,
        &mut ws,
        &tx,
        Some(&bad),
        &pkt(5, t_low, v_low - 10.0, t_low_c + 40.0, 50.0),
    )
    .await;
    assert_eq!(flags(&v), 0x7F, "all seven bits: {v}");
}

#[tokio::test]
async fn untrusted_or_unreadable_time_falls_back_to_receive_time_and_flags_v2() {
    let gw = start_gateway("format = \"cds\"\nmax_skew_s = 5.0").await;
    let mut ws = connect_ws(&gw).await;
    let tx = StdUdp::bind("127.0.0.1:0").unwrap();

    // Onboard time six years in the past: skew flag, physics at receive time.
    let v = exchange(
        &gw,
        &mut ws,
        &tx,
        None,
        &cds_packet(0x100, 1, epoch(), b"x"),
    )
    .await;
    assert_eq!(v["physics_flags_v2"], 1, "time-tag skew: {v}");
    assert_eq!(v["physics_time"], v["received_at"]);
    assert!(
        v["onboard_time"].is_string(),
        "the claimed time is still reported: {v}"
    );

    // Onboard time ~ now: trusted, no flag.
    let v = exchange(
        &gw,
        &mut ws,
        &tx,
        None,
        &cds_packet(0x100, 2, Utc::now(), b"x"),
    )
    .await;
    assert_eq!(v["physics_flags_v2"], 0, "{v}");
    assert_eq!(v["physics_time"], v["onboard_time"]);

    // Secondary header too short to hold a time code: invalid flag, fallback.
    let short = encode_synthetic_tm_with_time(0x100, 3, &[1, 2, 3], &[]);
    let v = exchange(&gw, &mut ws, &tx, None, &short).await;
    assert_eq!(v["physics_flags_v2"], 2, "time code invalid: {v}");
    assert!(v.get("onboard_time").is_none());

    // No secondary header: not flagged, nothing to decode.
    let v = exchange(
        &gw,
        &mut ws,
        &tx,
        None,
        &encode_synthetic_tm(0x100, 4, b"x"),
    )
    .await;
    assert_eq!(v["physics_flags_v2"], 0, "{v}");
    assert!(v.get("onboard_time").is_none());
    assert_eq!(
        v["payload_base64"], "eA==",
        "payload untouched without a time field"
    );

    let m = http_get_json(gw.http, "/api/v1/chronus/metrics").await;
    // The two v2-flagged frames count as anomalies even if their physics_flags are 0 (the 2020
    // TLE is stale at wall-clock now, so other frames may flag below-horizon too).
    assert!(m["gateway"]["anomaly_frames"].as_u64().unwrap() >= 2, "{m}");
}

#[tokio::test]
async fn cuc_time_codes_are_decoded_in_lab_mode() {
    let station = StationConfig::default();
    let (t, state) = find_time(&station, |el| el >= 20.0);
    let gw = start_gateway("format = \"cuc\"\nmax_skew_s = 0.0\ncuc_tai_minus_utc_s = 37").await;
    let mut ws = connect_ws(&gw).await;
    let tx = StdUdp::bind("127.0.0.1:0").unwrap();

    let pkt = encode_synthetic_tm_with_time(0x100, 1, &encode_cuc(t, 37).unwrap(), b"x");
    let v = exchange(&gw, &mut ws, &tx, None, &pkt).await;
    let got: DateTime<Utc> = v["physics_time"].as_str().unwrap().parse().unwrap();
    assert!((got - t).num_milliseconds().abs() <= 1, "{got} vs {t}");
    assert!(
        (v["elevation_deg"].as_f64().unwrap() - state.elevation_deg).abs() < 0.01,
        "{v}"
    );
    assert_eq!(v["physics_flags_v2"], 0);
}
