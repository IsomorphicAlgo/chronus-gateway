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
//! ## Standards & compliance
//!
//! Built strictly on open, international standards (CCSDS). See `AGENTS.md` for the project's
//! ITAR/EAR posture, attribution policy, and security priorities — all contributors and agents
//! must follow it.
//!
//! ## Current workflow
//!
//! The executable wires the public modules in this order:
//!
//! 1. [`ingest`] receives UDP datagrams as [`RawFrame`] values.
//! 2. [`ccsds::parse_telemetry`] validates TM Space Packets and returns [`TelemetryFrame`].
//! 3. [`TrackingProvider`] supplies station-relative look-angles and range rate.
//! 4. [`apply_physics_validation`] writes the stable `physics_flags` bitfield for consumers.
//!
//! The WebSocket/Open MCT consumer contract is planned for Milestone 5 and is not exposed yet.

pub mod ccsds;
pub mod config;
pub mod ingest;
pub mod propagator;
pub mod validate;

pub use ccsds::{CcsdsError, TelemetryFrame};
pub use config::{ConfigError, IngestConfig, StationConfig, TleSource};
pub use ingest::{IngestStats, RawFrame};
pub use propagator::{
    EphemerustPropagator, OrbitalPropagator, TrackingProvider, TrackingState,
};
pub use validate::{
    apply_physics_validation, expected_carrier_hz, RfMetadata, FLAG_BELOW_HORIZON,
    FLAG_DOPPLER_ANOMALY, FLAG_RSSI_RESERVED, SPEED_OF_LIGHT_M_S,
};
