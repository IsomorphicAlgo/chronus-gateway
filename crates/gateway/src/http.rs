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
//! - `GET /api/v1/chronus/history?start=&end=&apid=&limit=` — recent `openmct.realtime.v1`
//!   messages from the history ring ([`crate::history`], schema `chronus.history.v1`).
//! - `GET /api/v1/chronus/openmct/dictionary` — the Open MCT telemetry dictionary generated from
//!   [`crate::openmct::POINTS`].
//! - `GET /api/v1/chronus/openmct/history/{key}?start=&end=` — one point's history as Open MCT
//!   datums (`[{"id","timestamp","value"}]`).
//! - `GET /api/v1/chronus/schemas` and `/schemas/{name}` — the published JSON Schemas.
//!
//! Since 0.5.0 the WebSocket relays messages produced once per packet by the
//! [`pipeline`](crate::pipeline); a server built from [`router`] must also run
//! [`crate::pipeline::spawn`].

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use chrono::Utc;
use serde::{Deserialize, Serialize};
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
    /// Extended co-validation bits (D-016 / D-047): bit 0 time-tag skew, bit 1 time code invalid.
    /// Additive field (schema v1 unchanged); `0` when spacecraft time decoding is off.
    pub physics_flags_v2: u8,
    /// Spacecraft time decoded from the packet's secondary header, when `time.format` is set and
    /// the packet carried one. Additive field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onboard_time: Option<chrono::DateTime<chrono::Utc>>,
    /// The instant physics was evaluated at: onboard time when trusted, else `received_at`.
    /// Additive field.
    pub physics_time: chrono::DateTime<chrono::Utc>,
    /// Whether receiver measurements (`chronus.rf.v1` or caller-supplied) were available for the
    /// Doppler / link-budget / pointing checks on this frame. Additive field.
    pub rf_matched: bool,
    /// Virtual channel the packet arrived on, when the link carries TM Transfer Frames
    /// (`framing.mode = "tm_frames"`). Additive field (0.5.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcid: Option<u8>,
    /// Base64 of the CCSDS packet data field (secondary header + user data).
    pub payload_base64: String,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct CombinedMetricsResponse {
    pub ingest: crate::ingest::IngestSnapshot,
    pub gateway: GatewayMetricsSnapshot,
    pub rf: crate::rf::RfSnapshot,
    pub framing: crate::frames::FramingSnapshot,
    pub history: crate::history::HistorySnapshot,
    pub pipeline: crate::pipeline::PipelineSnapshot,
    pub avg_processing_latency_ms: Option<f64>,
}

/// The published JSON Schemas: `(name, document)`.
pub const SCHEMAS: &[(&str, &str)] = &[
    (
        "openmct.realtime.v1",
        include_str!("../schemas/openmct.realtime.v1.schema.json"),
    ),
    (
        "chronus.rf.v1",
        include_str!("../schemas/chronus.rf.v1.schema.json"),
    ),
    (
        "chronus.history.v1",
        include_str!("../schemas/chronus.history.v1.schema.json"),
    ),
    (
        "chronus.openmct.dictionary.v1",
        include_str!("../schemas/chronus.openmct.dictionary.v1.schema.json"),
    ),
];

/// Builds the Axum [`Router`] with shared [`SharedGateway`] state.
pub fn router(state: Arc<SharedGateway>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/chronus/metrics", get(metrics))
        .route("/api/v1/chronus/status", get(status))
        .route("/api/v1/chronus/history", get(history))
        .route("/api/v1/chronus/openmct/dictionary", get(dictionary))
        .route("/api/v1/chronus/openmct/history/{key}", get(point_history))
        .route("/api/v1/chronus/schemas", get(schema_list))
        .route("/api/v1/chronus/schemas/{name}", get(schema))
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
        rf: state.rf.snapshot(),
        framing: state.framing.snapshot(state.station.framing.mode),
        history: state.history.snapshot(),
        pipeline: state.pipeline.snapshot(),
        avg_processing_latency_ms: avg,
    })
}

async fn status(State(state): State<Arc<SharedGateway>>) -> Json<crate::diagnose::HealthReport> {
    Json(state.health_report())
}

