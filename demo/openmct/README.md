# Open MCT for ChronusGateway (Showcase S2 — Track A)

A real [NASA Open MCT](https://github.com/nasa/openmct) (Apache-2.0, prebuilt **4.3.1** from npm)
wired to the gateway's dictionary, history and realtime stream. Added in 0.5.0 (Tranche 3); the
API it uses is documented in [`docs/OPENMCT.md`](../../docs/OPENMCT.md).

**Not** published on crates.io: like the other demos, it lives under the workspace `demo/` folder
(`Methodology.md` D-025).

| File | What it is |
|---|---|
| `src/chronus-plugin.js` | The plugin: object tree from the dictionary, history provider, one shared WebSocket for realtime, red limits on the alarm bitfields |
| `src/main.js` | Open MCT bootstrap (time conductor: realtime, last 15 minutes) |
| `vite.config.js` | Serves Open MCT's `dist/` and proxies `/api` and `/telemetry` to the gateway |
| `verify_history.py` | Headless check: history loads, realtime arrives, everything is still there after a reload |

## Run

You need Node.js 24 LTS (Open MCT 4.3 declares `node >= 24`) and a gateway with history on (it is
on by default; add `persist_path` to keep it across restarts).

```bash
# terminal 1 — the gateway (any config; for the lab pass below use [time] format="cds",
# max_skew_s=0.0 and [rf] bind_addr="127.0.0.1:7302")
cargo run -p chronus-gateway -- --config gateway.toml

# terminal 2 — Open MCT on http://127.0.0.1:5174
cd demo/openmct
npm install
npm run dev

# terminal 3 — some telemetry: a lab ISS pass with a Doppler fault part-way through
cargo run -p chronus-replay -- -f demo/replay/fixtures/iss_pass_lab.jsonl \
    --rf-dest 127.0.0.1:7302 --delay-ms 30
```

Open the tree: **ISS (ZARYA)** holds one object per telemetry point. Plots and tables first load
history from the gateway, then follow the live stream. Reload the page, or restart the gateway
when `persist_path` is set, and the data is still there.

To use another gateway: `CHRONUS_GATEWAY=http://10.0.0.5:8080 npm run dev`.

## Verify

With the gateway and `npm run dev` running (Python Playwright needed:
`pip install playwright && playwright install chromium`):

```bash
python3 demo/openmct/verify_history.py --expect-at-least 1 \
    --send "cargo run -q -p chronus-replay -- -f demo/replay/fixtures/tm_frames_lab.hex --delay-ms 20"
```

The script needs a gateway configured for the frame fixture (`[framing] mode="tm_frames"`, see
[`docs/TM_TRANSFER_FRAMES.md`](../../docs/TM_TRANSFER_FRAMES.md)), or pass any other `--send`
command that matches your gateway's config.

## Credits

Open MCT, Copyright (c) 2014-2024 United States Government as represented by the Administrator of
NASA, Apache License 2.0. It is used unmodified from npm, and its license and notices ship inside
`node_modules/openmct/`. The plugin follows the provider pattern from the Open MCT tutorial and the
`example/generator` plugin in the Open MCT sources.
