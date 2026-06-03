# Milestone 7 — HIL / NeXosim profiling notes

This document is a **lightweight harness guide** for driving the gateway from the
`chronus-hil-sim` NeXosim bench (synthetic EPS / thermal / ADCS scalars in CCSDS TM over UDP).

All telemetry is **synthetic** and generic (see `AGENTS.md`). Do not replace these examples with
mission-specific frequencies, keys, spacecraft identifiers, or operational parameters.

## Architecture covered

```
SpacecraftDemo ──TelemSample──▶ UdpDownlinkBridge ──CCSDS TM / UDP──▶ gateway ingest
 (NeXosim)                         (std::net::UdpSocket)              127.0.0.1:7301
```

Codepaths:

- `crates/chronus-hil-sim/src/lib.rs` — `SpacecraftDemo`, `TelemSample`,
  `UdpDownlinkBridge`, and `run_nexosim_udp_hil`.
- `crates/chronus-hil-sim/src/main.rs` — CLI wrapper: `chronus-hil-sim [DEST] [FRAMES]`.
- `crates/gateway/src/ccsds.rs` — `encode_synthetic_tm`, shared with tests and the simulator.
- `crates/gateway/src/ingest.rs` and `crates/gateway/src/http.rs` — gateway receive path and
  metrics endpoint used for profiling.

## Synthetic packet contract

The simulator emits one CCSDS telemetry Space Packet per UDP datagram:

- APID: `0x7B0` from the CLI default.
- Sequence count: `TelemSample.seq & 0x3FFF`.
- Secondary header: not set.
- Packet data field length: 16 bytes.
- Payload encoding: four big-endian scalars:

| Offset | Field | Type | Meaning |
|--------|-------|------|---------|
| `0..4` | `seq` | `u32` | Monotonic frame index for the simulator run. |
| `4..8` | `eps_bus_voltage_v` | `f32` | Abstract EPS bus voltage. |
| `8..12` | `thermal_panel_c` | `f32` | Abstract panel temperature. |
| `12..16` | `body_rate_deg_s` | `f32` | Abstract body rate. |

Simulation steps are 1 ms of **simulation time**, not wall-clock pacing, so short runs complete
quickly and are suitable for tests.

## Smoke (automated)

Integration tests in `crates/chronus-hil-sim/tests/hil_ingest.rs`:

- `nexosim_smoke_reaches_ingest_and_parse` — NeXosim → loopback UDP → real `ingest::run` → parse.
- `nexosim_soak_bounded_recv_errors` — 400 frames; asserts `recv_errors == 0` and `frames_received == N`.

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

The gateway metrics remain zero unless a WebSocket client is connected to `/telemetry/openmct`
while frames are arriving, because parsing/validation for client distribution happens on the
WebSocket path.

## Operational checks

Use this checklist during a profiling run:

1. `ingest.frames_received` equals the requested simulator frame count.
2. `ingest.recv_errors` remains `0`.
3. `ingest.oversized_dropped` remains `0` for the default 16-byte payload.
4. With a WebSocket client connected, `gateway.telemetry_frames_emitted` and
   `gateway.ws_messages_sent` increase together.
5. If `gateway.anomaly_frames` increases, inspect `physics_flags`; the default ISS TLE and
   synthetic station can legitimately trip the below-horizon bit during some runs.

## Troubleshooting

- **No frames received:** confirm the gateway is running first and that the simulator destination
  exactly matches the UDP bind address (`127.0.0.1:7301` by default).
- **Bind failure:** another local process is using the UDP or HTTP port. Stop the conflict or use
  custom `IngestConfig` values in an embedding harness.
- **Metrics show only ingest counters:** connect a WebSocket client before sending frames if you
  need parse/validation latency samples.
- **Cargo cannot resolve Ephemerust:** this workspace expects a sibling checkout at
  `../Ephemerust`; see the root README setup notes.
- **Do not compare wall-clock throughput to frame count directly:** NeXosim advances simulation
  time internally and does not sleep 1 ms between packets in this harness.

## References

- [`crates/chronus-hil-sim/src/lib.rs`](../crates/chronus-hil-sim/src/lib.rs) — simulator model,
  payload layout, and UDP bridge.
- [`crates/chronus-hil-sim/tests/hil_ingest.rs`](../crates/chronus-hil-sim/tests/hil_ingest.rs) —
  deterministic smoke and soak coverage.
- [`crates/gateway/src/ccsds.rs`](../crates/gateway/src/ccsds.rs) — CCSDS synthetic TM encoder.
- [`crates/gateway/src/http.rs`](../crates/gateway/src/http.rs) — metrics response and WebSocket
  distribution path.
- [NeXosim](https://github.com/asynchronics/nexosim) — discrete-event simulation framework used by
  the HIL driver.

## Acknowledgements

Thanks to the **NeXosim** project for the discrete-event simulation framework, **CCSDS** for the
open packet standards this harness exercises, and the ChronusGateway-RS / Ephemerust maintainer
for the shared synthetic telemetry and astrodynamics foundation.

**Credit:** [NeXosim](https://github.com/asynchronics/nexosim) — MIT OR Apache-2.0.

*Last updated: 2026-06-03.*