/// Query string of the history endpoints. Times are RFC 3339 or milliseconds since the Unix epoch
/// (what Open MCT sends).
#[derive(Debug, Default, Deserialize)]
struct HistoryParams {
    start: Option<String>,
    end: Option<String>,
    apid: Option<String>,
    limit: Option<String>,
}

/// A JSON error body with the same plain-English shape as `/status` findings.
fn bad_request(status: StatusCode, problem: String, fix: &str) -> Response {
    (
        status,
        Json(serde_json::json!({"error": problem, "fix": fix})),
    )
        .into_response()
}

/// Query-parameter errors are ready-made `400` responses (boxed: a `Response` is large).
type ParamError = Box<Response>;

fn bad_param(status: StatusCode, problem: String, fix: &str) -> ParamError {
    Box::new(bad_request(status, problem, fix))
}

fn parse_time_param(name: &str, raw: Option<&str>) -> Result<Option<i64>, ParamError> {
    let Some(raw) = raw.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    if let Ok(ms) = raw.parse::<i64>() {
        return Ok(Some(ms));
    }
    // Open MCT may send fractional milliseconds.
    if let Ok(ms) = raw.parse::<f64>()
        && ms.is_finite()
        && ms.abs() < 1.0e15
    {
        return Ok(Some(ms as i64));
    }
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|t| Some(t.timestamp_millis()))
        .map_err(|_| {
            bad_param(
                StatusCode::BAD_REQUEST,
                format!("`{name}={raw}` is not a time."),
                "Use milliseconds since 1970-01-01 (e.g. 1792843200000) or RFC 3339 (e.g. 2026-10-24T12:00:00Z).",
            )
        })
}

fn parse_query(p: &HistoryParams) -> Result<crate::history::HistoryQuery, ParamError> {
    let start = parse_time_param("start", p.start.as_deref())?;
    let end = parse_time_param("end", p.end.as_deref())?;
    if let (Some(s), Some(e)) = (start, end)
        && s > e
    {
        return Err(bad_param(
            StatusCode::BAD_REQUEST,
            "`start` is after `end`.".into(),
            "Swap the two times.",
        ));
    }
    let apid = match p.apid.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        None => None,
        Some(a) => {
            let parsed = a
                .strip_prefix("0x")
                .or_else(|| a.strip_prefix("0X"))
                .map_or_else(
                    || a.parse::<u16>().ok(),
                    |h| u16::from_str_radix(h, 16).ok(),
                );
            match parsed.filter(|&v| v <= crate::hil_tm::CCSDS_APID_MAX) {
                Some(v) => Some(v),
                None => {
                    return Err(bad_param(
                        StatusCode::BAD_REQUEST,
                        format!("`apid={a}` is not an APID."),
                        "Use a number from 0 to 2047 (decimal or 0x-prefixed hex).",
                    ));
                }
            }
        }
    };
    let limit = match p.limit.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
        None => 0,
        Some(l) => l.parse::<usize>().map_err(|_| {
            bad_param(
                StatusCode::BAD_REQUEST,
                format!("`limit={l}` is not a count."),
                "Use a whole number; at most 10000 messages are returned per request.",
            )
        })?,
    };
    Ok(crate::history::HistoryQuery::new()
        .between(start, end)
        .with_apid(apid)
        .with_limit(limit))
}

async fn history(
    State(state): State<Arc<SharedGateway>>,
    Query(params): Query<HistoryParams>,
) -> Response {
    let q = match parse_query(&params) {
        Ok(q) => q,
        Err(r) => return *r,
    };
    let page = state.history.query(&q);
    // The messages are already JSON text: splice them in instead of re-encoding.
    let mut body =
        String::with_capacity(256 + page.entries.iter().map(|e| e.json.len() + 1).sum::<usize>());
    let head = serde_json::json!({
        "chronus_schema": crate::history::HISTORY_SCHEMA_V1,
        "start": q.start_ms,
        "end": q.end_ms,
        "apid": q.apid,
        "count": page.entries.len(),
        "truncated": page.truncated,
        "capacity": state.history.snapshot().capacity,
    })
    .to_string();
    body.push_str(&head[..head.len() - 1]);
    body.push_str(",\"messages\":[");
    for (i, e) in page.entries.iter().enumerate() {
        if i > 0 {
            body.push(',');
        }
        body.push_str(&e.json);
    }
    body.push_str("]}");
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

async fn dictionary(State(state): State<Arc<SharedGateway>>) -> Json<serde_json::Value> {
    Json(crate::openmct::dictionary(&state.station))
}

/// One Open MCT datum.
#[derive(Debug, Serialize)]
struct Datum<'a> {
    id: &'a str,
    timestamp: i64,
    value: f64,
}

