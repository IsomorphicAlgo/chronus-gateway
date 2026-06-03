# Milestone 7 - HIL / NeXosim runbook

This guide documents the **implemented** `chronus-hil-sim` hardware-in-the-loop (HIL)
driver. It runs a NeXosim discrete-event model that emits synthetic spacecraft telemetry as
CCSDS Telemetry (TM) Space Packets over UDP into the real gateway ingest path.

All telemetry is **synthetic and generic** (abstract EPS / thermal / ADCS scalars); do not add
real mission keys, frequencies, link budgets, or operational parameters here. See `AGENTS.md`.

## Architecture covered

```
SpacecraftDemo (NeXosim) -> UdpDownlinkBridge -> UDP datagram
    -> chronus-gateway ingest::run -> ccsds::parse_telemetry
    -> /telemetry/openmct WebSocket and /api/v1/chronus/metrics
```

Source code:

- `crates/chronus-hil-sim/src/lib.rs` - `SpacecraftDemo`, `TelemSample`,
  `UdpDownlinkBridge`, and `run_nexosim_udp_hil`.
- `crates/chronus-hil-sim/src/main.rs` - CLI wrapper: `chronus-hil-sim [DEST] [FRAMES]`.
- `crates/gateway/src/ccsds.rs` - `encode_synthetic_tm` and CCSDS TM parsing.
- `crates/gateway/src/http.rs` - Open MCT WebSocket and metrics routes.

## HIL telemetry contract

The simulator sends one UDP datagram per telemetry sample.

| Field | Value / constraint |
|-------|--------------------|
| Destination | CLI `DEST`, default `127.0.0.1:7301` |
| Frame count | CLI `FRAMES`, default `100` |
| Default APID | `0x7B0` in the binary |
| Sequence count | `sample.seq & 0x3FFF` |
| CCSDS type | TM (`PacketType::Tm`), unsegmented, no secondary header |
| Payload length | 16 bytes |
| Payload encoding | Big-endian `u32 seq`, `f32 eps_bus_voltage_v`, `f32 thermal_panel_c`, `f32 body_rate_deg_s` |
| Simulation cadence | 1 ms **simulation time** between events; the CLI can finish much faster than wall-clock frame duration |

The helper `encode_synthetic_tm(apid, seq_count, payload)` is intentionally a lab/HIL encoder,
not a full PUS or mission packet encoder. It asserts that `payload` is non-empty and masks APID
and sequence count to their CCSDS bit widths.

## Automated smoke and soak

Run the Milestone 7 integration suite:

```bash
cargo test -p chronus-hil-sim --test hil_ingest
```

The tests in `crates/chronus-hil-sim/tests/hil_ingest.rs` cover:

- `nexosim_smoke_reaches_ingest_and_parse` - NeXosim -> loopback UDP -> real `ingest::run`
  -> `ccsds::parse_telemetry`.
- `nexosim_soak_bounded_recv_errors` - 400 synthetic frames, APID and payload-length checks,
  `recv_errors == 0`, and `frames_received == N`.

These tests use loopback UDP and deterministic synthetic packets; they do not require SDR
hardware or live network services.

## Manual profiling workflow

1. Start the gateway:

   ```bash
   cargo run -p chronus-gateway
   ```

   Defaults: UDP ingest `127.0.0.1:7301`, HTTP/WebSocket `127.0.0.1:8080`.

2. In another shell, run the simulator:

   ```bash
   cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 5000
   ```

3. Poll metrics:

   ```bash
   curl -s http://127.0.0.1:8080/api/v1/chronus/metrics
   ```

   Response shape:

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

   The exact counters vary by frame count and machine. `bytes_received` is `frames * 22` for
   the default HIL packet shape (6-byte CCSDS primary header + 16-byte payload).

4. To exercise the gateway serialization path, keep a WebSocket client connected to
   `ws://127.0.0.1:8080/telemetry/openmct` while the simulator runs. Gateway counters such as
   `telemetry_frames_emitted`, `ws_messages_sent`, `anomaly_frames`, and latency samples are
   updated from the WebSocket subscriber loop, so they remain zero when no client is connected.

## Operational notes and pitfalls

- Start the gateway before the simulator. The HIL bridge uses UDP; packets sent before the socket
  is listening are lost.
- The simulator uses simulation time, not wall-clock pacing. A large `FRAMES` value can burst
  quickly into the OS UDP buffers; use release mode for profiling and watch `recv_errors` and
  `frames_received`.
- `recv_errors` should stay at zero during normal loopback runs. A non-zero value indicates a
  socket receive error; check port conflicts, firewall/container networking, and whether another
  process is bound to `127.0.0.1:7301`.
- `oversized_dropped` should stay at zero for HIL packets. The default packet is 22 bytes, far
  below `IngestConfig::max_datagram_size` (`65_542` bytes).
- If `ingest.frames_received` increases but WebSocket counters do not, connect a WebSocket
  client; metrics for parse/validation/serialization are intentionally tied to live subscribers.
- The current binary exposes destination and frame count only. APID is fixed to `0x7B0` in
  `crates/chronus-hil-sim/src/main.rs`; use the library API for tests that need a different APID.

## References

- CCSDS 133.0-B-2 Space Packet primary-header model, via the `spacepackets` crate used in
  `crates/gateway/src/ccsds.rs`.
- `spacepackets` crate documentation and source for CCSDS packet parsing behavior.
- NeXosim crate documentation and source for the discrete-event model, ports, mailboxes, and
  simulation runner used by `chronus-hil-sim`.
- Gateway metrics and WebSocket behavior verified against `crates/gateway/src/http.rs`,
  `crates/gateway/src/metrics.rs`, and `crates/gateway/src/ingest.rs`.

## Acknowledgements

- [NeXosim](https://github.com/asynchronics/nexosim) by asynchronics - the MIT OR Apache-2.0
  discrete-event simulation framework that powers this HIL driver.
- [CCSDS](https://public.ccsds.org/) - the open international standards body whose Space Packet
  conventions define the synthetic TM framing.
- The `spacepackets` maintainers - the CCSDS/ECSS parsing crate used by the gateway.

*Last updated: 2026-06-03.*
