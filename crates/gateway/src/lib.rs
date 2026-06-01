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
//! ## Current library pipeline
//!
//! Library consumers compose the implemented stages explicitly:
//!
//! 1. configure [`IngestConfig`] and [`StationConfig`];
//! 2. bind/run [`ingest::run`] to receive [`RawFrame`] datagrams;
//! 3. parse each frame with [`ccsds::parse_telemetry`];
//! 4. obtain a [`TrackingState`] from [`TrackingProvider`]; and
//! 5. call [`apply_physics_validation`] to populate [`TelemetryFrame::physics_flags`].
//!
//! The most common types are re-exported at the crate root. Stage entrypoints such as
//! [`ccsds::parse_telemetry`], [`ingest::bind`], and [`ingest::run`] remain in their modules so the
//! crate-level namespace stays focused on shared data types and validation primitives.
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
pub use propagator::{
    EphemerustPropagator, OrbitalPropagator, TrackingProvider, TrackingState,
};
pub use validate::{
    apply_physics_validation, expected_carrier_hz, RfMetadata, FLAG_BELOW_HORIZON,
    FLAG_DOPPLER_ANOMALY, FLAG_RSSI_RESERVED, SPEED_OF_LIGHT_M_S,
};
