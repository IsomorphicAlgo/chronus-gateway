# Receiver data and spacecraft time (`chronus.rf.v1`)

*Added in 0.4.0 (Tranche 2 of the 1.0 plan, Methodology D-047).*

A CCSDS packet can only tell the gateway what the **spacecraft** says. Three of the physics checks
compare the physics model with what the **ground receiver** measured, and a packet cannot carry
that:

| Bit | Check | Needs |
|----:|-------|-------|
| 0 | Doppler | the measured carrier frequency (Hz) |
| 2 | Link budget | the measured received power (dBm) |
| 3 | Pointing | where the antenna was actually pointing (azimuth, elevation) |

Before 0.4.0 these three were only reachable by embedding the library. They now work in the shipped
binary: a receiver sidecar sends **`chronus.rf.v1`** datagrams to a second UDP port and the gateway
joins each sample to the telemetry frame it belongs to.

The same release adds **spacecraft time**: physics can run at the instant the spacecraft stamped on
the packet instead of the instant the datagram arrived.

Both features are **off by default**, so an existing deployment behaves exactly as before.

---

## 1. Turn it on

```toml
[rf]
bind_addr = "127.0.0.1:7302"   # second UDP port; absent = side channel off
window_ms = 250                # match a frame to the nearest sample within ±250 ms
capacity  = 1024               # samples kept; oldest dropped first

[time]
format = "cds"                 # "none" (default) | "cds" | "cuc"
max_skew_s = 5.0               # 0 = lab/replay mode (see §4)
cuc_tai_minus_utc_s = 37       # only for "cuc"
```

Both tables are optional and every key in them is optional. Bind `0.0.0.0` only on a network you
trust: like the telemetry port, the RF port has no authentication.

## 2. The `chronus.rf.v1` datagram

One JSON object per UDP datagram (at most 2 KiB):

```json
{"chronus_schema":"chronus.rf.v1","t":"2026-10-11T12:00:00.250Z",
 "carrier_hz":437512340.5,"rx_power_dbm":-112.4,"az_deg":211.7,"el_deg":34.2}
```

| Field | Type | Meaning |
|-------|------|---------|
| `chronus_schema` | string, **required** | must be exactly `"chronus.rf.v1"` |
| `t` | RFC 3339 UTC, optional | when the measurement was taken. **Omit it** and the gateway stamps the arrival time, which is the right choice for a sidecar on the same network |
| `carrier_hz` | number, optional | measured downlink carrier → Doppler check |
| `rx_power_dbm` | number, optional | measured received power → link-budget check |
| `az_deg`, `el_deg` | numbers, optional | measured look direction (azimuth clockwise from true north, elevation above the horizon) → pointing check. **Both** are needed |

The JSON Schema is published at `GET /api/v1/chronus/schemas/chronus.rf.v1`
([`crates/gateway/schemas/chronus.rf.v1.schema.json`](../crates/gateway/schemas/chronus.rf.v1.schema.json)).

Any subset is fine; a check without its measurement is skipped, not failed. Unknown fields are
ignored, so v1 can grow additively. Datagrams that are not valid JSON, name another schema, or hold
a non-finite number are counted (`rf.samples_invalid`) and dropped.

### How a sample is joined to a frame

Samples go into a bounded in-memory store. Each telemetry frame is matched to the sample **nearest
its receive time within `window_ms`**.

- No sample in range: the frame is processed without the RF checks and counted as unmatched.
- Matching never consumes a sample, so every connected dashboard sees the same result.
- The store is capped at `capacity`; the oldest sample is evicted first (`rf.samples_evicted`).

Because matching is by time, **send the sample just before its frame**, and pace multiple frames at
least a few milliseconds apart. If the sender's clock differs from the gateway's, leave `t` out.
`chronus-replay` and `chronus-hil-sim` do both for you.

The Open MCT message gains `rf_matched` (whether any measurement was available for the frame).
`GET /api/v1/chronus/metrics` gains an `rf` object (`enabled`, `samples_received`,
`samples_invalid`, `samples_evicted`, `frames_matched`, `frames_unmatched`), and `/status` explains
the common problems:

| Code | Meaning |
|------|---------|
| `CG-511` | telemetry flows but no RF samples arrive, or most frames find none in the window (sender down, wrong port, clock offset) |
| `CG-512` | something is sending invalid datagrams to the RF port |
| `CG-129` | an `[rf]` / `[time]` setting is out of range (startup error) |

## 3. Spacecraft time codes

With `time.format` set, a packet whose **secondary-header flag** is set is expected to begin its data
field with a 6-byte time code (CCSDS 301.0-B-4, agency-defined P-field):

| Format | Layout (big-endian) | Scale |
|--------|---------------------|-------|
| `cds` | 16-bit days + 32-bit milliseconds of day since 1958-01-01 | UTC |
| `cuc` | 32-bit seconds + 16-bit fraction (1/65536 s) since 1958-01-01 | TAI (`cuc_tai_minus_utc_s` is subtracted) |

The gateway then:

1. evaluates physics at the **onboard time** (`physics_time` in the JSON) when it is trusted;
2. falls back to the **receive time** when it is not;
3. strips the time field before interpreting the payload, so the application bytes (for example the
   synthetic HIL payload) still decode.

