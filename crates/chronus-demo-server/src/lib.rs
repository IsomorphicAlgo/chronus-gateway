//! Public **per-visitor sandbox** for ChronusGateway-RS.
//!
//! Each browser tab that opens `GET /sandbox/ws` gets a private, time-accelerated synthetic
//! spacecraft ([`sandbox::SessionSim`]) whose frames run through the *same* CCSDS parse →
//! physics co-validation → Open MCT JSON path as the real gateway
//! ([`chronus_gateway::http::process_frame_with_rf`]); only the UDP hop is replaced by an
//! in-process hand-off. Visitors can inject faults without affecting anyone else.
//!
//! The server exposes **no UDP socket** and accepts no uploaded data; everything is synthetic
//! (public ISS TLE, toy subsystem models).

#![forbid(unsafe_code)]

pub mod sandbox;
pub mod server;
