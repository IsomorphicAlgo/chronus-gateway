//! HTTP + WebSocket distribution (Milestone 5).
//!
//! Serves:
//! - `GET /health` — liveness for orchestration.
//! - `GET /telemetry/openmct` — WebSocket upgrade; each CCSDS TM frame is one JSON text message
//!   (`chronus_schema: "openmct.realtime.v1"`) suitable for a thin Open MCT plugin or external
//!   bridge (OD-B: Axum + documented JSON contract).
//! - `GET /api/v1/chronus/metrics` — combined UDP ingest + gateway counters (Milestone 6).
//! - `GET /api/v1/chronus/status` — plain-English health report: overall status, a one-line
//!   headline, and findings with *why* and *how to fix* (see [`crate::diagnose`]).
//! - `GET /api/v1/chronus/history` — stub (empty); future persistence / query API.
//! - `GET /api/v1/chronus/openmct/dictionary` — stub dictionary of telemetry point identifiers.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use chrono::Utc;
use serde::Serialize;
use tower_http::trace::TraceLayer;

use crate::ccsds;
use crate::ingest::RawFrame;
use crate::metrics::GatewayMetricsSnapshot;
use crate::propagator::TrackingState;
use crate::state::SharedGateway;
use crate::validate::{RfMetadata, apply_physics_validation};

