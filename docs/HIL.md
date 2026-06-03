# Milestone 7 — HIL / NeXosim profiling notes

This document is a **lightweight harness guide** for driving the gateway from the
`chronus-hil-sim` NeXosim bench (synthetic EPS / thermal / ADCS scalars in CCSDS TM over UDP).
It documents the lab path implemented in `crates/chronus-hil-sim`; it is not a live RF or
mission-specific operations guide.

## End-to-end flow

```
SpacecraftDemo
  emits TelemSample every 1 ms of simulation time
        |
        v
UdpDownlinkBridge
  encode_synthetic_tm(apid, seq, payload)
        |
        v
Loopback UDP datagrams (default gateway target 127.0.0.1:7301)
        |
        v
chronus-gateway ingest::run -> ccsds::parse_telemetry
        |
        +--> optional Open MCT WebSocket subscriber (/telemetry/openmct)
        +--> metrics snapshot (/api/v1/chronus/metrics)
```

The HIL binary uses APID `0x7B0` by default and accepts:

```bash
cargo run -p chronus-hil-sim --release -- [DEST] [FRAMES]
```

- `DEST`: UDP destination, default `127.0.0.1:7301`.
- `FRAMES`: number of synthetic TM packets to emit, default `100`.

## Synthetic payload layout

`TelemSample::to_payload_bytes()` packs a 16-byte CCSDS packet data field in big-endian order:

| Offset | Type | Field | Meaning |
|--------|------|-------|---------|
| `0..4` | `u32` | `seq` | Monotonic frame index for this simulator run. |
| `4..8` | `f32` | `eps_bus_voltage_v` | Abstract EPS bus voltage in volts. |
| `8..12` | `f32` | `thermal_panel_c` | Abstract panel temperature in degrees C. |
| `12..16` | `f32` | `body_rate_deg_s` | Abstract body rate about a nominal axis in deg/s. |

The bridge wraps that payload with `chronus_gateway::encode_synthetic_tm`, which writes a CCSDS
Space Packet primary header, marks the packet as telemetry (TM), and uses an unsegmented sequence
count (`sample.seq & 0x3FFF`). This helper is for synthetic lab/HIL traffic only; it is not a PUS
or mission secondary-header encoder.

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

```bash
curl -s http://127.0.0.1:8080/api/v1/chronus/metrics
```

Representative response shape (field names are stable; counts depend on the run):

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

The UDP ingest counters update as soon as datagrams arrive. Gateway/WebSocket counters update from
the `/telemetry/openmct` subscriber loop, so connect a WebSocket client during the HIL run if you
need `telemetry_frames_emitted`, `ws_messages_sent`, or latency samples.

All telemetry is **synthetic** (see `AGENTS.md`).

## References

- `crates/chronus-hil-sim/src/lib.rs` — `SpacecraftDemo`, `TelemSample`,
  `UdpDownlinkBridge`, and `run_nexosim_udp_hil`.
- `crates/chronus-hil-sim/tests/hil_ingest.rs` — smoke and 400-frame soak tests against the real
  UDP ingest loop.
- `crates/gateway/src/ccsds.rs` — CCSDS primary-header parser and `encode_synthetic_tm`.
- `crates/gateway/src/http.rs` and `crates/gateway/src/metrics.rs` — metrics response fields and
  Open MCT WebSocket fan-out behavior.
- `Methodology.md` D-010, D-013, and D-014 — packet parsing, distribution contract, and HIL
  design rationale.
- [CCSDS Space Packet Protocol](https://public.ccsds.org/) — public open standard used for packet
  framing.
- [NeXosim](https://github.com/asynchronics/nexosim) — discrete-event simulation framework used by
  the HIL harness.

## Acknowledgements

Thanks to the NeXosim project for the discrete-event simulation framework, the CCSDS community for
the public packet standards, the `spacepackets` crate maintainers for Rust CCSDS parsing support,
and the ChronusGateway/Ephemerust author for the synthetic lab harness and propagator boundary.

*Last updated: 2026-06-03.*
