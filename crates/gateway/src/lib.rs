//! # ChronusGateway-RS
//!
//! An asynchronous, physics-validated Telemetry & Command (TMTC) gateway that bridges raw
//! spacecraft downlinks and web-based mission control (e.g. NASA Open MCT).
//!
//! This crate is in **foundation** state: it currently exposes the [`propagator`] seam
//! (the keystone of the Physics-Telemetry Co-Validation engine). The async ingestion loop,
//! CCSDS parsing, validation engine, and WebSocket fan-out land in subsequent milestones
//! (see `Methodology.md` and the build plan).
//!
//! ## Standards & compliance
//!
//! Built strictly on open, international standards (CCSDS). See `AGENTS.md` for the project's
//! ITAR/EAR posture, attribution policy, and security priorities — all contributors and agents
//! must follow it.

pub mod propagator;
