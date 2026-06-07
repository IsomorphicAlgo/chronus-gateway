# Milestone 7 — HIL / NeXosim profiling notes

This document is a **lightweight harness guide** for driving the gateway from the
`chronus-hil-sim` NeXosim bench (synthetic EPS / thermal / ADCS scalars in CCSDS TM over UDP).

## Synthetic payload layout (**CV-3** / `chronus.hil.tm.v1`)

The NeXosim driver packs each `TelemSample` (`crates/chronus-hil-sim/src/lib.rs`) into a **fixed 24-byte** CCSDS packet data field using
`chronus_gateway::hil_tm::encode_hil_tm_v1_payload` (`crates/gateway/src/hil_tm.rs`) (big-endian; magic **`CHI1`**, version byte **`1`**, three zero reserved bytes, then `seq` + three `f32` demo fields).

Decode on the gateway side with `decode_hil_tm_v1` on `tm.payload()` after CCSDS parse. **APID policy:** synthetic HIL frames are expected on APIDs in the inclusive range configured as `StationConfig::hil_tm_v1_apid_min` … `hil_tm_v1_apid_max` (defaults **0x7B0…0x7BF**; see `gateway.example.toml` optional keys).

## CV-4 self-consistency (NeXosim)

The simulator advances a synthetic UTC clock from **`2020-07-12T21:00:00Z`** at **1 ms** per frame (see `SpacecraftDemo` in `crates/chronus-hil-sim/src/lib.rs`), evaluates `chronus_gateway::nadir_sun_illumination_cos` with the **same public ISS TLE** as `StationConfig::default`, and fills `eps_bus_voltage_v` / `thermal_panel_c` with the **same linear maps** as the gateway’s default `StationConfig` CV-4 endpoints. When you connect Open MCT to the WebSocket path with the default station, HIL frames on APIDs **0x7B0…0x7BF** should therefore keep **physics_flags** bits **4–5** clear unless the gateway TLE or CV-4 tunables are intentionally diverged, or you enable a **scripted anomaly** window (see below).

## Scripted anomaly window (Showcase S3)

For repeatable **`physics_flags`** demos without editing replay fixtures, the sim can force **synthetic**
subsystem values **out of band** for a contiguous range of **0-based frame indices** (`TelemSample::seq`):

| CLI | Effect on wire | Expected gateway flag (default station) |
|-----|----------------|------------------------------------------|
| `--scripted-anomaly eps-voltage` | EPS **10 V** (vs illumination model ~24–28 V) | CV-4 — bit **4** |
| `--scripted-anomaly thermal` | Panel **80 °C** (vs model band) | CV-4 — bit **5** |
| `--scripted-anomaly body-rate` | **99 deg/s** body rate (vs default ceiling **5** deg/s) | CV-5 — bit **6** |

**Window:** `--anomaly-after-frame N` (first corrupted index, default **50**) and `--anomaly-frame-count M` (**M = 0** disables injection even if a kind is set). APID override: `--apid 0x7B0` (decimal also accepted).

Example (body-rate excursion then nominal):

```bash
cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 500 \
  --scripted-anomaly body-rate --anomaly-after-frame 100 --anomaly-frame-count 50
```

**Note:** Doppler / link-budget / pointing bits (**0–3**) are driven by optional **RF metadata** on the gateway path; this HIL hook covers **subsystem CV-4/CV-5** only (synthetic, `AGENTS.md`-safe).

## Smoke (automated)

Integration tests in `crates/chronus-hil-sim/tests/hil_ingest.rs`:

- `nexosim_smoke_reaches_ingest_and_parse` — NeXosim → loopback UDP → real `ingest::run` → parse + **HIL v1** decode when APID is in the default band.
- `nexosim_scripted_body_rate_window_on_wire` — scripted **CV-5** injection window; decodes **99 deg/s** on wire for `seq ∈ [start, start+duration)`.

1. Start the gateway: `cargo run -p chronus-gateway` (UDP default `127.0.0.1:7301`, HTTP `127.0.0.1:8080`).
2. In another shell, run the sim (release optional):  
   `cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 5000`
3. Poll `GET http://127.0.0.1:8080/api/v1/chronus/metrics` for ingest + gateway counters and average
   processing latency (document numbers in your own run log; figures vary by machine).

All telemetry is **synthetic** (public demo / compliance posture; see repository README).

**Credit:** [NeXosim](https://github.com/asynchronics/nexosim) — MIT OR Apache-2.0.

*Last updated: 2026-06-04.*
