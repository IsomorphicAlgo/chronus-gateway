//! # ChronusGateway-RS
//!
//! An asynchronous, physics-validated Telemetry & Command (TMTC) gateway that bridges raw
//! spacecraft downlinks and web-based mission control (e.g. NASA Open MCT).
//!
//! Implemented so far: the [`propagator`] seam, the asynchronous UDP [`ingest`] loop (Milestone 1),
//! [`ccsds`] Space Packet parsing (Milestone 2), station-configured tracking (Milestone 3), the
//! [`validate`] Physics–Telemetry Co-Validation engine (Milestone 4 + **CV-1** link budget), and Axum HTTP + WebSocket
//! distribution with Open MCT–shaped JSON ([`http`], Milestone 5), observability / benches / CI
//! gates (Milestone 6), the NeXosim HIL workspace crate `chronus-hil-sim` (Milestone 7), and
//! optional TOML file configuration ([`config::file`], Milestone 8). See `docs/BUILD_PLAN.md`.
//! Post-M8 co-validation extensions are chartered in **`Methodology.md` D-016** and
//! `docs/EXTENDED_COVALIDATION_PLAN.md` (**CV-0…CV-4**; **CV-1** link budget implemented; **Gate CV-1** pending before **CV-2**).
//!
//! ## Standards & compliance
//!
//! Built strictly on open, international standards (CCSDS). See the repository `README.md` and
//! `Methodology.md` for compliance posture, attribution, and security priorities.

pub mod ccsds;
pub mod config;
pub mod http;
pub mod ingest;
pub mod metrics;
pub mod propagator;
pub mod state;
pub mod validate;

pub use ccsds::{encode_synthetic_tm, CcsdsError, TelemetryFrame};
pub use config::{
    load_effective_gateway_config, load_gateway_from_path, resolve_config_path, ConfigError,
    ConfigLoadError, IngestConfig, StationConfig, TleSource,
};
pub use ingest::{IngestStats, RawFrame};
pub use propagator::{EphemerustPropagator, OrbitalPropagator, TrackingProvider, TrackingState};
pub use validate::{
    apply_physics_validation, expected_carrier_hz, expected_rx_power_dbm, free_space_path_loss_db,
    LinkBudgetStationParams, RfMetadata, FLAG_BELOW_HORIZON, FLAG_DOPPLER_ANOMALY,
    FLAG_LINK_BUDGET_ANOMALY, FLAG_RSSI_RESERVED, SPEED_OF_LIGHT_M_S,
};

pub use http::{router, OpenMctRealtimeMessageV1};
pub use metrics::{GatewayMetrics, GatewayMetricsSnapshot};
pub use state::SharedGateway;
