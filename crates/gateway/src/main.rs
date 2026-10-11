//! ChronusGateway-RS entrypoint.
//!
//! Runs the UDP ingest loop and the Axum HTTP/WebSocket server until Ctrl-C (Milestones 5–8).

#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::sync::Arc;

use chronus_gateway::config::load_effective_gateway_config;
use chronus_gateway::diagnose::{
    Diagnosis, Explain, Listener, Severity, check_oem_coverage, check_tle_age, config_warnings,
    explain_bind_error, explain_tracking_error, tle_epoch,
};
use chronus_gateway::http;
use chronus_gateway::ingest::{self, IngestStats};
use chronus_gateway::metrics::GatewayMetrics;
use chronus_gateway::oem::OemEphemeris;
use chronus_gateway::propagator::{TrackingProvider, propagator_from_station};
use chronus_gateway::state::SharedGateway;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Exit code for "couldn't start" (bad config, port in use, ...), distinct from a runtime crash.
const EXIT_STARTUP: u8 = 2;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Startup(d)) => {
            eprintln!("\n{d}\n");
            eprintln!("The gateway did not start. Fix the problem above and run it again.");
            ExitCode::from(EXIT_STARTUP)
        }
        Err(Failure::Runtime(e)) => {
            eprintln!("\nerror: the gateway stopped unexpectedly: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Why `run` returned early.
enum Failure {
    /// Could not start; carries a plain-English explanation.
    Startup(Diagnosis),
    /// Failed after starting (unexpected; reported with the full error chain).
    Runtime(anyhow::Error),
}

/// Prints a non-fatal diagnosis through the log at a level matching its severity.
fn report(d: &Diagnosis) {
    match d.severity {
        Severity::Info => tracing::info!("\n{d}"),
        Severity::Warning | Severity::Error => tracing::warn!("\n{d}"),
    }
}

async fn run() -> Result<(), Failure> {
    let (ingest_cfg, station_cfg) =
        load_effective_gateway_config().map_err(|e| Failure::Startup(e.explain()))?;
    for d in config_warnings(&ingest_cfg, &station_cfg) {
        report(&d);
    }

    let socket = ingest::bind(&ingest_cfg).await.map_err(|e| {
        Failure::Startup(explain_bind_error(Listener::Udp, ingest_cfg.bind_addr, &e))
    })?;
    let udp_local = socket
        .local_addr()
        .map_err(|e| Failure::Runtime(e.into()))?;
    tracing::info!(%udp_local, "UDP telemetry ingest listening");

    let (frame_tx, _rx) = broadcast::channel(ingest_cfg.channel_capacity);
    let ingest_stats = Arc::new(IngestStats::default());
    let gateway_metrics = Arc::new(GatewayMetrics::default());
    let station = Arc::new(station_cfg);

    let tracking = match propagator_from_station(station.as_ref()) {
        Ok(prop) => {
            tracing::info!(
                lat = station.latitude_deg,
                lon = station.longitude_deg,
                carrier_hz = station.nominal_carrier_hz,
                ephemeris = if station.oem_file.is_some() {
                    "oem"
                } else {
                    "tle"
                },
                refraction = station.refraction,
                tle_age_doppler_margin = station.doppler_tle_age_margin,
                "orbital tracking provider ready"
            );
            let now = chrono::Utc::now();
            let freshness = match &station.oem_file {
                // Startup-only second read of the file, purely for the coverage check.
                Some(path) => OemEphemeris::from_file(path)
                    .ok()
                    .and_then(|eph| check_oem_coverage(eph.coverage(), now)),
                None => station
                    .resolve_tle_text()
                    .ok()
                    .as_deref()
                    .and_then(tle_epoch)
                    .and_then(|epoch| check_tle_age(epoch, now)),
            };
            if let Some(d) = freshness {
                report(&d);
            }
            Some(Arc::new(TrackingProvider::new(
                prop,
                station.min_recompute_interval_ms,
            )))
        }
        Err(e) => {
            report(&explain_tracking_error(&e));
            None
        }
    };

    let listener = TcpListener::bind(ingest_cfg.http_bind).await.map_err(|e| {
        Failure::Startup(explain_bind_error(Listener::Http, ingest_cfg.http_bind, &e))
    })?;
    let http_local = listener
        .local_addr()
        .map_err(|e| Failure::Runtime(e.into()))?;

    let state = Arc::new(
        SharedGateway::new(
            frame_tx.clone(),
            Arc::clone(&ingest_stats),
            Arc::clone(&gateway_metrics),
            Arc::clone(&station),
            tracking,
        )
        .with_listen_addrs(udp_local, http_local),
    );

    let cancel = CancellationToken::new();
    let ingest_cancel = cancel.child_token();
    let ingest_handle = tokio::spawn({
        let ingest_stats = Arc::clone(&state.ingest_stats);
        let ingest_cfg_clone = ingest_cfg.clone();
        async move {
            ingest::run(
                socket,
                frame_tx,
                ingest_cfg_clone,
                ingest_stats,
                ingest_cancel.cancelled(),
            )
            .await
        }
    });

    tracing::info!(%http_local, "HTTP + WebSocket (GET /telemetry/openmct) listening");
    tracing::info!(
        "\n  Gateway is running.\n  \
         - Send CCSDS telemetry to   udp://{udp_local}\n  \
         - Connect dashboards to     ws://{http_local}/telemetry/openmct\n  \
         - Check health anytime at   http://{http_local}/api/v1/chronus/status\n  \
         Press Ctrl+C to stop."
    );

    let app = http::router(Arc::clone(&state));
    let cancel_serve = cancel.clone();
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        cancel_serve.cancel();
    });

    server
        .await
        .map_err(|e| Failure::Runtime(anyhow::Error::new(e).context("HTTP server terminated")))?;

    ingest_handle
        .await
        .map_err(|e| Failure::Runtime(anyhow::Error::new(e).context("ingest task join")))?
        .map_err(|e| Failure::Runtime(anyhow::Error::new(e).context("ingest loop")))?;

    let (frames, bytes, oversized, errors) = state.ingest_stats.snapshot();
    tracing::info!(frames, bytes, oversized, errors, "shutdown complete");
    Ok(())
}
