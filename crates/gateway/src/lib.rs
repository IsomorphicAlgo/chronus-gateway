//! # ChronusGateway-RS
//!
//! An asynchronous, physics-validated Telemetry & Command (TMTC) gateway that bridges raw
//! spacecraft downlinks and web-based mission control (e.g. NASA Open MCT).
//!
//! Implemented so far: the [`propagator`] seam, the asynchronous UDP [`ingest`] loop (Milestone 1),
//! [`ccsds`] Space Packet parsing (Milestone 2), station-configured tracking (Milestone 3), and
//! the [`validate`] Physics–Telemetry Co-Validation engine (Milestone 4). WebSocket / Open MCT
//! distribution is Milestone 5 (see `BUILD_PLAN.md`).
//!
//! ## Current pipeline
//!
//! `RawFrame` -> [`ccsds::parse_telemetry`] -> [`TrackingProvider::tracking_state`] ->
//! [`apply_physics_validation`]. The crate re-exports the public frame, config, propagator, and
//! validation types used by that path so integration tests and future distribution adapters do not
//! depend on private module internals.
//!
//! ## Standards & compliance
//!
//! Built strictly on open, international standards (CCSDS). See `AGENTS.md` for the project's
//! ITAR/EAR posture, attribution policy, and security priorities — all contributors and agents
//! must follow it.

pub mod ccsds;
pub mod config;
pub mod ingest;
pub mod propagator;
pub mod validate;

pub use ccsds::{CcsdsError, TelemetryFrame};
pub use config::{ConfigError, IngestConfig, StationConfig, TleSource};
pub use ingest::{IngestStats, RawFrame};
pub use propagator::{EphemerustPropagator, OrbitalPropagator, TrackingProvider, TrackingState};
pub use validate::{
    apply_physics_validation, expected_carrier_hz, RfMetadata, FLAG_BELOW_HORIZON,
    FLAG_DOPPLER_ANOMALY, FLAG_RSSI_RESERVED, SPEED_OF_LIGHT_M_S,
};
