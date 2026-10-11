//! HTTP + WebSocket front end for the per-visitor sandbox, with the abuse limits a public
//! endpoint needs: global and per-IP session caps, an `Origin` allow-list, a hard session
//! lifetime, tiny inbound messages, a command rate limit and a slow-client send timeout.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, MissedTickBehavior};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::sandbox::{SCENARIOS, Scenario, SessionSim};

/// Runtime limits and wiring. Every field has an environment override (see [`ServerConfig::from_env`]).
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    /// Directory of static files (the dashboard build) served at `/`.
    pub static_dir: Option<PathBuf>,
    /// Allowed `Origin` values for the WebSocket. Empty = allow any (local development).
    pub allowed_origins: Vec<String>,
    /// Maximum simultaneous sessions across all visitors.
    pub max_sessions: usize,
    /// Maximum simultaneous sessions per client IP.
    pub max_per_ip: usize,
    /// Hard session lifetime, seconds.
    pub session_secs: u64,
    /// Frames per second per session (clamped to 1..=20).
    pub tick_hz: u32,
    /// Trust `Fly-Client-IP` for the client address (only safe behind Fly's proxy).
    pub trust_fly_header: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 8080)),
            static_dir: None,
            allowed_origins: Vec::new(),
            max_sessions: 40,
            max_per_ip: 3,
            session_secs: 600,
            tick_hz: 10,
            trust_fly_header: false,
        }
    }
}

impl ServerConfig {
    /// Reads configuration from the environment:
    /// `BIND` (default `0.0.0.0:$PORT` or `127.0.0.1:8080`), `STATIC_DIR`, `ALLOWED_ORIGINS`
    /// (comma-separated), `MAX_SESSIONS`, `MAX_PER_IP`, `SESSION_SECS`, `TICK_HZ`.
    /// `FLY_APP_NAME` (set by Fly.io) turns on `Fly-Client-IP` trust.
    #[must_use]
    pub fn from_env() -> Self {
        let mut c = Self::default();
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let num = |k: &str| var(k).and_then(|v| v.trim().parse::<u64>().ok());

        if let Some(b) = var("BIND").and_then(|v| v.parse().ok()) {
            c.bind = b;
        } else if let Some(p) = num("PORT") {
            c.bind = SocketAddr::from(([0, 0, 0, 0], p as u16));
        }
        c.static_dir = var("STATIC_DIR").map(PathBuf::from);
        if let Some(o) = var("ALLOWED_ORIGINS") {
            c.allowed_origins = o
                .split(',')
                .map(|s| s.trim().trim_end_matches('/').to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Some(n) = num("MAX_SESSIONS") {
            c.max_sessions = n as usize;
        }
        if let Some(n) = num("MAX_PER_IP") {
            c.max_per_ip = n as usize;
        }
        if let Some(n) = num("SESSION_SECS") {
            c.session_secs = n;
        }
        if let Some(n) = num("TICK_HZ") {
            c.tick_hz = n.clamp(1, 20) as u32;
        }
        c.trust_fly_header = var("FLY_APP_NAME").is_some();
        c
    }
}

/// Why a visitor was turned away.
#[derive(Debug, PartialEq, Eq)]
pub enum Reject {
    /// Server-wide session cap reached.
    Capacity,
    /// This client IP already holds its share of sessions.
    PerIp,
}

/// Shared limiter + counters. Cloning shares the same state.
#[derive(Clone)]
pub struct AppState {
    cfg: Arc<ServerConfig>,
    permits: Arc<Semaphore>,
    per_ip: Arc<Mutex<HashMap<IpAddr, usize>>>,
    active: Arc<AtomicUsize>,
    total: Arc<AtomicU64>,
}

/// RAII admission ticket: releases the global slot and the per-IP count on drop.
pub struct SessionGuard {
    _permit: OwnedSemaphorePermit,
    ip: IpAddr,
    per_ip: Arc<Mutex<HashMap<IpAddr, usize>>>,
    active: Arc<AtomicUsize>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
        if let Ok(mut m) = self.per_ip.lock()
            && let Some(n) = m.get_mut(&self.ip)
        {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.ip);
            }
        }
    }
}

impl AppState {
    #[must_use]
    pub fn new(cfg: ServerConfig) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(cfg.max_sessions)),
            cfg: Arc::new(cfg),
            per_ip: Arc::default(),
            active: Arc::default(),
            total: Arc::default(),
        }
    }

    /// Sessions currently open.
    #[must_use]
    pub fn active_sessions(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }

    /// Admits a visitor or says why not.
    pub fn admit(&self, ip: IpAddr) -> Result<SessionGuard, Reject> {
        let permit = Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(|_| Reject::Capacity)?;
        {
            let mut m = self.per_ip.lock().map_err(|_| Reject::Capacity)?;
            let n = m.entry(ip).or_insert(0);
            if *n >= self.cfg.max_per_ip {
                return Err(Reject::PerIp);
            }
            *n += 1;
        }
        self.active.fetch_add(1, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed);
        Ok(SessionGuard {
            _permit: permit,
            ip,
            per_ip: Arc::clone(&self.per_ip),
            active: Arc::clone(&self.active),
        })
    }
}

