# Milestone 7 — HIL / NeXosim profiling notes

This document is a **lightweight harness guide** for driving the gateway from the
`chronus-hil-sim` NeXosim bench (synthetic EPS / thermal / ADCS scalars in CCSDS TM over UDP).

## Synthetic payload layout (**CV-3** / `chronus.hil.tm.v1`)

The NeXosim driver packs each `TelemSample` (`crates/chronus-hil-sim/src/lib.rs`) into a **fixed 24-byte** CCSDS packet data field using
`chronus_gateway::hil_tm::encode_hil_tm_v1_payload` (`crates/gateway/src/hil_tm.rs`) (big-endian; magic **`CHI1`**, version byte **`1`**, three zero reserved bytes, then `seq` + three `f32` demo fields).

Decode on the gateway side with `decode_hil_tm_v1` on `tm.payload()` after CCSDS parse. **APID policy:** synthetic HIL frames are expected on APIDs in the inclusive range configured as `StationConfig::hil_tm_v1_apid_min` … `hil_tm_v1_apid_max` (defaults **0x7B0…0x7BF**; see `gateway.example.toml` optional keys).

## Smoke (automated)

Integration tests in `crates/chronus-hil-sim/tests/hil_ingest.rs`:

- `nexosim_smoke_reaches_ingest_and_parse` — NeXosim → loopback UDP → real `ingest::run` → parse + **HIL v1** decode when APID is in the default band.
- `nexosim_soak_bounded_recv_errors` — 400 frames; asserts `recv_errors == 0`, `frames_received == N`, payload length **24**, and round-trip decode `seq` matches frame index.

## Manual profiling with gateway metrics (M6)

1. Start the gateway: `cargo run -p chronus-gateway` (UDP default `127.0.0.1:7301`, HTTP `127.0.0.1:8080`).
2. In another shell, run the sim (release optional):  
   `cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 5000`
3. Poll `GET http://127.0.0.1:8080/api/v1/chronus/metrics` for ingest + gateway counters and average
   processing latency (document numbers in your own run log; figures vary by machine).

All telemetry is **synthetic** (public demo / compliance posture; see repository README).

**Credit:** [NeXosim](https://github.com/asynchronics/nexosim) — MIT OR Apache-2.0.

*Last updated: 2026-06-05.*
