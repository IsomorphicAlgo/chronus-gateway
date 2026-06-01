//! ChronusGateway-RS entrypoint.
//!
//! At foundation stage this binary is a smoke test that proves the Ephemerust-backed
//! propagator links and runs end to end. As milestones land it grows into the full gateway:
//! UDP ingestion → CCSDS parse → physics co-validation → Open MCT WebSocket fan-out.

use anyhow::Result;
use chrono::{TimeZone, Utc};
use chronus_gateway::propagator::{EphemerustPropagator, OrbitalPropagator};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Reference ISS (ZARYA) element set (valid checksums; epoch ~2020-07-12).
    // Replace with a live TLE + configured station once ingestion + config land.
    let iss = "ISS (ZARYA)\n\
        1 25544U 98067A   20194.88612269 -.00002218  00000-0 -31515-4 0  9992\n\
        2 25544  51.6461 221.2784 0001413  89.1723 280.4612 15.49507896236008";

    // Example ground station (lat °, lon °, altitude m). Placeholder until config is wired.
    let propagator = EphemerustPropagator::new(iss, 35.0, -116.0, 1000.0)?;

    // Evaluate near the TLE epoch so the propagation stays inside SGP4's accurate window.
    let epoch = Utc.with_ymd_and_hms(2020, 7, 12, 21, 0, 0).single().expect("valid instant");
    let state = propagator.tracking_state(epoch)?;

    tracing::info!(
        azimuth_deg = state.azimuth_deg,
        elevation_deg = state.elevation_deg,
        range_km = state.range_km,
        range_rate_km_s = state.range_rate_km_s,
        "propagator smoke test OK"
    );
    println!("ChronusGateway-RS foundation OK\n  tracking state @ {epoch}: {state:#?}");

    Ok(())
}