/// `true` if `origin` is acceptable under `allowed` (empty list = anything goes).
#[must_use]
pub fn origin_allowed(allowed: &[String], origin: Option<&str>) -> bool {
    if allowed.is_empty() {
        return true;
    }
    match origin {
        Some(o) => {
            let o = o.trim().trim_end_matches('/').to_ascii_lowercase();
            allowed.contains(&o)
        }
        None => false,
    }
}

/// Client address: `Fly-Client-IP` when trusted and present, else the socket peer.
fn client_ip(cfg: &ServerConfig, headers: &HeaderMap, peer: SocketAddr) -> IpAddr {
    if cfg.trust_fly_header
        && let Some(ip) = headers
            .get("fly-client-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
    {
        return ip;
    }
    peer.ip()
}

/// Builds the router. Serve it with
/// `into_make_service_with_connect_info::<SocketAddr>()` so per-IP limits see the peer address.
pub fn router(state: AppState) -> Router {
    let static_dir = state.cfg.static_dir.clone();
    let mut app = Router::new()
        .route("/health", get(health))
        .route("/api/v1/sandbox/scenarios", get(scenarios))
        .route("/sandbox/ws", get(sandbox_ws))
        .with_state(state);
    if let Some(dir) = static_dir {
        app = app.fallback_service(ServeDir::new(dir).append_index_html_on_directories(true));
    }
    app.layer(TraceLayer::new_for_http())
}

async fn health(State(app): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "active_sessions": app.active_sessions(),
        "max_sessions": app.cfg.max_sessions,
        "sessions_served": app.total.load(Ordering::Relaxed),
    }))
}

async fn scenarios() -> Json<serde_json::Value> {
    Json(json!({ "scenarios": SCENARIOS }))
}

async fn sandbox_ws(
    ws: WebSocketUpgrade,
    State(app): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if !origin_allowed(&app.cfg.allowed_origins, origin) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    let ip = client_ip(&app.cfg, &headers, peer);
    let guard = match app.admit(ip) {
        Ok(g) => g,
        Err(Reject::Capacity) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::RETRY_AFTER, "30")],
                "sandbox is full; try again shortly",
            )
                .into_response();
        }
        Err(Reject::PerIp) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, "30")],
                "too many sandboxes open from your address",
            )
                .into_response();
        }
    };
    let cfg = Arc::clone(&app.cfg);
    ws.max_message_size(1024)
        .max_frame_size(1024)
        .on_upgrade(move |socket| run_session(socket, cfg, guard))
}

/// Visitor → server control messages.
#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Command {
    Scenario { value: Scenario },
    Speed { value: f64 },
    HorizonMask { enabled: bool },
    Reset,
}

const SEND_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_COMMANDS_PER_SEC: u32 = 10;

async fn send(socket: &mut WebSocket, text: String) -> bool {
    matches!(
        tokio::time::timeout(SEND_TIMEOUT, socket.send(Message::Text(text.into()))).await,
        Ok(Ok(()))
    )
}

async fn run_session(mut socket: WebSocket, cfg: Arc<ServerConfig>, _guard: SessionGuard) {
    // Seed noise from the clock so concurrent visitors don't see identical jitter.
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    let mut sim = match SessionSim::new(seed) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "sandbox init failed");
            let _ = send(
                &mut socket,
                json!({"chronus_schema":"chronus.sandbox.v1","kind":"error","message":"sandbox failed to start"}).to_string(),
            )
            .await;
            return;
        }
    };

    let hello = json!({
        "chronus_schema": "chronus.sandbox.v1",
        "kind": "hello",
        "session_secs": cfg.session_secs,
        "tick_hz": cfg.tick_hz,
        "speed": sim.speed(),
        "scenarios": SCENARIOS,
        "note": "Synthetic data only: public ISS TLE, toy subsystem models.",
    });
    if !send(&mut socket, hello.to_string()).await {
        return;
    }

    let period = Duration::from_millis(1000 / u64::from(cfg.tick_hz.clamp(1, 20)));
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let started = Instant::now();
    let deadline = started + Duration::from_secs(cfg.session_secs);
    let mut last_tick = Instant::now();
    let mut cmd_window = Instant::now();
    let mut cmd_count = 0u32;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let now = Instant::now();
                let dt = now.duration_since(last_tick).as_secs_f64();
                last_tick = now;
                match sim.step(dt) {
                    Ok(out) => {
                        let left = deadline.saturating_duration_since(now).as_secs();
                        let tick = inject_seconds_left(&out.tick_json, left);
                        if !send(&mut socket, out.frame_json).await
                            || !send(&mut socket, tick).await
                        {
                            return;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "sandbox step failed; closing session");
                        let _ = send(&mut socket, json!({"chronus_schema":"chronus.sandbox.v1","kind":"error","message":"simulation error; please reconnect"}).to_string()).await;
                        return;
                    }
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                    Some(Ok(Message::Ping(p))) => {
                        if socket.send(Message::Pong(p)).await.is_err() { return; }
                    }
                    Some(Ok(Message::Text(t))) => {
                        if cmd_window.elapsed() >= Duration::from_secs(1) {
                            cmd_window = Instant::now();
                            cmd_count = 0;
                        }
                        cmd_count += 1;
                        if cmd_count > MAX_COMMANDS_PER_SEC { continue; }
                        match serde_json::from_str::<Command>(&t) {
                            Ok(Command::Scenario { value }) => sim.set_scenario(value),
                            Ok(Command::Speed { value }) => sim.set_speed(value),
                            Ok(Command::HorizonMask { enabled }) => {
                                if sim.set_horizon_mask(enabled).is_err() { return; }
                            }
                            Ok(Command::Reset) => sim.reset(),
                            Err(_) => {}
                        }
                    }
                    Some(Ok(_)) => {}
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                let _ = send(&mut socket, json!({
                    "chronus_schema": "chronus.sandbox.v1",
                    "kind": "ended",
                    "message": "Session time limit reached. Reload to start a fresh sandbox.",
                }).to_string()).await;
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
        }
    }
}