async fn point_history(
    State(state): State<Arc<SharedGateway>>,
    Path(key): Path<String>,
    Query(params): Query<HistoryParams>,
) -> Response {
    let Some(index) = crate::openmct::point_index(&key) else {
        let known: Vec<&str> = crate::openmct::POINTS.iter().map(|p| p.key).collect();
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("There is no telemetry point `{key}`."),
                "fix": "Use one of the keys from GET /api/v1/chronus/openmct/dictionary.",
                "points": known,
            })),
        )
            .into_response();
    };
    let mut q = match parse_query(&params) {
        Ok(q) => q,
        Err(r) => return *r,
    };
    if q.limit == 0 {
        q = q.with_limit(crate::history::MAX_QUERY_LIMIT);
    }
    let page = state.history.query(&q.with_point(Some(index)));
    let key = crate::openmct::POINTS[index].key;
    let data: Vec<Datum<'_>> = page
        .entries
        .iter()
        .filter_map(|e| {
            Some(Datum {
                id: key,
                timestamp: e.received_at_ms,
                value: e.values.get(index).copied().flatten()?,
            })
        })
        .collect();
    Json(data).into_response()
}

async fn schema_list() -> Json<serde_json::Value> {
    let names: Vec<serde_json::Value> = SCHEMAS
        .iter()
        .map(|(name, _)| {
            serde_json::json!({"name": name, "url": format!("/api/v1/chronus/schemas/{name}")})
        })
        .collect();
    Json(serde_json::json!({"schemas": names}))
}

