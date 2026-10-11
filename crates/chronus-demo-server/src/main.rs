//! `chronus-demo-server` — hosts the per-visitor sandbox and (optionally) the dashboard build.
//!
//! Configuration is via environment variables; see [`chronus_demo_server::server::ServerConfig::from_env`].

#![forbid(unsafe_code)]

use std::net::SocketAddr;

use chronus_demo_server::server::{AppState, ServerConfig, router};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = ServerConfig::from_env();
    tracing::info!(
        bind = %cfg.bind,
        static_dir = ?cfg.static_dir,
        origins = ?cfg.allowed_origins,
        max_sessions = cfg.max_sessions,
        max_per_ip = cfg.max_per_ip,
        session_secs = cfg.session_secs,
        "starting chronus-demo-server"
    );
    if cfg.allowed_origins.is_empty() {
        tracing::warn!(
            "ALLOWED_ORIGINS is empty: any web origin may open a sandbox (fine for local dev only)"
        );
    }

    let listener = TcpListener::bind(cfg.bind).await?;
    let app = router(AppState::new(cfg));
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
