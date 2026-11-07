//! ChronusGateway-RS entrypoint.
//!
//! Runs the UDP ingest loop, the processing pipeline and the Axum HTTP/WebSocket server until
//! Ctrl-C (Milestones 5–8, Tranches 2–4). `chronus-gateway --help` lists the options.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, ValueEnum};

use chronus_gateway::config::{
    EphemerisSource, FramingMode, IngestConfig, StationConfig, load_gateway_from_path,
};
use chronus_gateway::diagnose::{
    Diagnosis, Explain, Listener, Severity, config_warnings, explain_bind_error,
    explain_tracking_error,
};
use chronus_gateway::freshness::{DataStatus, Refresher, assess_data};
use chronus_gateway::history::HistoryStore;
use chronus_gateway::http;
use chronus_gateway::ingest::{self, IngestStats};
use chronus_gateway::metrics::GatewayMetrics;
use chronus_gateway::propagator::{TrackingProvider, propagator_from_station};
use chronus_gateway::security::{ApiTokens, SourceFilter, UdpPort, check_bind_policy};
use chronus_gateway::state::SharedGateway;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Exit code for "couldn't start" (bad config, port in use, ...), distinct from a runtime crash.
const EXIT_STARTUP: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LogFormat {
    /// Human-readable lines.
    Text,
    /// One JSON object per line (for log shippers).
    Json,
}

/// CCSDS telemetry gateway with physics co-validation.
///
/// Receives CCSDS Space Packets or TM Transfer Frames over UDP, checks them against orbital
/// physics, and serves them to dashboards (WebSocket, history, Open MCT, Prometheus).
#[derive(Debug, Parser)]
#[command(
    name = "chronus-gateway",
    version,
    after_help = "Configuration reference: gateway.example.toml. Operator guide: docs/USER_GUIDE.md. \
                  Hardened deployment: docs/DEPLOYMENT.md.\n\
                  Environment: CHRONUS_GATEWAY_CONFIG (config file), CHRONUS_API_TOKENS (API tokens), \
                  CHRONUS_LOG_FORMAT (text|json), RUST_LOG (log level, e.g. info or debug)."
)]
struct Cli {
    /// TOML configuration file. Without one, built-in defaults are used (loopback only).
    #[arg(short, long, env = "CHRONUS_GATEWAY_CONFIG", value_name = "FILE")]
    config: Option<PathBuf>,

    /// Check the configuration and data files, print what the gateway would run with, and exit
    /// (0 = ready, 2 = a problem must be fixed first). Opens no sockets.
    #[arg(long)]
    check_config: bool,

    /// Print a new random API token (for `security.api_tokens_file`) and exit.
    #[arg(long)]
    generate_token: bool,

