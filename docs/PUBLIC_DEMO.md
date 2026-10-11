# Public sandbox (Fly.io + your own domain)

A hosted, per-visitor trial of ChronusGateway-RS. Each visitor tab gets a **private, time-accelerated
synthetic spacecraft** whose telemetry runs through the real pipeline (CCSDS parse → physics
co-validation → `openmct.realtime.v1` JSON). Visitors inject faults (Doppler spoof, eclipse
mismatch, antenna error, …) and watch `physics_flags` react. Nobody else sees their faults.

```
Visitor ──https──▶ shintratech.net            Squarespace: story, diagram, "Launch demo" button
   │
   └─────wss────▶ demo.shintratech.net        Fly.io: chronus-demo-server (one small machine)
                    ├─ /            static visitor page (demo/sandbox/index.html)
                    ├─ /sandbox/ws  per-visitor WebSocket (own clock, own gateway state)
                    └─ /health      liveness + session counters
```

The server has **no UDP socket** and accepts **no uploaded data**. The UDP hop is replaced by an
in-process hand-off; every frame is still encoded to CCSDS bytes, parsed and validated by
`chronus_gateway::http::process_frame_with_rf`. Data is synthetic: public ISS TLE, toy subsystem
models.

## Run it locally first

```bash
STATIC_DIR=demo/sandbox cargo run --release -p chronus-demo-server
```

Open `http://127.0.0.1:8080/`. Environment knobs: `BIND`/`PORT`, `STATIC_DIR`, `ALLOWED_ORIGINS`
(comma list; empty = any, fine locally), `MAX_SESSIONS`, `MAX_PER_IP`, `SESSION_SECS`, `TICK_HZ`.

## Deploy to Fly.io

One-time (install `flyctl`, then `fly auth login`):

```bash
cd chronus-gateway
fly launch --no-deploy --copy-config --ha=false     # keep fly.toml; choose a unique app name
fly deploy --ha=false                               # builds demo/Dockerfile.public remotely
curl https://<app>.fly.dev/health
```

If the app name is not `chronus-sandbox`, edit `app` and the `*.fly.dev` entry in `ALLOWED_ORIGINS`
in `fly.toml`, then redeploy. **Keep a single machine**: the session caps are per process.

## Point demo.shintratech.net at it

1. `fly certs add demo.shintratech.net`
2. In the DNS settings for `shintratech.net` (Squarespace Domains if your Google domain was
   migrated there), add: **CNAME**, host `demo`, value `<app>.fly.dev`.
3. `fly certs show demo.shintratech.net` until it reports the certificate as issued (minutes).
4. Visit `https://demo.shintratech.net/`.

The root site on Squarespace is unaffected; only the `demo` host record is new.

## Squarespace page

Single scrolling page: one-line pitch, architecture diagram, the Vitro screenshot, the three-step
explanation from the README, then a **"Launch live demo"** button linking to
`https://demo.shintratech.net/` (opens in a new tab). Embedding the demo in an iframe via a Code
Block is possible on plans that allow code blocks, but the dashboard wants the full screen, so a
button is the dependable option.

## Abuse limits (all configurable)

| Limit | Default | Why |
|-------|---------|-----|
| `MAX_SESSIONS` | 40 | Hard ceiling on CPU/memory; extra visitors get HTTP 503 + `Retry-After` |
| `MAX_PER_IP` | 3 | One visitor cannot fill the server (uses `Fly-Client-IP` on Fly) |
| `SESSION_SECS` | 600 | Sandboxes expire; the page offers a fresh one |
| `ALLOWED_ORIGINS` | your domains | Other websites cannot embed your sandbox in their pages |
| Inbound message size | 1 KiB | Commands are tiny JSON objects |
| Command rate | 10/s | Extra commands are ignored |
| Send timeout | 5 s | A stalled client is dropped, never buffered |
| Scale to zero | on | No traffic, no machine running |

`Origin` checks stop casual cross-site embedding; they are not authentication. Anyone can still open
the page itself, which is the point of a public trial.

## Operating it

```bash
fly logs                      # tracing output
fly status                    # machine state
curl https://demo.shintratech.net/health   # active_sessions / sessions_served
fly releases                  # previous deploys; redeploy an older image to roll back
```

Cost is a single shared-CPU 256 MB machine that stops when idle. Set a billing alert in the Fly
dashboard anyway.

## Roadmap

- **Open MCT front end** in `demo/openmct/`: a plugin that consumes `/sandbox/ws` frames
  (`openmct.realtime.v1`) and the `chronus.sandbox.v1` tick messages, with ready-made layouts.
- Bring-your-own CCSDS frames (size- and rate-capped replay into the visitor's own sandbox).
