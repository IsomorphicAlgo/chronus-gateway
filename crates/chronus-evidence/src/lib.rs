//! Evidence tools for the ChronusGateway-RS 1.0 release (Tranche 5). Lab tooling, not published.
//!
//! | Binary | Module | What it produces |
//! |---|---|---|
//! | `chronus-soak` | [`soak`] | Load / soak report: end-to-end latency percentiles, loss, memory trend (`docs/SOAK.md`) |
//! | `chronus-doppler` | [`doppler`] | Real-RF Doppler residuals against T-DOPPLER (`docs/RF_VALIDATION.md`) |

pub mod doppler;
pub mod soak;
