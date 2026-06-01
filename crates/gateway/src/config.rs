//! Gateway configuration.
//!
//! Milestone 1 only needs ingestion settings; this module will grow station/propagator and
//! distribution settings in later milestones (see `BUILD_PLAN.md`).

use std::net::SocketAddr;

/// Configuration for the asynchronous UDP ingestion loop.
#[derive(Debug, Clone)]
pub struct IngestConfig {
    /// Local address the UDP socket binds to.
    ///
    /// Defaults to loopback to avoid firewall prompts during development; production deployments
    /// typically bind `0.0.0.0` (or a specific NIC) to receive from the SDR/front-end host.
    pub bind_addr: SocketAddr,

    /// Capacity of the internal broadcast channel, in frames.
    ///
    /// The channel is intentionally **lossy**: when full, the oldest frames are dropped and slow
    /// subscribers observe a lag rather than blocking the receive loop (see [`crate::ingest`]).
    pub channel_capacity: usize,

    /// Maximum accepted datagram size, in bytes. This fixes the receive-buffer size so the loop
    /// never allocates based on attacker-controlled input. Datagrams larger than this are dropped
    /// (Windows) or truncated by the OS (Unix); either way the loop stays in sync.
    pub max_datagram_size: usize,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            // 7301 is an arbitrary unprivileged default for the TMTC downlink port.
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 7301)),
            channel_capacity: 1024,
            // CCSDS space packets are at most 65536 + 6 bytes; a 64 KiB ceiling covers any single
            // packet while bounding per-datagram memory. Tunable per deployment.
            max_datagram_size: 65_542,
        }
    }
}
