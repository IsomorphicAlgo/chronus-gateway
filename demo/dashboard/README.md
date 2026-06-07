# Chronus demo dashboard (Showcase **S2** — Track B)

Minimal **Vite + TypeScript** UI that subscribes to the gateway WebSocket
`GET /telemetry/openmct` and shows **`physics_flags`** as alarm badges plus latest azimuth, elevation,
range, and range rate.

**Not** published on crates.io — lives under workspace `demo/` per [`Methodology.md`](../../Methodology.md) **D-025**.

## Prerequisites

- Node.js **22 LTS** or newer **Active LTS** (e.g. 24) and npm — avoid EOL lines (Node **20** reached EOL **2026-04-30** per the [official schedule](https://github.com/nodejs/release)).
- Gateway running with HTTP/WebSocket on the URL you configure (default `ws://127.0.0.1:8080/telemetry/openmct`).
- A UDP telemetry source (e.g. `chronus-hil-sim`) so frames appear — see [`../../docs/DEMO.md`](../../docs/DEMO.md).

## Commands

```bash
cd demo/dashboard
npm install
npm run dev
```

Open the printed local URL (usually `http://127.0.0.1:5173`). Override the WebSocket URL:

- **Query:** `http://127.0.0.1:5173/?ws=ws://127.0.0.1:8080/telemetry/openmct`
- **Env:** create `.env.local` with `VITE_GATEWAY_WS=ws://host:port/telemetry/openmct`

**Production build** (static files in `dist/`):

```bash
npm run build
npm run preview   # optional local static preview
```

## Open MCT (Track A)

For NASA Open MCT integration, see [`../openmct/README.md`](../openmct/README.md) (placeholder / backlog).
