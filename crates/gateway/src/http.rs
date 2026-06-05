//! HTTP + WebSocket distribution (Milestone 5).
//!
//! Serves:
//! - `GET /health` — liveness for orchestration.
//! - `GET /telemetry/openmct` — WebSocket upgrade; each CCSDS TM frame is one JSON text message
//!   (`chronus_schema: "openmct.realtime.v1"`) suitable for a thin Open MCT plugin or external
//!   bridge (OD-B: Axum + documented JSON contract).
//! - `GET /api/v1/chronus/metrics` — combined UDP ingest + gateway counters (Milestone 6).
//! - `GET /api/v1/chronus/history` — stub (empty); future persistence / query API.
//! - `GET /api/v1/chronus/openmct/dictionary` — stub dictionary of telemetry point identifiers.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
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
use crate::validate::{apply_physics_validation, LinkBudgetStationParams, RfMetadata};

/// JSON envelope for each WebSocket text message (one line per telemetry frame).
///
/// `physics_flags` semantics and extension policy: **`Methodology.md` D-016** (CV-0 charter).
/// If more than eight independent alarms are needed, add a new field (e.g. `physics_flags_v2`)
/// rather than repurposing reserved bits.
#[derive(Debug, Serialize)]
pub struct OpenMctRealtimeMessageV1 {
    /// Contract identifier for adapters.
    pub chronus_schema: &'static str,
    pub apid: u16,
    pub seq_count: u16,
    pub received_at: chrono::DateTime<chrono::Utc>,
    /// Bitfield per D-016 / `validate` module docs (M4 + CV-1 + CV-2; CV-3…CV-4 planned).
    pub physics_flags: u8,
    pub source: String,
    pub elevation_deg: Option<f64>,
    pub azimuth_deg: Option<f64>,
    pub range_km: Option<f64>,
    pub range_rate_km_s: Option<f64>,
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
                            if socket.send(Message::Text(json)).await.is_err() {
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
fn process_frame(state: &SharedGateway, frame: &RawFrame) -> Option<String> {
    let mut tm = match ccsds::parse_telemetry(frame) {
        Ok(tm) => tm,
        Err(_) => {
            state
                .gateway_metrics
                .telemetry_parse_errors
                .fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };

    let station = state.station.as_ref();
    let physics: Option<TrackingState> = state
        .tracking
        .as_ref()
        .and_then(|t| t.tracking_state(tm.received_at).ok());

    if let Some(s) = physics.as_ref() {
        let link_budget = Some(LinkBudgetStationParams {
            tx_power_dbm: station.tx_power_dbm,
            tx_gain_dbi: station.tx_gain_dbi,
            rx_gain_dbi: station.rx_gain_dbi,
            tolerance_db: station.link_budget_tolerance_db,
        });
        apply_physics_validation(
            &mut tm,
            s,
            station.nominal_carrier_hz,
            RfMetadata::default(),
            station.doppler_tolerance_hz,
            station.minimum_elevation_deg,
            link_budget,
            station.pointing_tolerance_deg,
        );
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
        payload_base64: payload_b64,
    };

    let json = serde_json::to_string(&msg).ok()?;
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