Packets with no secondary header are untouched and carry no time.

### `physics_flags_v2`

Time findings do not fit the eight `physics_flags` bits (D-016 reserves bit 7), so they live in a
new field, **`physics_flags_v2`** (0 when time decoding is off):

| Bit | Value | Meaning | Physics runs at |
|----:|------:|---------|-----------------|
| 0 | `0x01` | **Time-tag skew:** onboard and receive time differ by more than `max_skew_s` | receive time |
| 1 | `0x02` | **Time code invalid:** the packet has a secondary header but the time field is too short or out of range | receive time |

A frame with either bit set counts toward `anomaly_frames`. The JSON also reports `onboard_time`
(when decoded) and `physics_time`.

## 4. Lab and replay mode (`max_skew_s = 0`)

A recording or a simulation is not "now". With `max_skew_s = 0` the skew check is off and the
onboard time is trusted as stamped, so a recorded stream is validated at *its own* mission time
against a TLE or OEM that covers it. This is how the shipped fixture and the HIL simulator work.

For live data keep a positive `max_skew_s` (the default is 5 s): it catches a spacecraft clock that
has drifted or reset, which would otherwise silently shift every Doppler and pointing prediction.

## 5. Drive it without a radio

**Replay a recording** (`demo/replay/fixtures/rf_lab.jsonl` holds seven frames with receiver
measurements consistent with the public ISS TLE, some with deliberate faults):

```bash
# gateway: [rf] bind_addr = "127.0.0.1:7302"; [time] format = "cds", max_skew_s = 0.0
cargo run -p chronus-gateway
cargo run -p chronus-replay -- --file demo/replay/fixtures/rf_lab.jsonl \
    --rf-dest 127.0.0.1:7302 --delay-ms 200
# physics_flags per frame: 0x00 0x00 0x01 0x04 0x08 0x0d 0x00
```

A fixture line is either `{"udp_hex": "..."}` or a described packet, plus an optional `rf` object:

```json
{"apid":256,"seq":3,"onboard_time":"2020-07-13T05:08:30.000Z","data_hex":"48454C4C4F",
 "rf":{"carrier_hz":437510641.9,"rx_power_dbm":-105.81,"az_deg":226.576,"el_deg":30.972}}
```

`"onboard_time": "now"` stamps the send time; `--time-format cuc` switches the time code.

**Run the synthetic spacecraft with a simulated receiver** (needs the gateway built with
`--features hil` for the subsystem bits):

```bash
# gateway config: [rf] bind_addr = "127.0.0.1:7302"; [time] format = "cds", max_skew_s = 0.0
cargo run -p chronus-gateway --features hil
cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 200 \
    --time-code cds --epoch-visible --rf-dest 127.0.0.1:7302 \
    --rf-anomaly doppler --rf-anomaly-after-frame 50 --rf-anomaly-frame-count 20 \
    --scripted-anomaly body-rate --anomaly-after-frame 120 --anomaly-frame-count 20
```

`--epoch-visible` starts the mission clock on a pass (the ISS at least 10° above the default
station), so nominal frames raise no horizon alarm. `--rf-dest` also paces the run at 20 ms per
frame so every sample is matched to its own frame.

**Reach all seven bits** in the shipped binary: the Doppler, link-budget and pointing faults
(`--rf-anomaly doppler|link-budget|pointing`), the subsystem faults (`--scripted-anomaly
eps-voltage|thermal|body-rate`), and the horizon bit by starting the epoch below the horizon.
`crates/gateway/tests/live_rf_e2e.rs` does exactly this against the real binary and asserts the
exact flag value for each case, including `0x7F` for all seven at once.

## 6. Writing your own sidecar

Any program that can send a UDP datagram works. A GNU Radio flowgraph, an SDR script or a rotator
controller needs only to emit the JSON above whenever it has a new measurement. From Rust, the
`chronus_gateway::rf::RfSampleV1` type builds and parses the datagram:

```rust
use chronus_gateway::rf::RfSampleV1;

let sample = RfSampleV1::new()
    .with_carrier_hz(437_512_340.5)
    .with_rx_power_dbm(-112.4)
    .with_pointing_deg(211.7, 34.2);
socket.send_to(&sample.to_datagram(), "127.0.0.1:7302")?;
```

`RfSampleV1::nominal_for(&ctx, &tracking_state)` returns what a *perfect* receiver would report, which
is handy for tests and simulators.

## 7. What this does not do

- **No real radio is included.** The shipped sources are a replay tool and a simulator. A live
  receiver sidecar (for example RTL-SDR plus a rotator) is the planned closing step of the 1.0
  plan, where real-RF evidence replaces synthetic evidence.
- **The measurement model is the synthetic one** from D-016: free-space link budget with
  station-side transmit parameters, a constant Doppler tolerance band, no ionosphere.
- **Time codes with a transmitted P-field, other epochs, or sub-millisecond CDS fields** are not
  decoded; the formats above are fixed-length and agency-defined.
- **No authentication on the RF port.** Keep it on loopback or a trusted network until Tranche 4.
