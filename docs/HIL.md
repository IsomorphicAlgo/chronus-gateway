# Milestone 7 — HIL / NeXosim profiling notes

This document is a **lightweight harness guide** for driving the gateway from the
`chronus-hil-sim` NeXosim bench (synthetic EPS / thermal / ADCS scalars in CCSDS TM over UDP).

## What the HIL driver sends

`chronus-hil-sim` is a synthetic, single-spacecraft harness. It does not model real mission
hardware, keys, RF frequencies, or operational parameters.

| Item | Value / behavior |
|------|------------------|
| CLI | `chronus-hil-sim [DEST] [FRAMES]` |
| Default destination | `127.0.0.1:7301` (gateway UDP ingest) |
| Default frame count | `100` |
| CLI APID | `0x7B0` |
| Simulation cadence | 1 ms **simulation-time** steps; the run returns quickly and is not wall-clock paced |
| Packet encoder | `chronus_gateway::encode_synthetic_tm` (TM, unsegmented, no secondary header) |

Each packet data field is a 16-byte big-endian `TelemSample`:

| Offset | Type | Field |
|--------|------|-------|
| `0..4` | `u32` | monotonic sample sequence |
| `4..8` | `f32` | abstract EPS bus voltage, volts |
| `8..12` | `f32` | abstract panel temperature, degrees Celsius |
| `12..16` | `f32` | abstract body rate, degrees/second |

## Smoke (automated)

Integration tests in `crates/chronus-hil-sim/tests/hil_ingest.rs`:

- `nexosim_smoke_reaches_ingest_and_parse` — NeXosim → loopback UDP → real `ingest::run` → parse.
- `nexosim_soak_bounded_recv_errors` — 400 frames; asserts `recv_errors == 0` and `frames_received == N`.

These tests cover the HIL simulator, UDP ingest, and CCSDS parse path. They do not exercise the
Axum WebSocket distribution layer; that path is covered by `crates/gateway/tests/distribution.rs`.

## Manual profiling with gateway metrics (M6)

1. Start the gateway: `cargo run -p chronus-gateway` (UDP default `127.0.0.1:7301`, HTTP `127.0.0.1:8080`).
2. In another shell, run the sim (release optional):  
   `cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 5000`
3. Poll `GET http://127.0.0.1:8080/api/v1/chronus/metrics` for ingest + gateway counters and average
   processing latency (document numbers in your own run log; figures vary by machine).

Example metrics response shape:

```json
{
  "ingest": {
    "frames_received": 5000,
    "bytes_received": 110000,
    "oversized_dropped": 0,
    "recv_errors": 0
  },
  "gateway": {
    "telemetry_frames_emitted": 0,
    "telemetry_parse_errors": 0,
    "anomaly_frames": 0,
    "ws_messages_sent": 0,
    "processing_latency_ms_sum": 0,
    "processing_latency_ms_count": 0,
    "ws_clients_connected": 0
  },
  "avg_processing_latency_ms": null
}
```

If no WebSocket client is connected, only `ingest.*` counters are expected to increase. The gateway
parses, validates, and emits telemetry JSON on the WebSocket subscriber path, so
`telemetry_frames_emitted`, `ws_messages_sent`, and latency counters increase when a client is
connected to `GET /telemetry/openmct`.

All telemetry is **synthetic** (see `AGENTS.md`).

## References

- `crates/chronus-hil-sim/src/lib.rs` — `SpacecraftDemo`, `TelemSample`, and UDP bridge.
- `crates/chronus-hil-sim/src/main.rs` — CLI defaults and APID.
- `crates/gateway/src/ccsds.rs` — synthetic TM encoder and parse constraints.
- `crates/gateway/src/http.rs` — metrics and WebSocket distribution path.

## Acknowledgements

- [NeXosim](https://github.com/asynchronics/nexosim) (asynchronics) — discrete-event simulation
  framework used for the HIL driver, licensed MIT OR Apache-2.0.
- [CCSDS](https://public.ccsds.org/) — open Space Packet standard used for the synthetic wire
  format.

*Last updated: 2026-06-03.*
