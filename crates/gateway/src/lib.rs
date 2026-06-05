//! # ChronusGateway-RS
//!
//! An asynchronous, physics-validated Telemetry & Command (TMTC) gateway that bridges raw
//! spacecraft downlinks and web-based mission control (e.g. NASA Open MCT).
//!
//! Implemented so far: the [`propagator`] seam, the asynchronous UDP [`ingest`] loop (Milestone 1),
//! [`ccsds`] Space Packet parsing (Milestone 2), station-configured tracking (Milestone 3), the
//! [`validate`] Physics–Telemetry Co-Validation engine (Milestone 4 + **CV-1** link budget + **CV-2** pointing), and Axum HTTP + WebSocket
//! distribution with Open MCT–shaped JSON ([`http`], Milestone 5), observability / benches / CI
//! gates (Milestone 6), the NeXosim HIL workspace crate `chronus-hil-sim` (Milestone 7), and
//! optional TOML file configuration ([`config::file`], Milestone 8). See `docs/BUILD_PLAN.md`.
//! Post-M8 co-validation extensions are chartered in **`Methodology.md` D-016** and
//! `docs/EXTENDED_COVALIDATION_PLAN.md` (**CV-0…CV-4**; **CV-1…CV-4** implemented — synthetic HIL TM in [`hil_tm`], subsystem toy checks vs Sun proxy in [`validate`] / [`propagator`]).
//!
//! ## Standards & compliance
//!
//! Built strictly on open, international standards (CCSDS). See the repository `README.md` and
//! `Methodology.md` for compliance posture, attribution, and security priorities.

pub mod ccsds;
pub mod config;
pub mod hil_tm;
pub mod http;
pub mod ingest;
pub mod metrics;
pub mod propagator;
pub mod state;
pub mod validate;

pub use ccsds::{encode_synthetic_tm, CcsdsError, TelemetryFrame};
pub use hil_tm::{
    decode_hil_tm_v1, encode_hil_tm_v1_payload, CCSDS_APID_MAX, DecodedHilTmV1,
    DEFAULT_HIL_TM_V1_APID_MAX, DEFAULT_HIL_TM_V1_APID_MIN, HilTmV1DecodeError, HIL_TM_V1_MAGIC,
    HIL_TM_V1_PAYLOAD_LEN, HIL_TM_V1_SCHEMA_VERSION,
};
pub use config::{
    load_effective_gateway_config, load_gateway_from_path, resolve_config_path, ConfigError,
    ConfigLoadError, IngestConfig, StationConfig, TleSource,
};
pub use ingest::{IngestStats, RawFrame};
pub use propagator::{
    nadir_sun_illumination_cos, EphemerustPropagator, OrbitalPropagator, TrackingProvider,
    TrackingState,
};
pub use validate::{
    angular_separation_deg, apply_physics_validation, expected_carrier_hz, expected_rx_power_dbm,
    free_space_path_loss_db, HilSubsystemCvParams, LinkBudgetStationParams, RfMetadata,
    FLAG_BELOW_HORIZON, FLAG_DOPPLER_ANOMALY, FLAG_EPS_SUBSYSTEM_ANOMALY,
    FLAG_LINK_BUDGET_ANOMALY, FLAG_POINTING_ANOMALY, FLAG_RSSI_RESERVED,
    FLAG_THERMAL_SUBSYSTEM_ANOMALY, SPEED_OF_LIGHT_M_S,
};

pub use http::{router, OpenMctRealtimeMessageV1};
pub use metrics::{GatewayMetrics, GatewayMetricsSnapshot};
pub use state::SharedGateway;