async fn schema(Path(name): Path<String>) -> Response {
    let name = name
        .strip_suffix(".schema.json")
        .or_else(|| name.strip_suffix(".json"))
        .unwrap_or(&name);
    match SCHEMAS.iter().find(|(n, _)| *n == name) {
        Some((_, doc)) => {
            ([(header::CONTENT_TYPE, "application/schema+json")], *doc).into_response()
        }
        None => bad_request(
            StatusCode::NOT_FOUND,
            format!("There is no schema `{name}`."),
            "GET /api/v1/chronus/schemas lists the published schemas.",
        ),
    }
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
    // The pipeline processes each packet once; clients only relay the published JSON.
    let mut rx = state.telemetry_tx.subscribe();

    loop {
        tokio::select! {
            biased;
            recv = rx.recv() => {
                match recv {
                    Ok(json) => {
                        // axum 0.8: `Message::Text` carries `Utf8Bytes` (cheap byte-buffer text).
                        if socket.send(Message::Text(json.as_ref().into())).await.is_err() {
                            break;
                        }
                        state.gateway_metrics.ws_messages_sent.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        state.pipeline.ws_messages_dropped.fetch_add(n, Ordering::Relaxed);
                        continue;
                    }
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
///
/// Receiver measurements come from the `chronus.rf.v1` side channel
/// ([`SharedGateway::rf`]): the sample nearest the frame's receive time, within the configured
/// window. With the side channel disabled, the RF-dependent checks are skipped.
pub fn process_frame(state: &SharedGateway, frame: &RawFrame) -> Option<String> {
    process_inner(state, frame, None, None)
}

/// [`process_frame`] for a packet extracted from virtual channel `vcid` of a TM frame stream
/// (used by the [`pipeline`](crate::pipeline)).
pub(crate) fn process_packet(
    state: &SharedGateway,
    frame: &RawFrame,
    vcid: Option<u8>,
) -> Option<String> {
    process_inner(state, frame, None, vcid)
}

/// Like [`process_frame`], with receiver-chain measurements (`rf`) supplied alongside the frame so
/// the Doppler, link-budget and pointing co-validation checks can run.
///
/// Over the wire these arrive on the `chronus.rf.v1` side channel ([`process_frame`] consults it);
/// this entry point bypasses the store so lab and demo drivers can pass them in directly.
pub fn process_frame_with_rf(
    state: &SharedGateway,
    frame: &RawFrame,
    rf: RfMetadata,
) -> Option<String> {
    process_inner(state, frame, Some(rf), None)
}

fn process_inner(
    state: &SharedGateway,
    frame: &RawFrame,
    explicit_rf: Option<RfMetadata>,
    vcid: Option<u8>,
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
                if frame.bytes.starts_with(&crate::frames::ASM) {
                    tracing::warn!(
                        "that datagram starts with the CCSDS sync marker 1ACFFC1D: the sender emits \
                         TM Transfer Frames. Set [framing] mode = \"tm_frames\" and asm = true \
                         (docs/TM_TRANSFER_FRAMES.md)."
                    );
                }
            } else {
                tracing::debug!(error = %e, "invalid packet dropped");
            }
            return None;
        }
    };

    let station = state.station.as_ref();
    // Tranche 2: physics runs at the spacecraft's own time when it is present and trusted.
    let physics_time = crate::time::resolve_frame_time(&station.time, &mut tm);
    let rf = explicit_rf
        .or_else(|| state.rf.lookup(tm.received_at))
        .unwrap_or_default();
    // F-4: a propagator failure degrades to a physics-free frame (never drops it), but is
    // counted so operators can tell "physics off" from "physics failing" (e.g. stale TLE).
    let physics: Option<TrackingState> = match state.tracking.as_ref() {
        None => None,
        Some(t) => match t.tracking_state(physics_time) {
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

    if tm.physics_flags != 0 || tm.physics_flags_v2 != 0 {
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
        physics_flags_v2: tm.physics_flags_v2,
        onboard_time: tm.onboard_time,
        physics_time,
        rf_matched: !rf.is_empty(),
        vcid,
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
    async fn empty_history_and_dictionary_respond() {
        let (code, v) = get_json(router(gateway()), "/api/v1/chronus/history").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(v["chronus_schema"], "chronus.history.v1");
        assert!(v["messages"].as_array().unwrap().is_empty());
        assert_eq!(v["capacity"], 10_000);
        let (_, v) = get_json(router(gateway()), "/api/v1/chronus/openmct/dictionary").await;
        assert_eq!(
            v["measurements"].as_array().unwrap().len(),
            crate::openmct::POINTS.len()
        );
    }

    #[tokio::test]
    async fn unknown_route_is_404() {
        let res = router(gateway())
            .oneshot(Request::builder().uri("/nope").body(Body::empty()).unwrap())
            .await
            .expect("response");
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    // ---- Tranche 2: RF side channel + spacecraft time ------------------------------------

    fn gateway_with(configure: impl FnOnce(&mut StationConfig)) -> Arc<SharedGateway> {
        let mut station = StationConfig::default();
        configure(&mut station);
        let tracking = Arc::new(crate::propagator::TrackingProvider::new(
            crate::propagator::propagator_from_station(&station).expect("propagator"),
            0,
        ));
        let (tx, _rx) = broadcast::channel(4);
        Arc::new(SharedGateway::new(
            tx,
            Arc::new(IngestStats::default()),
            Arc::new(GatewayMetrics::default()),
            Arc::new(station),
            Some(tracking),
        ))
    }

    fn json_of(g: &SharedGateway, frame: &RawFrame) -> serde_json::Value {
        serde_json::from_str(&process_frame(g, frame).expect("emitted")).expect("json")
    }

    #[test]
    fn process_frame_reports_v2_fields_even_without_time_decoding() {
        let g = gateway();
        let v = json_of(&g, &raw(ccsds::encode_synthetic_tm(1, 1, b"x")));
        assert_eq!(v["physics_flags_v2"], 0);
        assert_eq!(v["rf_matched"], false);
        assert!(v.get("onboard_time").is_none());
        assert_eq!(v["physics_time"], v["received_at"]);
    }

    #[test]
    fn rf_store_sample_is_used_by_process_frame_and_explicit_rf_bypasses_it() {
        let g = gateway_with(|s| {
            s.rf.bind_addr = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
            s.rf.window_ms = 500;
        });
        let frame = raw(ccsds::encode_synthetic_tm(1, 1, b"x"));
        // A wildly wrong carrier stored near the frame time raises the Doppler bit...
        g.rf.insert(frame.received_at, RfMetadata::new().with_carrier_hz(1.0e6));
        let v = json_of(&g, &frame);
        assert_eq!(v["rf_matched"], true);
        assert_eq!(v["physics_flags"].as_u64().unwrap() & 1, 1, "{v}");
        // ...but explicit metadata (lab drivers) bypasses the store entirely.
        let explicit = process_frame_with_rf(&g, &frame, RfMetadata::new()).expect("emit");
        let v: serde_json::Value = serde_json::from_str(&explicit).unwrap();
        assert_eq!(v["rf_matched"], false);
        assert_eq!(v["physics_flags"].as_u64().unwrap() & 1, 0);
        let snap = g.rf.snapshot();
        assert_eq!((snap.frames_matched, snap.frames_unmatched), (1, 0));
    }

    #[test]
    fn disabled_side_channel_never_matches_even_if_filled() {
        let g = gateway_with(|_| {});
        let frame = raw(ccsds::encode_synthetic_tm(1, 1, b"x"));
        g.rf.insert(frame.received_at, RfMetadata::new().with_carrier_hz(1.0e6));
        let v = json_of(&g, &frame);
        assert_eq!(v["rf_matched"], false);
    }

    #[test]
    fn onboard_time_drives_physics_and_skew_falls_back_to_receive_time() {
        use crate::time::{TimeFormat, encode_cds};
        let g = gateway_with(|s| {
            s.time.format = TimeFormat::Cds;
            s.time.max_skew_s = 5.0;
        });
        let rx = Utc::now();
        let near = rx + chrono::Duration::milliseconds(300);
        let pkt = ccsds::encode_synthetic_tm_with_time(7, 1, &encode_cds(near).unwrap(), b"hi");
        let frame = RawFrame {
            bytes: Arc::from(pkt.as_slice()),
            received_at: rx,
            source: SocketAddr::from(([127, 0, 0, 1], 1)),
        };
        let v = json_of(&g, &frame);
        assert_eq!(v["physics_flags_v2"], 0);
        assert!(v["onboard_time"].is_string());
        let physics: chrono::DateTime<Utc> = v["physics_time"].as_str().unwrap().parse().unwrap();
        assert!((physics - near).num_milliseconds().abs() <= 1, "{v}");

        let far = rx - chrono::Duration::hours(1);
        let pkt = ccsds::encode_synthetic_tm_with_time(7, 2, &encode_cds(far).unwrap(), b"hi");
        let frame = RawFrame {
            bytes: Arc::from(pkt.as_slice()),
            received_at: rx,
            source: SocketAddr::from(([127, 0, 0, 1], 1)),
        };
        let v = json_of(&g, &frame);
        assert_eq!(v["physics_flags_v2"], 1, "time-tag skew bit");
        assert_eq!(v["physics_time"], v["received_at"]);
        assert!(g.gateway_metrics.snapshot().anomaly_frames >= 1);
    }

    #[tokio::test]
    async fn metrics_route_exposes_rf_counters() {
        let g = gateway_with(|s| s.rf.bind_addr = Some(SocketAddr::from(([127, 0, 0, 1], 0))));
        g.rf.insert(Utc::now(), RfMetadata::new().with_carrier_hz(1.0));
        let (_, v) = get_json(router(Arc::clone(&g)), "/api/v1/chronus/metrics").await;
        assert_eq!(v["rf"]["enabled"], true);
        assert_eq!(v["rf"]["samples_received"], 1);
    }

    // ---- Tranche 3: history, dictionary, schemas -------------------------------------------

    fn record(g: &SharedGateway, apid: u16, at: chrono::DateTime<Utc>) {
        let frame = RawFrame {
            bytes: Arc::from(ccsds::encode_synthetic_tm(apid, 1, b"x").as_slice()),
            received_at: at,
            source: SocketAddr::from(([127, 0, 0, 1], 1)),
        };
        crate::pipeline::handle_datagram(g, None, &frame);
    }

    #[tokio::test]
    async fn history_route_filters_and_returns_raw_messages() {
        let g = gateway_with(|_| {});
        let t0 = chrono::DateTime::parse_from_rfc3339("2026-10-24T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        for i in 0..6 {
            record(
                &g,
                0x100 + (i % 2),
                t0 + chrono::Duration::seconds(i.into()),
            );
        }
        let (code, v) = get_json(router(Arc::clone(&g)), "/api/v1/chronus/history").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(v["count"], 6);
        assert_eq!(v["messages"][0]["chronus_schema"], "openmct.realtime.v1");
        assert!(
            v["messages"][0]["elevation_deg"].is_f64(),
            "physics included"
        );

        let start = t0.timestamp_millis() + 2000;
        let (_, v) = get_json(
            router(Arc::clone(&g)),
            &format!("/api/v1/chronus/history?start={start}&end=2026-10-24T12:00:04Z&apid=0x100"),
        )
        .await;
        assert_eq!(v["count"], 2, "{v}");
        assert!(
            v["messages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["apid"] == 0x100)
        );

        let (_, v) = get_json(router(Arc::clone(&g)), "/api/v1/chronus/history?limit=2").await;
        assert_eq!(
            (v["count"].as_u64(), v["truncated"].as_bool()),
            (Some(2), Some(true))
        );
    }

    #[tokio::test]
    async fn history_route_explains_bad_parameters() {
        for q in [
            "start=yesterday",
            "apid=4096",
            "apid=0xZZ",
            "limit=-1",
            "start=2026-10-24T12:00:05Z&end=2026-10-24T12:00:00Z",
        ] {
            let (code, v) =
                get_json(router(gateway()), &format!("/api/v1/chronus/history?{q}")).await;
            assert_eq!(code, StatusCode::BAD_REQUEST, "{q}");
            assert!(v["error"].is_string() && v["fix"].is_string(), "{q}: {v}");
        }
    }

    #[tokio::test]
    async fn point_history_returns_openmct_datums() {
        let g = gateway_with(|_| {});
        let t0 = Utc::now();
        for i in 0..3 {
            record(&g, 7, t0 + chrono::Duration::milliseconds(i * 10));
        }
        let (code, v) = get_json(
            router(Arc::clone(&g)),
            "/api/v1/chronus/openmct/history/chronus.physics.elevation_deg",
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["id"], "chronus.physics.elevation_deg");
        assert_eq!(rows[0]["timestamp"], t0.timestamp_millis());
        assert!(rows[0]["value"].is_f64());

        // Without a TLE there is no elevation: the point simply has no data.
        let bare = gateway();
        record(&bare, 7, t0);
        let (_, v) = get_json(
            router(Arc::clone(&bare)),
            "/api/v1/chronus/openmct/history/chronus.physics.elevation_deg",
        )
        .await;
        assert!(v.as_array().unwrap().is_empty());
        let (_, v) = get_json(
            router(bare),
            "/api/v1/chronus/openmct/history/chronus.tm.apid",
        )
        .await;
        assert_eq!(v[0]["value"], 7.0);

        let (code, v) = get_json(router(gateway()), "/api/v1/chronus/openmct/history/nope").await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert!(v["points"].as_array().unwrap().len() >= 10);
    }

    #[tokio::test]
    async fn schemas_are_listed_and_served() {
        let (code, v) = get_json(router(gateway()), "/api/v1/chronus/schemas").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(v["schemas"].as_array().unwrap().len(), SCHEMAS.len());
        for (name, _) in SCHEMAS {
            for suffix in ["", ".json", ".schema.json"] {
                let (code, doc) = get_json(
                    router(gateway()),
                    &format!("/api/v1/chronus/schemas/{name}{suffix}"),
                )
                .await;
                assert_eq!(code, StatusCode::OK);
                assert!(doc["$schema"].is_string(), "{name}");
            }
        }
        let (code, _) = get_json(router(gateway()), "/api/v1/chronus/schemas/nope").await;
        assert_eq!(code, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn metrics_route_exposes_framing_history_and_pipeline() {
        let g = gateway();
        record(&g, 1, Utc::now());
        let (_, v) = get_json(router(Arc::clone(&g)), "/api/v1/chronus/metrics").await;
        assert_eq!(v["framing"]["mode"], "packets");
        assert_eq!(v["history"]["len"], 1);
        assert_eq!(v["pipeline"]["messages_published"], 1);
    }
}
