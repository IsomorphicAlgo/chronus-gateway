//! ChronusGateway-RS entrypoint.
//!
//! Current M1-M4 demonstration pipeline: bind the UDP downlink socket, broadcast raw frames,
//! parse CCSDS telemetry, compute station tracking state, apply Physics-Telemetry
//! Co-Validation, and emit structured logs. WebSocket/Open MCT fan-out is Milestone 5.

use std::sync::Arc;

use anyhow::Context;
use chronus_gateway::ccsds;
use chronus_gateway::config::{IngestConfig, StationConfig};
use chronus_gateway::ingest::{self, IngestStats, RawFrame};
use chronus_gateway::propagator::{EphemerustPropagator, TrackingProvider};
use chronus_gateway::validate::{apply_physics_validation, RfMetadata};
use tokio::sync::broadcast;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = IngestConfig::default();
    let socket = ingest::bind(&config)
        .await
        .with_context(|| format!("failed to bind UDP socket on {}", config.bind_addr))?;
    let local = socket.local_addr()?;
    tracing::info!(%local, "ChronusGateway-RS listening for telemetry");

    let (tx, mut rx) = broadcast::channel::<RawFrame>(config.channel_capacity);
    let stats = Arc::new(IngestStats::default());

    // Build the orbital tracking provider from the station configuration. If it cannot be built
    // (e.g. a bad TLE), the gateway still ingests and parses — it just runs without physics state.
    let station = StationConfig::default();
    let tracking = match EphemerustPropagator::from_station(&station) {
        Ok(prop) => {
            tracing::info!(
                lat = station.latitude_deg,
                lon = station.longitude_deg,
                carrier_hz = station.nominal_carrier_hz,
                "orbital tracking provider ready"
            );
            Some(Arc::new(TrackingProvider::new(
                Arc::new(prop),
                station.min_recompute_interval_ms,
            )))
        }
        Err(e) => {
            tracing::warn!(error = %e, "no orbital propagator; running without physics state");
            None
        }
    };

    let doppler_tol = station.doppler_tolerance_hz;
    let min_el = station.minimum_elevation_deg;
    let nominal_hz = station.nominal_carrier_hz;

    // Demonstration consumer: parse → tracking state → physics co-validation (Doppler skipped
    // until SDR metadata is wired; elevation gate always runs when physics is available).
    let logger = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(frame) => match ccsds::parse_telemetry(&frame) {
                    Ok(mut tm) => {
                        let physics = tracking
                            .as_ref()
                            .and_then(|t| t.tracking_state(tm.received_at).ok());
                        match physics {
                            Some(s) => {
                                apply_physics_validation(
                                    &mut tm,
                                    &s,
                                    nominal_hz,
                                    RfMetadata::default(),
                                    doppler_tol,
                                    min_el,
                                );
                                tracing::info!(
                                    apid = tm.apid,
                                    seq = tm.seq_count,
                                    payload = tm.payload_len(),
                                    az_deg = s.azimuth_deg,
                                    el_deg = s.elevation_deg,
                                    range_km = s.range_km,
                                    range_rate_km_s = s.range_rate_km_s,
                                    physics_flags = tm.physics_flags,
                                    "telemetry frame parsed"
                                );
                            }
                            None => tracing::info!(
                                apid = tm.apid,
                                seq = tm.seq_count,
                                payload = tm.payload_len(),
                                "telemetry frame parsed (no physics state)"
                            ),
                        }
                    }
                    Err(e) => tracing::warn!(
                        error = %e,
                        bytes = frame.bytes.len(),
                        "dropping invalid/non-telemetry datagram"
                    ),
                },
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "consumer lagged; dropped frames")
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Run until Ctrl-C.
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    ingest::run(socket, tx, config, Arc::clone(&stats), shutdown).await?;

    logger.abort();
    let (frames, bytes, oversized, errors) = stats.snapshot();
    tracing::info!(frames, bytes, oversized, errors, "shutdown complete");
    Ok(())
}
