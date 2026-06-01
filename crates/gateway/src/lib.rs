//! # ChronusGateway-RS
//!
//! An asynchronous, physics-validated Telemetry & Command (TMTC) gateway that bridges raw
//! spacecraft downlinks and web-based mission control (e.g. NASA Open MCT).
//!
//! Implemented so far: the [`propagator`] seam (keystone of the Physics-Telemetry Co-Validation
//! engine) and the asynchronous UDP [`ingest`] loop (Milestone 1). CCSDS parsing, the validation
//! engine, and the WebSocket fan-out land in subsequent milestones (see `BUILD_PLAN.md`).
//!
//! ## Standards & compliance
//!
//! Built strictly on open, international standards (CCSDS). See `AGENTS.md` for the project's
//! ITAR/EAR posture, attribution policy, and security priorities — all contributors and agents
//! must follow it.

pub mod config;
pub mod ingest;
pub mod propagator;

pub use config::IngestConfig;
pub use ingest::{IngestStats, RawFrame};
