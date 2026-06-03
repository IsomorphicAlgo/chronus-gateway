# Milestone 7 — HIL / NeXosim runbook

This document is the operator/developer runbook for driving ChronusGateway-RS from the
`chronus-hil-sim` NeXosim bench. The bench emits **synthetic** EPS / thermal / ADCS scalar samples
as CCSDS telemetry over loopback UDP; it does not model real mission hardware or operational RF
parameters.

## Architecture

```
SpacecraftDemo (NeXosim model)
  └─ emits TelemSample every 1 ms of simulation time
       └─ UdpDownlinkBridge
            └─ encode_synthetic_tm(APID, seq, payload)
                 └─ UDP -> chronus-gateway ingest -> CCSDS parser -> WebSocket / metrics
```

- `SpacecraftDemo` produces one `TelemSample` per frame.
- `UdpDownlinkBridge` sends the sample with a standard `std::net::UdpSocket` to the gateway's UDP
  ingest address.
- `run_nexosim_udp_hil(dest, total_frames, apid)` advances simulation time until all scheduled
  telemetry is emitted. The 1 ms cadence is simulation time, not a wall-clock sleep, so tests finish
  quickly.
- The `chronus-hil-sim` binary accepts `chronus-hil-sim [DEST] [FRAMES]`; defaults are
  `127.0.0.1:7301` and `100` frames, with synthetic APID `0x7B0`.

## Packet contract

Each HIL frame is a CCSDS TM Space Packet encoded by `chronus_gateway::encode_synthetic_tm`.

| Field | Value / constraint |
|-------|--------------------|
| Packet type | TM (`PacketType::Tm`); TC packets are rejected by the gateway ingestion path. |
| APID | Synthetic test APID from the CLI binary (`0x7B0`) or test harness. |
| Sequence count | Lower 14 bits of `TelemSample.seq`. |
| Secondary header | Not set by the helper. |
| Payload length | 16 bytes. |
| Payload encoding | Big-endian `u32 seq`, `f32 eps_bus_voltage_v`, `f32 thermal_panel_c`, `f32 body_rate_deg_s`. |

The payload is deliberately generic: abstract bus voltage, panel temperature, and body-rate scalars
only. Keep future HIL examples synthetic and public-reference friendly per `AGENTS.md`.

## Smoke (automated)

Integration tests in `crates/chronus-hil-sim/tests/hil_ingest.rs`:

- `nexosim_smoke_reaches_ingest_and_parse` — NeXosim → loopback UDP → real `ingest::run` → parse.
- `nexosim_soak_bounded_recv_errors` — 400 frames; asserts `recv_errors == 0` and `frames_received == N`.

Run only the HIL integration tests:

```bash
cargo test -p chronus-hil-sim --test hil_ingest
```

## Manual profiling with gateway metrics (M6)

1. Start the gateway:

   ```bash
   RUST_LOG=info cargo run -p chronus-gateway
   ```

   Defaults: UDP `127.0.0.1:7301`, HTTP/WebSocket `127.0.0.1:8080`.

2. Optional but recommended: connect an Open MCT adapter or generic WebSocket client to
   `ws://127.0.0.1:8080/telemetry/openmct`. Metrics still record UDP ingest without a client, but
   WebSocket emission counters only advance while a subscriber is connected.

3. In another shell, run the sim:

   ```bash
   cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 5000
   ```

4. Poll metrics:

   ```bash
   curl http://127.0.0.1:8080/api/v1/chronus/metrics
   ```

   Watch:

   - `ingest.frames_received` — should increase by the number of generated HIL datagrams.
   - `ingest.recv_errors` — should remain `0` during normal loopback HIL.
   - `gateway.telemetry_frames_emitted` — increases when a WebSocket client is connected and
     packets parse as TM.
   - `gateway.telemetry_parse_errors` — should remain `0` for HIL packets.
   - `avg_processing_latency_ms` — receive-to-JSON average for frames processed by the distribution
     path; document machine-specific numbers in your run log.

## Troubleshooting

- **No frames in metrics:** confirm the gateway is running before the simulator and that both use
  the same UDP destination (`127.0.0.1:7301` by default).
- **`frames_received` increases but `telemetry_frames_emitted` does not:** connect a WebSocket
  client before running the simulator. The broadcast channel does not replay old frames.
- **Parse errors from a custom HIL source:** verify packet type is TM, the CCSDS data-length field
  matches the actual payload, and the packet is at least one byte of data field plus the 6-byte
  primary header.
- **Path dependency build failure:** verify the sibling `../Ephemerust` checkout exists; the
  workspace uses it for propagation.

## References

- `crates/chronus-hil-sim/src/lib.rs` — NeXosim model, UDP bridge, and payload encoding.
- `crates/gateway/src/ccsds.rs` — CCSDS parser and `encode_synthetic_tm` helper.
- `crates/gateway/src/http.rs` and `crates/gateway/src/metrics.rs` — WebSocket and metrics paths.
- [`TEST_PLAN.md`](../TEST_PLAN.md#m7--hil-simulation) — HIL smoke/soak gate.
- [`Methodology.md`](../Methodology.md#d-014--nexosim-hil-driver-milestone-7-closes-od-d-for-single-spacecraft-laptop-scope) — decision record and attribution for the HIL design.

## Acknowledgements

Thanks to the NeXosim project for the discrete-event simulation framework used by this HIL driver,
the CCSDS standards community and `spacepackets` ecosystem for packet-format grounding, and
Ephemerust for the orbital-state backend that the gateway validates against.

**Credit:** [NeXosim](https://github.com/asynchronics/nexosim) — MIT OR Apache-2.0.

*Last updated: 2026-06-03.*