    /// Log output format.
    #[arg(long, value_enum, default_value_t = LogFormat::Text, env = "CHRONUS_LOG_FORMAT")]
    log_format: LogFormat,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.generate_token {
        return match chronus_gateway::security::generate_token() {
            Ok(t) => {
                println!("{t}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: the operating system could not provide random bytes: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    match cli.log_format {
        LogFormat::Text => tracing_subscriber::fmt().with_env_filter(filter).init(),
        LogFormat::Json => tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_env_filter(filter)
            .init(),
    }

    let result = if cli.check_config {
        check_config(&cli).await
    } else {
        run(&cli).await
    };
    match result {
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
        // `Severity` is #[non_exhaustive]: unknown future levels are logged as warnings.
        _ => tracing::warn!("\n{d}"),
    }
}

/// Everything decided before any socket is opened.
struct Prepared {
    ingest: IngestConfig,
    /// Station with today's Earth orientation applied.
    station: StationConfig,
    tokens: ApiTokens,
    data: Arc<DataStatus>,
    refresher: Refresher,
    tracking: Option<Arc<TrackingProvider>>,
    warnings: Vec<Diagnosis>,
}

/// Loads and checks configuration, tokens and data files (shared by `run` and `--check-config`).
async fn prepare(cli: &Cli) -> Result<Prepared, Failure> {
    let (ingest, station) = match &cli.config {
        Some(path) => load_gateway_from_path(path).map_err(|e| Failure::Startup(e.explain()))?,
        None => {
            let (i, s) = (IngestConfig::default(), StationConfig::default());
            i.validate().map_err(|e| Failure::Startup(e.explain()))?;
            s.validate().map_err(|e| Failure::Startup(e.explain()))?;
            (i, s)
        }
    };
    let mut warnings = config_warnings(&ingest, &station);

    let tokens = ApiTokens::load(&station.security).map_err(|e| Failure::Startup(e.explain()))?;
    if let Some(d) = check_bind_policy(ingest.http_bind, &tokens, &station.security) {
        return Err(Failure::Startup(d));
    }

    let data = Arc::new(DataStatus::default());
    #[cfg(feature = "refresh")]
    {
        // Fetch only what is missing or older than the interval, before reading the files.
        let (st, ds) = (station.clone(), Arc::clone(&data));
        let _ = tokio::task::spawn_blocking(move || {
            chronus_gateway::freshness::fetch_due(&st, &ds, false);
        })
        .await;
    }

    let now = chrono::Utc::now();
    let (refresher, effective) = Refresher::new(&station, Arc::clone(&data), now)
        .map_err(|e| Failure::Startup(e.explain()))?;

    let tracking = match propagator_from_station(&effective) {
        Ok(prop) => Some(Arc::new(TrackingProvider::new(
            prop,
            effective.tracking.min_recompute_interval_ms,
        ))),
        Err(e) => {
            warnings.push(explain_tracking_error(&e));
            refresher.mark_physics_off();
            None
        }
    };
    if tracking.is_some() {
        warnings.extend(assess_data(&data.snapshot(now), now));
    }
    Ok(Prepared {
        ingest,
        station: effective,
        tokens,
        data,
        refresher,
        tracking,
        warnings,
    })
}

/// `--check-config`: print what the gateway would run with.
async fn check_config(cli: &Cli) -> Result<(), Failure> {
    let p = prepare(cli).await?;
    let (i, s) = (&p.ingest, &p.station);
    let now = chrono::Utc::now();
    let d = p.data.snapshot(now);
    let on_off = |b: bool| if b { "on" } else { "off" };

    println!(
        "chronus-gateway {} — configuration check",
        env!("CARGO_PKG_VERSION")
    );
    println!(
        "  config file        {}",
        cli.config
            .as_ref()
            .map_or("(none: built-in defaults)".to_string(), |p| p
                .display()
                .to_string())
    );
    println!("  telemetry (UDP)    {}", i.bind_addr);
    println!("  dashboards (HTTP)  {}", i.http_bind);
    println!(
        "  framing            {}",
        match s.framing.mode {
            FramingMode::TmFrames => format!(
                "TM transfer frames (length {:?}, sync marker {}, derandomize {}, CRC {})",
                s.framing.frame_length,
                on_off(s.framing.asm),
                on_off(s.framing.derandomize),
                on_off(s.framing.fecf)
            ),
            _ => "one Space Packet per datagram".to_string(),
        }
    );
    println!(
        "  receiver data      {}",
        s.rf.bind_addr
            .map_or("off".to_string(), |a| format!("chronus.rf.v1 on {a}"))
    );
    println!(
        "  spacecraft time    {:?} (max skew {} s)",
        s.time.format, s.time.max_skew_s
    );
    println!(
        "  history            {} messages{}",
        s.history.capacity,
        s.history
            .persist_path
            .as_ref()
            .map_or(String::new(), |p| format!(", file {}", p.display()))
    );
    println!(
        "  API tokens         {}",
        if p.tokens.is_enabled() {
            format!("required ({} configured)", p.tokens.len())
        } else {
            "not required".to_string()
        }
    );
    println!(
        "  WebSocket          up to {} clients; origins: this computer{}",
        s.security.max_ws_clients,
        if s.security.allowed_origins.is_empty() {
            String::new()
        } else {
            format!(", {}", s.security.allowed_origins.join(", "))
        }
    );
    let nets = |n: &Option<Vec<chronus_gateway::config::IpNet>>| {
        n.as_ref().map_or("any sender".to_string(), |v| {
            v.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        })
    };
    println!(
        "  UDP senders        telemetry: {}; receiver: {}",
        nets(&s.security.udp_allowed_sources),
        nets(&s.security.rf_allowed_sources)
    );
    match d.ephemeris_source {
        "none" => println!("  orbit data         unavailable: physics checks off"),
        src => {
            if let (Some(epoch), Some(age)) = (d.tle_epoch, d.tle_age_days) {
                println!(
                    "  orbit data         {src}: {} epoch {} ({age:.1} days old)",
                    d.tle_name.as_deref().unwrap_or("TLE"),
                    epoch.format("%Y-%m-%d %H:%M UTC")
                );
            } else if let (Some(a), Some(b)) = (d.oem_start, d.oem_stop) {
                println!(
                    "  orbit data         {src}: OEM covering {} to {}",
                    a.format("%Y-%m-%d %H:%M UTC"),
                    b.format("%Y-%m-%d %H:%M UTC")
                );
            }
        }
    }
    println!(
        "  Earth orientation  {}: UT1-UTC {:+.7} s, pole {:.6}\" {:.6}\"{}",
        d.eop_source,
        d.ut1_minus_utc_s,
        d.polar_motion_x_arcsec,
        d.polar_motion_y_arcsec,
        d.eop_file_last_day
            .map_or(String::new(), |l| format!(" (file covers to {l})"))
    );
    println!(
        "  data reload        {}",
        if s.freshness.reload_interval_s == 0 {
            "off".to_string()
        } else {
            format!("every {} s", s.freshness.reload_interval_s)
        }
    );

    let errors = p
        .warnings
        .iter()
        .filter(|w| w.severity == Severity::Error)
        .count();
    for w in &p.warnings {
        println!("\n{w}");
    }
    if errors > 0 {
        return Err(Failure::Startup(
            Diagnosis::new(
                "CG-142",
                Severity::Error,
                format!(
                    "{errors} problem(s) above must be fixed before the gateway can run properly."
                ),
                "--check-config found errors.",
            )
            .fix("Fix the problems listed above and run --check-config again."),
        ));
    }
    println!("\nReady: the gateway can start with this configuration.");
    Ok(())
}

async fn run(cli: &Cli) -> Result<(), Failure> {
    let p = prepare(cli).await?;
    for d in &p.warnings {
        report(d);
    }
    let Prepared {
        ingest: ingest_cfg,
        station: station_cfg,
        tokens,
        data,
        refresher,
        tracking,
        ..
    } = p;

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

    if tracking.is_some() {
        tracing::info!(
            lat = station.site.latitude_deg,
            lon = station.site.longitude_deg,
            carrier_hz = station.site.nominal_carrier_hz,
            ephemeris = if matches!(station.tracking.ephemeris, EphemerisSource::OemFile(_)) {
                "oem"
            } else {
                "tle"
            },
            refraction = station.tracking.refraction,
            tle_age_doppler_margin = station.tracking.doppler_tle_age_margin,
            ut1_minus_utc_s = station.tracking.ut1_minus_utc_s,
            "orbital tracking provider ready"
        );
    }

    // Tranche 2: optional chronus.rf.v1 receiver-metadata side channel.
    let rf_socket = match station.rf.bind_addr {
        Some(addr) => {
            let sock = tokio::net::UdpSocket::bind(addr)
                .await
                .map_err(|e| Failure::Startup(explain_bind_error(Listener::Rf, addr, &e)))?;
            let local = sock.local_addr().map_err(|e| Failure::Runtime(e.into()))?;
            tracing::info!(
                rf_local = %local,
                window_ms = station.rf.window_ms,
                capacity = station.rf.capacity,
                "chronus.rf.v1 receiver-metadata channel listening"
            );
            Some(sock)
        }
        None => None,
    };

    let listener = TcpListener::bind(ingest_cfg.http_bind).await.map_err(|e| {
        Failure::Startup(explain_bind_error(Listener::Http, ingest_cfg.http_bind, &e))
    })?;
    let http_local = listener
        .local_addr()
        .map_err(|e| Failure::Runtime(e.into()))?;

    // Tranche 3: recent-telemetry history, optionally reloaded from / appended to a file.
    let history =
        HistoryStore::open(&station.history).map_err(|e| Failure::Startup(e.explain()))?;
    if let Some(path) = history.persist_path() {
        tracing::info!(
            path = %path.display(),
            reloaded = history.len(),
            capacity = station.history.capacity,
            "telemetry history file open"
        );
    }
    let history = Arc::new(history);

    let auth_enabled = tokens.is_enabled();
    let state = Arc::new(
        SharedGateway::new(
            frame_tx.clone(),
            Arc::clone(&ingest_stats),
            Arc::clone(&gateway_metrics),
            Arc::clone(&station),
            tracking.clone(),
        )
        .with_listen_addrs(udp_local, http_local)
        .with_history(Arc::clone(&history))
        .with_api_tokens(tokens)
        .with_data_status(Arc::clone(&data)),
    );
    tracing::info!(
        api_tokens = if auth_enabled { "required" } else { "not required" },
        max_ws_clients = station.security.max_ws_clients,
        allowed_origins = ?station.security.allowed_origins,
        "access control"
    );

    let cancel = CancellationToken::new();
    // Subscribe the pipeline before ingest starts so no datagram is missed.
    let pipeline_cancel = cancel.child_token();
    let pipeline_handle = chronus_gateway::pipeline::spawn(Arc::clone(&state), async move {
        pipeline_cancel.cancelled().await;
    });
    match station.framing.mode {
        FramingMode::TmFrames => tracing::info!(
            frame_length = ?station.framing.frame_length,
            asm = station.framing.asm,
            derandomize = station.framing.derandomize,
            fecf = station.framing.fecf,
            spacecraft_id = ?station.framing.spacecraft_id,
            virtual_channels = ?station.framing.virtual_channels,
            "CCSDS TM Transfer Frame decoding on"
        ),
        _ => tracing::debug!("one CCSDS Space Packet per datagram"),
    }

    let ingest_cancel = cancel.child_token();
    let udp_filter = SourceFilter::new(
        station.security.udp_allowed_sources.clone(),
        Arc::clone(&state.security),
        UdpPort::Telemetry,
    );
    let ingest_handle = tokio::spawn({
        let ingest_stats = Arc::clone(&state.ingest_stats);
        let ingest_cfg_clone = ingest_cfg.clone();
        async move {
            ingest::run_filtered(
                socket,
                frame_tx,
                ingest_cfg_clone,
                ingest_stats,
                udp_filter,
                ingest_cancel.cancelled(),
            )
            .await
        }
    });

    let rf_handle = rf_socket.map(|sock| {
        let store = Arc::clone(&state.rf);
        let rf_cancel = cancel.child_token();
        let filter = SourceFilter::new(
            station.security.rf_allowed_sources.clone(),
            Arc::clone(&state.security),
            UdpPort::Rf,
        );
        tokio::spawn(async move {
            chronus_gateway::rf::run_listener_filtered(sock, store, filter, rf_cancel.cancelled())
                .await
        })
    });

    // Tranche 4: reload changed TLE / OEM / Earth-orientation files.
    let refresh_cancel = cancel.child_token();
    let refresh_handle = tokio::spawn(chronus_gateway::freshness::run(
        refresher,
        tracking,
        async move { refresh_cancel.cancelled().await },
    ));
    #[cfg(feature = "refresh")]
    let fetch_handle = {
        let fetch_cancel = cancel.child_token();
        tokio::spawn(chronus_gateway::freshness::run_fetcher(
            Arc::clone(&station),
            Arc::clone(&data),
            async move { fetch_cancel.cancelled().await },
        ))
    };

    if station.time.format != chronus_gateway::TimeFormat::None {
        tracing::info!(
            format = ?station.time.format,
            max_skew_s = station.time.max_skew_s,
            "spacecraft time decoding on: physics runs at onboard time{}",
            if station.time.max_skew_s == 0.0 { " (lab mode: skew check off)" } else { "" }
        );
    }

    tracing::info!(%http_local, "HTTP + WebSocket (GET /telemetry/openmct) listening");
    tracing::info!(
        "\n  Gateway is running.\n  \
         - Send CCSDS telemetry to   udp://{udp_local}\n  \
         - Connect dashboards to     ws://{http_local}/telemetry/openmct\n  \
         - Check health anytime at   http://{http_local}/api/v1/chronus/status\n  \
         - Recent history at         http://{http_local}/api/v1/chronus/history\n  \
         - Prometheus metrics at     http://{http_local}/metrics\n  \
         Press Ctrl+C to stop."
    );

    let app = http::router(Arc::clone(&state));
    let cancel_serve = cancel.clone();
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        shutdown_signal().await;
        cancel_serve.cancel();
    });

    server
        .await
        .map_err(|e| Failure::Runtime(anyhow::Error::new(e).context("HTTP server terminated")))?;

    ingest_handle
        .await
        .map_err(|e| Failure::Runtime(anyhow::Error::new(e).context("ingest task join")))?
        .map_err(|e| Failure::Runtime(anyhow::Error::new(e).context("ingest loop")))?;

    pipeline_handle
        .await
        .map_err(|e| Failure::Runtime(anyhow::Error::new(e).context("pipeline task join")))?;
    let _ = refresh_handle.await;
    #[cfg(feature = "refresh")]
    let _ = fetch_handle.await;
    // Flush the history file before exiting.
    history.close();

    if let Some(h) = rf_handle {
        h.await
            .map_err(|e| Failure::Runtime(anyhow::Error::new(e).context("rf task join")))?
            .map_err(|e| Failure::Runtime(anyhow::Error::new(e).context("rf listener")))?;
    }

    let (frames, bytes, oversized, errors) = state.ingest_stats.snapshot();
    tracing::info!(frames, bytes, oversized, errors, "shutdown complete");
    Ok(())
}

/// Ctrl-C everywhere, plus SIGTERM on Unix (what `docker stop` and systemd send), so the history
/// file is flushed on a normal service stop.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
