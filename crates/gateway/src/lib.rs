//! # ChronusGateway-RS
//!
//! An asynchronous, physics-validated Telemetry & Command (TMTC) gateway that bridges raw
//! spacecraft downlinks and web-based mission control (e.g. NASA Open MCT).
//!
//! Implemented so far: the [`propagator`] seam, the asynchronous UDP [`ingest`] loop (Milestone 1),
//! [`ccsds`] Space Packet parsing (Milestone 2), station-configured tracking (Milestone 3), the
//! [`validate`] Physics–Telemetry Co-Validation engine (Milestone 4), and Axum HTTP + WebSocket
//! distribution with Open MCT–shaped JSON ([`http`], Milestone 5), observability / benches / CI
//! gates (Milestone 6), and the NeXosim HIL workspace crate `chronus-hil-sim` (Milestone 7). See
//! `BUILD_PLAN.md`.
//!
//! ## Standards & compliance
//!
//! Built strictly on open, international standards (CCSDS). See `AGENTS.md` for the project's
//! ITAR/EAR posture, attribution policy, and security priorities — all contributors and agents
//! must follow it.

pub mod ccsds;
pub mod config;
pub mod http;
pub mod ingest;
pub mod metrics;
pub mod propagator;
pub mod state;
pub mod validate;

pub use ccsds::{encode_synthetic_tm, CcsdsError, TelemetryFrame};
pub use config::{ConfigError, IngestConfig, StationConfig, TleSource};
pub use ingest::{IngestStats, RawFrame};
pub use propagator::{
    EphemerustPropagator, OrbitalPropagator, TrackingProvider, TrackingState,
};
pub use validate::{
    apply_physics_validation, expected_carrier_hz, RfMetadata, FLAG_BELOW_HORIZON,
    FLAG_DOPPLER_ANOMALY, FLAG_RSSI_RESERVED, SPEED_OF_LIGHT_M_S,
};

pub use http::{router, OpenMctRealtimeMessageV1};
pub use metrics::{GatewayMetrics, GatewayMetricsSnapshot};
pub use state::SharedGateway;