/// JSON envelope for each WebSocket text message (one line per telemetry frame).
///
/// `physics_flags` semantics and extension policy: **`Methodology.md` D-016** (CV-0 charter).
/// If more than eight independent alarms are needed, add a new field (e.g. `physics_flags_v2`)
/// rather than repurposing reserved bits.
#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct OpenMctRealtimeMessageV1 {
    /// Contract identifier for adapters.
    pub chronus_schema: &'static str,
    /// CCSDS APID of the packet.
    pub apid: u16,
    /// CCSDS packet sequence count (14 bit).
    pub seq_count: u16,
    /// Gateway receive time (UTC), the clock the physics checks are evaluated at.
    pub received_at: chrono::DateTime<chrono::Utc>,
    /// Bitfield per D-016 / `validate` module docs (M4 + CV-1…CV-5 for assigned bits).
    pub physics_flags: u8,
    /// Sender `ip:port` of the UDP datagram.
    pub source: String,
    /// Geometric elevation (degrees); `None` without physics.
    pub elevation_deg: Option<f64>,
    /// Azimuth (degrees clockwise from true north); `None` without physics.
    pub azimuth_deg: Option<f64>,
    /// Slant range (km); `None` without physics.
    pub range_km: Option<f64>,
    /// Line-of-sight range rate (km/s, positive receding); `None` without physics.
    pub range_rate_km_s: Option<f64>,
    /// Apparent (refracted) elevation the antenna should point at, when the station models
    /// refraction (D-042). Additive field (schema v1 unchanged).
    pub apparent_elevation_deg: Option<f64>,
    /// Doppler acceptance band actually applied to this frame (Hz): **T-DOPPLER** plus any
    /// TLE-age margin (D-044). `None` without physics. Additive field (schema v1 unchanged).
    pub doppler_tolerance_hz: Option<f64>,
    /// Base64 of the CCSDS packet data field (secondary header + user data).
    pub payload_base64: String,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct HistoryStubResponse {
    note: &'static str,
    packets: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct DictionaryStubResponse {
    note: &'static str,
    points: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct CombinedMetricsResponse {
    pub ingest: crate::ingest::IngestSnapshot,
    pub gateway: GatewayMetricsSnapshot,
    pub avg_processing_latency_ms: Option<f64>,
}

/// Builds the Axum [`Router`] with shared [`SharedGateway`] state.
pub fn router(state: Arc<SharedGateway>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/chronus/metrics", get(metrics))
        .route("/api/v1/chronus/status", get(status))
        .route("/api/v1/chronus/history", get(history_stub))
        .route("/api/v1/chronus/openmct/dictionary", get(dictionary_stub))
        .route("/telemetry/openmct", get(openmct_ws))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn metrics(State(state): State<Arc<SharedGateway>>) -> Json<CombinedMetricsResponse> {
    let ingest = state.ingest_stats.snapshot_struct();
    let gateway = state.gateway_metrics.snapshot();
    let avg = gateway.avg_processing_latency_ms();
    Json(CombinedMetricsResponse {
        ingest,
        gateway,
        avg_processing_latency_ms: avg,
    })
}

async fn status(State(state): State<Arc<SharedGateway>>) -> Json<crate::diagnose::HealthReport> {
    Json(state.health_report())
}

async fn history_stub() -> (StatusCode, Json<HistoryStubResponse>) {
    (
        StatusCode::OK,
        Json(HistoryStubResponse {
            note: "stub: persistent history not implemented; use real-time WebSocket stream",
            packets: Vec::new(),
        }),
    )
}

async fn dictionary_stub() -> Json<DictionaryStubResponse> {
    Json(DictionaryStubResponse {
        note: "stub: expand with Open MCT telemetry keys when the official dictionary is wired",
        points: vec![
            "chronus.tm.apid",
            "chronus.tm.seq",
            "chronus.tm.physics_flags",
            "chronus.physics.elevation_deg",
            "chronus.physics.azimuth_deg",
        ],
    })
}

async fn openmct_ws(
    ws: WebSocketUpgrade,
    State(state): State<Arc<SharedGateway>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_openmct_socket(socket, state))
}

async fn handle_openmct_socket(mut socket: WebSocket, state: Arc<SharedGateway>) {
    state
        .gateway_metrics
        .ws_clients_connected
        .fetch_add(1, Ordering::Relaxed);
    let mut rx = state.frame_tx.subscribe();

    loop {
        tokio::select! {
            biased;
            recv = rx.recv() => {
                match recv {
                    Ok(frame) => {
                        if let Some(json) = process_frame(&state, &frame) {
                            // axum 0.8: `Message::Text` carries `Utf8Bytes` (cheap byte-buffer text).
                            if socket.send(Message::Text(json.into())).await.is_err() {
                                break;
                            }
                            state.gateway_metrics.ws_messages_sent.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            inc = socket.recv() => {
                match inc {
                    None => break,
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(Message::Ping(p))) => {
                        if socket.send(Message::Pong(p)).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }

    state
        .gateway_metrics
        .ws_clients_connected
        .fetch_sub(1, Ordering::Relaxed);
}

/// Parses, validates physics, and serializes one raw frame; updates metrics. Returns `None` on
/// parse failure (bad datagrams are counted, not sent on the WebSocket).
///
/// Public so embedders (e.g. the per-visitor demo sandbox) can drive the *same* parse → co-validate
/// → Open MCT JSON path without a UDP socket.
pub fn process_frame(state: &SharedGateway, frame: &RawFrame) -> Option<String> {
    process_frame_with_rf(state, frame, RfMetadata::default())
}

/// Like [`process_frame`], with receiver-chain measurements (`rf`) supplied alongside the frame so
/// the Doppler, link-budget and pointing co-validation checks can run.
///
/// Over the wire the gateway has no side channel for these yet (see `validate` module docs);
/// lab and demo drivers pass them in directly.
pub fn process_frame_with_rf(
    state: &SharedGateway,
    frame: &RawFrame,
    rf: RfMetadata,
) -> Option<String> {
    let mut tm = match ccsds::parse_telemetry(frame) {
        Ok(tm) => tm,
        Err(e) => {
            let prior = state
                .gateway_metrics
                .telemetry_parse_errors
                .fetch_add(1, Ordering::Relaxed);
            // Explain the *first* rejected packet in full; afterwards only count (no log spam).
            if prior == 0 {
                tracing::warn!(
                    "first invalid packet from {}:\n{}",
                    frame.source,
                    crate::diagnose::Explain::explain(&e)
                );
            } else {
                tracing::debug!(error = %e, "invalid packet dropped");
            }
            return None;
        }
    };

    let station = state.station.as_ref();
    // F-4: a propagator failure degrades to a physics-free frame (never drops it), but is
    // counted so operators can tell "physics off" from "physics failing" (e.g. stale TLE).
    let physics: Option<TrackingState> = match state.tracking.as_ref() {
        None => None,
        Some(t) => match t.tracking_state(tm.received_at) {
            Ok(s) => Some(s),
            Err(e) => {
                let prior = state
                    .gateway_metrics
                    .tracking_errors
                    .fetch_add(1, Ordering::Relaxed);
                if prior == 0 {
                    tracing::warn!("{}", crate::diagnose::explain_tracking_error(&e));
                } else {
                    tracing::debug!(error = %e, "propagator failed; frame emitted without physics fields");
                }
                None
            }
        },
    };

    if let Some(s) = physics.as_ref() {
        apply_physics_validation(&state.validation, &mut tm, s, rf);
    }

    if tm.physics_flags != 0 {
        state
            .gateway_metrics
            .anomaly_frames
            .fetch_add(1, Ordering::Relaxed);
    }

    let now = Utc::now();
    let latency_ms = (now - tm.received_at).num_milliseconds().max(0) as u64;

    let payload_b64 = base64::engine::general_purpose::STANDARD.encode(tm.payload());
    let (el, az, rk, rr) = match physics {
        Some(ref s) => (
            Some(s.elevation_deg),
            Some(s.azimuth_deg),
            Some(s.range_km),
            Some(s.range_rate_km_s),
        ),
        None => (None, None, None, None),
    };
    let apparent_el = physics
        .as_ref()
        .map(|s| s.apparent_elevation_deg)
        .filter(|x| x.is_finite());
    let doppler_tol = physics.as_ref().map(|s| {
        crate::validate::effective_doppler_tolerance_hz(
            station.tolerances.doppler_tolerance_hz,
            station.site.nominal_carrier_hz,
            s,
        )
    });

    let msg = OpenMctRealtimeMessageV1 {
        chronus_schema: "openmct.realtime.v1",
        apid: tm.apid,
        seq_count: tm.seq_count,
        received_at: tm.received_at,
        physics_flags: tm.physics_flags,
        source: tm.source.to_string(),
        elevation_deg: el,
        azimuth_deg: az,
        range_km: rk,
        range_rate_km_s: rr,
        apparent_elevation_deg: apparent_el,
        doppler_tolerance_hz: doppler_tol,
        payload_base64: payload_b64,
    };

    // F-3: serialization of this struct is practically infallible, but if it ever fails the
    // drop is counted rather than silent.
    let json = match serde_json::to_string(&msg) {
        Ok(j) => j,
        Err(e) => {
            state
                .gateway_metrics
                .serialize_errors
                .fetch_add(1, Ordering::Relaxed);
            tracing::warn!(error = %e, "telemetry JSON serialization failed; frame dropped");
            return None;
        }
    };
    state
        .gateway_metrics
        .telemetry_frames_emitted
        .fetch_add(1, Ordering::Relaxed);
    state
        .gateway_metrics
        .processing_latency_ms_sum
        .fetch_add(latency_ms, Ordering::Relaxed);
    state
        .gateway_metrics
        .processing_latency_ms_count
        .fetch_add(1, Ordering::Relaxed);
    Some(json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StationConfig;
    use crate::ingest::IngestStats;
    use crate::metrics::GatewayMetrics;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use std::net::SocketAddr;
    use tokio::sync::broadcast;
    use tower::ServiceExt;

    fn gateway() -> Arc<SharedGateway> {
        let (tx, _rx) = broadcast::channel(4);
        Arc::new(SharedGateway::new(
            tx,
            Arc::new(IngestStats::default()),
            Arc::new(GatewayMetrics::default()),
            Arc::new(StationConfig::default()),
            None,
        ))
    }

    fn raw(bytes: Vec<u8>) -> RawFrame {
        RawFrame {
            bytes: Arc::from(bytes.as_slice()),
            received_at: Utc::now(),
            source: SocketAddr::from(([127, 0, 0, 1], 4242)),
        }
    }

    async fn get_json(app: Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let res = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .expect("response");
        let status = res.status();
        let body = to_bytes(res.into_body(), 1 << 20).await.expect("body");
        (status, serde_json::from_slice(&body).expect("json body"))
    }

    #[test]
    fn process_frame_emits_openmct_json_for_valid_tm() {
        let g = gateway();
        let json = process_frame(&g, &raw(ccsds::encode_synthetic_tm(42, 7, b"hi")))
            .expect("valid TM is emitted");
        let v: serde_json::Value = serde_json::from_str(&json).expect("json");
        assert_eq!(v["chronus_schema"], "openmct.realtime.v1");
        assert_eq!(v["apid"], 42);
        assert_eq!(v["seq_count"], 7);
        assert_eq!(v["payload_base64"], "aGk=");
        assert_eq!(v["source"], "127.0.0.1:4242");
        // No tracking configured -> physics fields are null, not missing.
        assert!(v["elevation_deg"].is_null());
        assert_eq!(g.gateway_metrics.snapshot().telemetry_frames_emitted, 1);
    }

    #[test]
    fn process_frame_counts_and_drops_garbage() {
        let g = gateway();
        assert!(process_frame(&g, &raw(vec![1, 2])).is_none());
        assert!(process_frame(&g, &raw(vec![0xFF; 3])).is_none());
        let s = g.gateway_metrics.snapshot();
        assert_eq!(s.telemetry_parse_errors, 2);
        assert_eq!(s.telemetry_frames_emitted, 0);
    }

    #[test]
    fn process_frame_records_latency_sample() {
        let g = gateway();
        process_frame(&g, &raw(ccsds::encode_synthetic_tm(1, 1, b"x"))).expect("emit");
        let s = g.gateway_metrics.snapshot();
        assert_eq!(s.processing_latency_ms_count, 1);
        assert!(s.avg_processing_latency_ms().is_some());
    }

    #[tokio::test]
    async fn health_route_says_ok() {
        let (code, v) = get_json(router(gateway()), "/health").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(v["status"], "ok");
    }

    #[tokio::test]
    async fn metrics_route_combines_ingest_and_gateway() {
        let (code, v) = get_json(router(gateway()), "/api/v1/chronus/metrics").await;
        assert_eq!(code, StatusCode::OK);
        assert!(v["ingest"]["frames_received"].is_u64());
        assert!(v["gateway"]["tracking_errors"].is_u64());
        assert!(v["avg_processing_latency_ms"].is_null());
    }

    #[tokio::test]
    async fn status_route_explains_in_plain_english() {
        let g = gateway();
        let (code, v) = get_json(router(Arc::clone(&g)), "/api/v1/chronus/status").await;
        assert_eq!(code, StatusCode::OK);
        // No tracking in the test gateway -> warning, plus the waiting-for-data note.
        assert_eq!(v["status"], "attention");
        assert!(v["headline"].as_str().unwrap().len() > 10);
        let codes: Vec<_> = v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["code"].as_str().unwrap().to_string())
            .collect();
        assert!(codes.contains(&"CG-506".to_string()));
        assert!(codes.contains(&"CG-501".to_string()));
        assert!(!v["findings"][0]["fixes"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn stub_routes_respond() {
        let (code, v) = get_json(router(gateway()), "/api/v1/chronus/history").await;
        assert_eq!(code, StatusCode::OK);
        assert!(v["packets"].as_array().unwrap().is_empty());
        let (_, v) = get_json(router(gateway()), "/api/v1/chronus/openmct/dictionary").await;
        assert!(v["points"].as_array().unwrap().len() >= 5);
    }

    #[tokio::test]
    async fn unknown_route_is_404() {
        let res = router(gateway())
            .oneshot(Request::builder().uri("/nope").body(Body::empty()).unwrap())
            .await
            .expect("response");
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
