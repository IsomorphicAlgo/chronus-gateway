# Milestone 7 - HIL / NeXosim runbook

This runbook covers the implemented laptop-scope hardware-in-the-loop (HIL) path:
`chronus-hil-sim` drives the gateway with **synthetic** EPS / thermal / ADCS scalars packed into
CCSDS telemetry Space Packets over UDP. It is intended for gateway profiling and integration
checks, not mission data replay (see `AGENTS.md`).

## Architecture

```
NeXosim SpacecraftDemo
  -> TelemSample { seq, eps_bus_voltage_v, thermal_panel_c, body_rate_deg_s }
     -> UdpDownlinkBridge + encode_synthetic_tm(apid=0x7B0)
        -> UDP 127.0.0.1:7301
           -> chronus-gateway ingest -> CCSDS parse -> physics validation -> WebSocket JSON
```

Verified source paths:

- Simulator: `crates/chronus-hil-sim/src/lib.rs` and `src/main.rs`.
- UDP ingest and metrics: `crates/gateway/src/ingest.rs`, `metrics.rs`, and `http.rs`.
- Synthetic CCSDS encoder/parser: `crates/gateway/src/ccsds.rs`.

## Public interfaces

### Simulator CLI

```bash
cargo run -p chronus-hil-sim --release -- [DEST] [FRAMES]
```

- `DEST`: gateway UDP ingest address, default `127.0.0.1:7301`.
- `FRAMES`: number of synthetic TM packets to emit, default `100`.
- APID is fixed by the binary at `0x7B0`; sequence count is `seq & 0x3FFF`.
- Simulation steps are 1 ms of **simulation time**, not wall time, so test runs complete quickly.

Each payload is 16 bytes of big-endian values:

| Offset | Type | Field |
|--------|------|-------|
| 0 | `u32` | Monotonic sample sequence |
| 4 | `f32` | Synthetic EPS bus voltage [V] |
| 8 | `f32` | Synthetic panel temperature [deg C] |
| 12 | `f32` | Synthetic body rate [deg/s] |

### Gateway endpoints used during HIL

Run `cargo run -p chronus-gateway` first. Defaults are loopback-only:

- UDP ingest: `127.0.0.1:7301`.
- HTTP/WebSocket: `127.0.0.1:8080`.
- `GET /health` returns `{ "status": "ok" }`.
- `GET /api/v1/chronus/metrics` returns ingest counters, gateway counters, and
  `avg_processing_latency_ms`.
- `GET /telemetry/openmct` upgrades to a WebSocket; each valid TM frame becomes one JSON text
  message with `chronus_schema: "openmct.realtime.v1"`, CCSDS header fields, `physics_flags`,
  optional look-angle fields, and `payload_base64`.
- `GET /api/v1/chronus/openmct/dictionary` and `GET /api/v1/chronus/history` are stubs.

**Important:** UDP ingest counters increment as soon as datagrams arrive. WebSocket/gateway
counters (`telemetry_frames_emitted`, `ws_messages_sent`, anomaly counts, latency samples) advance
only while a WebSocket client is connected, because parsing/validation for distribution is driven
from the subscriber loop.

## Automated smoke and soak

Integration tests in `crates/chronus-hil-sim/tests/hil_ingest.rs`:

- `nexosim_smoke_reaches_ingest_and_parse` - NeXosim -> loopback UDP -> real `ingest::run` -> parse.
- `nexosim_soak_bounded_recv_errors` - 400 frames; asserts `recv_errors == 0` and
  `frames_received == N`.

Run the HIL test slice directly:

```bash
cargo test -p chronus-hil-sim --test hil_ingest
```

## Manual profiling workflow

1. Start the gateway:

   ```bash
   RUST_LOG=info cargo run -p chronus-gateway
   ```

2. Optional but recommended for end-to-end distribution metrics: connect an Open MCT adapter or a
   WebSocket client to `ws://127.0.0.1:8080/telemetry/openmct`.

3. In another shell, send synthetic telemetry:

   ```bash
   cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 5000
   ```

4. Poll metrics:

   ```bash
   curl -s http://127.0.0.1:8080/api/v1/chronus/metrics
   ```

   Fields to watch:

   - `ingest.frames_received` and `ingest.bytes_received`: UDP delivery into the gateway.
   - `ingest.recv_errors` and `ingest.oversized_dropped`: should remain bounded/zero for local HIL.
   - `gateway.telemetry_frames_emitted` and `gateway.ws_messages_sent`: WebSocket distribution.
   - `gateway.telemetry_parse_errors`: malformed or truncated CCSDS packets on the distribution path.
   - `avg_processing_latency_ms`: receive-to-JSON latency average, present after at least one
     distributed frame.

Record machine-specific throughput and latency numbers in your own run log; this repository does
not publish fixed performance figures.

## Constraints and known gaps

- All examples must stay synthetic/public. Do not use real mission keys, operational frequencies,
  controlled performance data, or controlled spacecraft parameters.
- The HIL bridge currently ignores individual UDP `send_to` errors; use ingest metrics to confirm
  delivery.
- The real-time WebSocket path currently passes `RfMetadata::default()`, so Doppler bit 0 is not set
  from live SDR measurements. Elevation/horizon bit 1 can still be set from propagator geometry.
- The history and Open MCT dictionary endpoints are placeholders until persistence and a formal
  telemetry dictionary land.
- The default station/TLE are public ISS demo values in `StationConfig::default`; configure a
  different public TLE or file source in code before making scenario-specific claims.

## Troubleshooting

| Symptom | Likely cause | Check / fix |
|---------|--------------|-------------|
| `cargo` cannot resolve `ephemerust` | Missing sibling checkout | Place `Ephemerust` next to this repo, matching the README layout. |
| Simulator completes but metrics show `frames_received = 0` | Wrong UDP destination or gateway not running | Confirm gateway logs show `UDP telemetry ingest listening` and run the simulator with `127.0.0.1:7301`. |
| Ingest counters rise but `telemetry_frames_emitted = 0` | No WebSocket subscriber | Connect a client to `/telemetry/openmct`; ingest is intentionally independent of subscribers. |
| `telemetry_parse_errors` rises | Datagram is not valid TM CCSDS or is truncated | Use `encode_synthetic_tm` for lab packets; check `max_datagram_size` and payload length. |
| WebSocket messages omit physics fields | Propagator failed to initialize | Check gateway startup logs for TLE/station validation errors. |
| HTTP or UDP bind fails | Port already in use | Start only one gateway instance or change `IngestConfig` bind addresses in code. |

## References

- CCSDS 133.0-B-2 Space Packet Protocol (public CCSDS standard).
- `spacepackets` crate - CCSDS Space Packet primary-header parsing used by `ccsds.rs`.
- NeXosim project and `nexosim` crate - discrete-event simulation framework used by
  `chronus-hil-sim`.
- Tokio and Axum documentation - async UDP, HTTP, and WebSocket runtime/framework behavior.
- NASA Open MCT documentation - target dashboard shape for the real-time adapter.
- Project decision log: `Methodology.md` entries D-010, D-013, and D-014.

## Acknowledgements

Thanks to the NeXosim maintainers (asynchronics) for the MIT OR Apache-2.0 discrete-event
simulation framework, to the CCSDS community for open packet standards, and to the Tokio/Axum and
Rust aerospace ecosystems for the runtime, protocol, and testing foundations this HIL path builds
on.

*Last updated: 2026-06-03.*
