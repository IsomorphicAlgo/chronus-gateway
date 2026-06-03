# Milestone 7 — HIL / NeXosim profiling notes

This document is a **lightweight harness guide** for driving the gateway from the
`chronus-hil-sim` NeXosim bench (synthetic EPS / thermal / ADCS scalars in CCSDS TM over UDP).

## Scope and constraints

- All telemetry is **synthetic** and generic; do not use mission-specific values, real keys,
  controlled frequencies, or operational parameters (see `AGENTS.md`).
- The HIL driver sends UDP datagrams only. UDP is fire-and-forget, so start the gateway before
  starting the simulator.
- NeXosim advances in **simulation time**. The model schedules one synthetic frame every 1 ms of
  simulation time, but `run_nexosim_udp_hil` returns as fast as the local process can execute.

## Smoke (automated)

Integration tests in `crates/chronus-hil-sim/tests/hil_ingest.rs`:

- `nexosim_smoke_reaches_ingest_and_parse` — NeXosim → loopback UDP → real `ingest::run` → parse.
- `nexosim_soak_bounded_recv_errors` — 400 frames; asserts `recv_errors == 0` and `frames_received == N`.

Run just the HIL tests with:

```bash
cargo test -p chronus-hil-sim
```

## Manual profiling with gateway metrics (M6)

1. Start the gateway: `cargo run -p chronus-gateway` (UDP default `127.0.0.1:7301`, HTTP `127.0.0.1:8080`).
2. In another shell, run the sim (release optional):  
   `cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 5000`
3. Poll `GET http://127.0.0.1:8080/api/v1/chronus/metrics` for ingest + gateway counters and average
   processing latency (document numbers in your own run log; figures vary by machine).

```bash
curl http://127.0.0.1:8080/api/v1/chronus/metrics
```

Use `RUST_LOG=debug cargo run -p chronus-gateway` when investigating parse or WebSocket behavior.

## HIL packet contract

The `chronus-hil-sim` binary accepts positional arguments:

```bash
chronus-hil-sim [DEST] [FRAMES]
```

| Argument | Default | Meaning |
|----------|---------|---------|
| `DEST` | `127.0.0.1:7301` | Gateway UDP ingest address. |
| `FRAMES` | `100` | Number of synthetic CCSDS TM packets to emit. |

The binary uses synthetic APID `0x7B0`. Tests also exercise APID `0x7B1`. Sequence counts are
masked to the CCSDS 14-bit sequence field with `sample.seq & 0x3FFF`.

Each packet is produced by `chronus_gateway::encode_synthetic_tm` and carries a 16-byte big-endian
payload:

| Offset | Type | Field |
|--------|------|-------|
| `0..4` | `u32` | `seq` |
| `4..8` | `f32` | `eps_bus_voltage_v` |
| `8..12` | `f32` | `thermal_panel_c` |
| `12..16` | `f32` | `body_rate_deg_s` |

The gateway treats this as an opaque CCSDS packet data field; payload decoding is a simulator
contract for profiling and tests, not a production telemetry dictionary.

## Metrics field guide

`GET /api/v1/chronus/metrics` combines UDP ingest counters with WebSocket/distribution counters:

| Field | Interpretation while profiling |
|-------|--------------------------------|
| `ingest.frames_received` | UDP datagrams accepted and forwarded by `ingest::run`. Should reach `FRAMES`. |
| `ingest.bytes_received` | Sum of accepted datagram byte lengths. HIL packets are `6 + 16` bytes each. |
| `ingest.oversized_dropped` | Oversized datagrams dropped by the OS/loop; should stay `0` for HIL. |
| `ingest.recv_errors` | Non-fatal socket receive errors; the automated soak requires `0`. |
| `gateway.telemetry_frames_emitted` | Valid CCSDS TM frames converted to WebSocket JSON. Requires an active WebSocket client. |
| `gateway.telemetry_parse_errors` | Datagrams rejected by CCSDS parsing on the distribution path. |
| `gateway.anomaly_frames` | Frames whose post-validation `physics_flags` is non-zero. |
| `gateway.ws_messages_sent` | WebSocket JSON text messages successfully sent. |
| `gateway.ws_clients_connected` | Current best-effort WebSocket connection count. |
| `avg_processing_latency_ms` | Receive-to-JSON-ready latency; `null` until a WebSocket client processes a frame. |

For a UDP-only HIL run with no WebSocket client, `ingest.frames_received` can increase while
`gateway.telemetry_frames_emitted` remains `0`. To exercise the WebSocket path, connect a client to
`ws://127.0.0.1:8080/telemetry/openmct` before sending frames.

## Common pitfalls

- **No frames observed:** confirm the gateway started first and is listening on the same UDP address
  passed as `DEST`.
- **Gateway metrics stay at zero:** `/api/v1/chronus/metrics` reports live process state; make sure
  you are polling the same gateway process that receives the HIL datagrams.
- **WebSocket counters stay at zero:** the UDP ingest path does not create WebSocket messages until
  a client is connected to `/telemetry/openmct`.
- **`avg_processing_latency_ms` is `null`:** no WebSocket frame has been serialized yet.
- **Unexpected parse errors:** verify the sender is using `encode_synthetic_tm` or another valid
  CCSDS TM Space Packet. TC packets and malformed/truncated datagrams are intentionally dropped.

## References

- `crates/chronus-hil-sim/src/lib.rs` — `SpacecraftDemo`, `TelemSample`,
  `UdpDownlinkBridge`, and `run_nexosim_udp_hil`.
- `crates/gateway/src/ccsds.rs` — CCSDS parser and `encode_synthetic_tm` helper.
- `crates/gateway/src/http.rs` and `crates/gateway/src/metrics.rs` — metrics endpoint and
  WebSocket distribution counters.
- [NeXosim](https://github.com/asynchronics/nexosim) — discrete-event simulation framework.
- CCSDS Space Packet Protocol, CCSDS 133.0-B series — open packet framing reference.

## Acknowledgements

**Credit:** [NeXosim](https://github.com/asynchronics/nexosim) — MIT OR Apache-2.0.
ChronusGateway-RS also relies on the owner's Ephemerust work for the live tracking state consumed
by gateway validation, and on Tokio/Axum for the asynchronous gateway runtime.

*Last updated: 2026-06-03.*
