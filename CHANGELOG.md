# Changelog

ChronusGateway-RS follows [Semantic Versioning](https://semver.org/). During `0.x`, the **minor**
version marks breaking changes and the **patch** version marks compatible ones (same policy as
the Ephemerust backend); a `1.0.0` release is reserved for a deliberately committed-stable API.
Versions cover all three workspace crates (`chronus-gateway`, `chronus-hil-sim`,
`chronus-replay`), which share one version via `[workspace.package]`.

## 0.1.0 — 2026-07-29

First versioned release line (previously `0.0.0` pre-release development). Not yet on crates.io.

### Highlights

- **Full pipeline (roadmap M0–M8):** async UDP ingest → CCSDS Space Packet parsing →
  Physics–Telemetry Co-Validation → Axum WebSocket distribution (`openmct.realtime.v1` JSON),
  metrics, NeXosim HIL simulator, and optional TOML configuration.
- **Extended co-validation CV-1…CV-5:** Doppler, elevation mask, link budget, pointing residual,
  and synthetic HIL subsystem envelopes (`physics_flags` bits; see
  `docs/EXTENDED_COVALIDATION_PLAN.md`).
- **Showcase S0–S4:** Docker Compose demo stack, Vite dashboard, `chronus-replay` UDP replay
  tool, scripted HIL anomalies, and curated public CCSDS fixtures.

### Changed in the run-up to 0.1.0

- **Ephemerust from crates.io** (`0.7` pin) replaces the sibling-checkout path dependency;
  CV-4 eclipse geometry upgraded to Ephemerust's conical umbra/penumbra model
  (Methodology **D-037**, **D-038**).
- **Rust edition 2024** with the MSRV-aware resolver (v3); MSRV **1.89** verified on the real
  toolchain. Dependency currency pass: axum 0.8, tower 0.5, tower-http 0.7, thiserror 2, toml 1,
  spacepackets 0.18, base64 0.23, criterion 0.8, tokio-tungstenite 0.30 (Methodology **D-039**).
- **Release mechanics:** all three crates pass full `cargo package` with verify builds;
  `chronus-hil-sim` declares `chronus-gateway` with both `path` and `version` for publishing.
