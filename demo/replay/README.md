# Synthetic TM replay (Showcase S3)

Deterministic **UDP replay** of lab-generated CCSDS telemetry: the same bytes every time, for
narrative demos, screenshots and end-to-end tests.

## Fixtures

| File | What it holds | Gateway settings it needs |
|------|---------------|---------------------------|
| [`fixtures/golden_tm.hex`](fixtures/golden_tm.hex), [`golden_tm.jsonl`](fixtures/golden_tm.jsonl) | A few plain Space Packets | defaults |
| [`fixtures/rf_lab.jsonl`](fixtures/rf_lab.jsonl) | 7 packets with CDS onboard time and `chronus.rf.v1` receiver measurements, faults on lines 3–6 (0.4.0, [`docs/RF_SIDE_CHANNEL.md`](../../docs/RF_SIDE_CHANNEL.md)) | `[rf] bind_addr`, `[time] format="cds"`, `max_skew_s=0.0` |
| [`fixtures/iss_pass_lab.jsonl`](fixtures/iss_pass_lab.jsonl) | A whole lab ISS pass: 330 packets 2 s apart, a deliberately wrong carrier on lines 151–175 (Doppler alarm); the first frames are below the horizon (0.5.0, Open MCT demo) | `[rf] bind_addr`, `[time] format="cds"`, `max_skew_s=0.0` |
| [`fixtures/tm_frames_lab.hex`](fixtures/tm_frames_lab.hex) | 14 **CADUs** (sync marker + randomized 223-octet TM Transfer Frames with CRC): 3 virtual channels, packets spanning frames, idle data, one corrupted frame, junk and split datagrams (0.5.0, [`docs/TM_TRANSFER_FRAMES.md`](../../docs/TM_TRANSFER_FRAMES.md)) | `[framing] mode="tm_frames"`, `frame_length=223`, `asm=true`, `derandomize=true`; `[time] format="cds"`, `max_skew_s=0.0` |

`tm_frames_lab.hex` and `tm_frames_lab.expected.json` (the packets it must decode to) are generated
by [`fixtures/gen_tm_frames.py`](fixtures/gen_tm_frames.py), an independent Python encoder. Rerun it
with `python3 demo/replay/fixtures/gen_tm_frames.py` to regenerate both.

## Fixture format

- **Hex lines:** one datagram per line (whitespace-separated or contiguous hex). That's a Space
  Packet, or a frame / CADU stream chunk when the gateway runs in TM-frame mode.
- **JSONL:** one JSON object per line, either `{"udp_hex": "..."}` or a described packet (`apid`,
  `seq`, `data_hex`, `onboard_time` as RFC 3339 or `"now"`), plus an optional `rf` object (see
  `chronus-replay --help`).

Blank lines and lines starting with `#` are skipped.

## Run

From the repository root, with the gateway's UDP ingest on `127.0.0.1:7301`:

```bash
cargo run -p chronus-replay -- --file demo/replay/fixtures/golden_tm.hex --delay-ms 100 --repeat 2
```

| Flag | Meaning |
|------|---------|
| `-f` / `--file` | Fixture path (required) |
| positional `DEST` | `HOST:PORT` (default `127.0.0.1:7301`) |
| `--delay-ms` | Pause between datagrams |
| `--repeat` | Send the whole sequence N times |
| `--rf-dest HOST:PORT` | Send each line's `rf` object to the gateway's `rf.bind_addr` as `chronus.rf.v1`, just before its packet |
| `--rf-lead-ms` | Pause between an RF sample and its packet (default 5) |
| `--time-format cds\|cuc`, `--tai-minus-utc-s` | Time code for described packets (must match the gateway's `[time]`) |
| `--tm-frames` | Wrap each packet in TM Transfer Frames, flushed per line (gateway: `[framing] mode="tm_frames"`) |
| `--frame-length`, `--scid`, `--vcid` | Frame length (default 223), spacecraft id (default 42), virtual channel (default 0) |
| `--asm`, `--randomize`, `--no-fecf` | Add the sync marker, apply the pseudo-randomizer, omit the CRC |

Don't combine `--tm-frames` with a fixture that already holds frames (`tm_frames_lab.hex`).

## Compliance

Use **only synthetic / lab-generated** payloads (`AGENTS.md`) in **`demo/replay/`**. Everything
here is invented, or computed from the public ISS reference TLE. For **Tier-2** curated public CCSDS
fixtures (ISS / AMSAT tracks), see **`demo/fixtures/`** (Showcase **S4**).