/// Adds `seconds_left` to an already-serialized tick object (cheap: it is a flat JSON object).
fn inject_seconds_left(tick_json: &str, left: u64) -> String {
    match serde_json::from_str::<serde_json::Value>(tick_json) {
        Ok(mut v) => {
            v["seconds_left"] = json!(left);
            v.to_string()
        }
        Err(_) => tick_json.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(max: usize, per_ip: usize) -> ServerConfig {
        ServerConfig {
            max_sessions: max,
            max_per_ip: per_ip,
            ..ServerConfig::default()
        }
    }

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, n])
    }

    #[test]
    fn origin_rules() {
        let allowed = vec!["https://demo.shintratech.net".to_string()];
        assert!(origin_allowed(
            &allowed,
            Some("https://demo.shintratech.net")
        ));
        assert!(origin_allowed(
            &allowed,
            Some("https://DEMO.shintratech.net/")
        ));
        assert!(!origin_allowed(&allowed, Some("https://evil.example")));
        assert!(
            !origin_allowed(&allowed, None),
            "no Origin is rejected when a list is set"
        );
        assert!(origin_allowed(&[], None), "empty list = open (dev)");
    }

    #[test]
    fn per_ip_and_global_caps_release_on_drop() {
        let app = AppState::new(cfg(3, 2));
        let a1 = app.admit(ip(1)).unwrap();
        let _a2 = app.admit(ip(1)).unwrap();
        assert_eq!(app.admit(ip(1)).err(), Some(Reject::PerIp));
        let _b1 = app.admit(ip(2)).unwrap();
        assert_eq!(app.active_sessions(), 3);
        assert_eq!(app.admit(ip(3)).err(), Some(Reject::Capacity));
        drop(a1);
        assert_eq!(app.active_sessions(), 2);
        // The freed slot is usable again, by the same IP.
        let _a3 = app.admit(ip(1)).unwrap();
        assert_eq!(app.active_sessions(), 3);
    }

    #[test]
    fn fly_header_only_trusted_when_enabled() {
        let mut h = HeaderMap::new();
        h.insert("fly-client-ip", "203.0.113.9".parse().unwrap());
        let peer = SocketAddr::from(([172, 16, 0, 1], 5555));
        let mut c = ServerConfig::default();
        assert_eq!(client_ip(&c, &h, peer), peer.ip());
        c.trust_fly_header = true;
        assert_eq!(
            client_ip(&c, &h, peer),
            "203.0.113.9".parse::<IpAddr>().unwrap()
        );
        h.insert("fly-client-ip", "garbage".parse().unwrap());
        assert_eq!(client_ip(&c, &h, peer), peer.ip());
    }

    #[test]
    fn command_parsing() {
        let c: Command =
            serde_json::from_str(r#"{"cmd":"scenario","value":"doppler_spoof"}"#).unwrap();
        assert!(matches!(
            c,
            Command::Scenario {
                value: Scenario::DopplerSpoof
            }
        ));
        assert!(serde_json::from_str::<Command>(r#"{"cmd":"scenario","value":"nope"}"#).is_err());
        assert!(serde_json::from_str::<Command>(r#"{"cmd":"rm -rf"}"#).is_err());
        let c: Command = serde_json::from_str(r#"{"cmd":"horizon_mask","enabled":true}"#).unwrap();
        assert!(matches!(c, Command::HorizonMask { enabled: true }));
    }
}
