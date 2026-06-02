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
//! ## Current workflow
//!
//! The binary and future distribution layer use the same core flow:
//!
//! `RawFrame` → [`ccsds::parse_telemetry`] → [`propagator::TrackingProvider`] →
//! [`validate::apply_physics_validation`] → downstream logs/clients.
//!
//! `IngestConfig` and `StationConfig` provide safe local defaults for development. External
//! runtime configuration and WebSocket fan-out are intentionally deferred to later milestones.
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
