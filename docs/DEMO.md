# ChronusGateway-RS — Demo runbook (Showcase **S1** + **S2**)

Two ways to run the gateway stack: **native Rust** (needs Ephemerust as a sibling checkout) or **Docker Compose** (Ephemerust is cloned during `docker build` inside the image). **Showcase S2** adds a **Vite dashboard** under [`demo/dashboard/`](../demo/dashboard/). Full acceptance checklists: [`Demo_Test.md`](Demo_Test.md).

---

## Ports and URLs (defaults)

| Surface | URL / address |
|--------|----------------|
| UDP ingest | `127.0.0.1:7301` (native default); Docker publishes `7301/udp` on the host |
| HTTP health | `http://127.0.0.1:8080/health` |
| Metrics JSON | `http://127.0.0.1:8080/api/v1/chronus/metrics` |
| Open MCT WebSocket | `ws://127.0.0.1:8080/telemetry/openmct` |

---

## Path A — Native (two terminals)

**Prerequisites:** Rust **1.89+**, **Ephemerust** as a sibling directory (`../Ephemerust` from this repo). See [`README.md`](../README.md).

**Terminal 1 — gateway**

```bash
cd chronus-gateway
cargo run -p chronus-gateway
```

**Terminal 2 — synthetic HIL feeder**

```bash
cd chronus-gateway
cargo run -p chronus-hil-sim --release -- 127.0.0.1:7301 200
```

**Checks**

```bash
curl -sS http://127.0.0.1:8080/health
curl -sS http://127.0.0.1:8080/api/v1/chronus/metrics
```

Open a WebSocket client on `ws://127.0.0.1:8080/telemetry/openmct` (e.g. [`websocat`](https://github.com/vi/websocat), browser devtools, or the **S2** dashboard in [Path C](#path-c--demo-dashboard-vite)). After frames arrive, each **text** message is one JSON object. Expect at least:

- `"chronus_schema":"openmct.realtime.v1"` (string)
- `"apid"`, `"seq_count"`, `"physics_flags"` (numbers / small integer)
- `"payload_base64"` (string)
- Optional geometry: `"elevation_deg"`, `"azimuth_deg"`, `"range_km"`, `"range_rate_km_s"` when the propagator started successfully

---

## Path B — Docker Compose

**Prerequisites:** [Docker](https://docs.docker.com/get-docker/) with Compose v2 (`docker compose …`).

From the **repository root** (`chronus-gateway/`):

```bash
docker compose -f demo/docker-compose.yml up -d --build --wait
```

- Builds an image that **clones upstream Ephemerust** inside the build stage (same public repo as CI), then compiles `chronus-gateway` + `chronus-hil-sim`.
- Starts **gateway** (HTTP `0.0.0.0:8080`, UDP `0.0.0.0:7301` via [`demo/gateway.docker.toml`](../demo/gateway.docker.toml)) and **hil-feeder** (sends 500 frames to `gateway:7301` on the Compose network).

**Checks** (same as native, on the host):

```bash
curl -sS http://127.0.0.1:8080/health
curl -sS http://127.0.0.1:8080/api/v1/chronus/metrics
```

**Tear down**

```bash
docker compose -f demo/docker-compose.yml down
```

**Validate compose file only** (no build; used in CI):

```bash
docker compose -f demo/docker-compose.yml config --quiet
```

---

## Optional: TOML config (native)

```bash
cargo run -p chronus-gateway -- --config gateway.example.toml
```

Docker path uses only [`demo/gateway.docker.toml`](../demo/gateway.docker.toml) (ingest binds on all interfaces).

---

## Path C — Demo dashboard (Vite, Showcase **S2**)

**Prerequisites:** [Node.js](https://nodejs.org/) **22 LTS** (or newer **Active LTS**, e.g. 24) and npm. Avoid **EOL** majors (e.g. 20 after 2026-04-30) for security patches.

With the gateway (and UDP feeder) already running per **Path A** or **Path B**:

```bash
cd demo/dashboard
npm install
npm run dev
```

Open the URL Vite prints (typically `http://127.0.0.1:5173`). Click **Connect** (default WebSocket URL matches the gateway). You should see **latest frame** fields update and **`physics_flags`** rendered as alarm badges when non-zero.

- **Override URL:** `http://127.0.0.1:5173/?ws=ws://127.0.0.1:8080/telemetry/openmct` or `.env.local` with `VITE_GATEWAY_WS=…` (see [`demo/dashboard/README.md`](../demo/dashboard/README.md)).
- **Static build:** `npm run build` → output in `demo/dashboard/dist/`.

---

## Troubleshooting

| Symptom | Likely cause | What to try |
|--------|----------------|-------------|
| `cargo build` cannot find `ephemerust` | Missing sibling checkout | Clone [`Ephemerust`](https://github.com/IsomorphicAlgo/Ephemerust) next to this repo (`../Ephemerust`). |
| Windows link / access denied | MSVC `link.exe` | See [`AGENTS.md`](../AGENTS.md) owner scratchpad and [`Methodology.md`](../Methodology.md) **D-008** (rust-lld). |
| Docker build slow first time | Compiling Rust + deps | Normal; later builds use layer cache. Ensure [`.dockerignore`](../.dockerignore) is present. |
| `curl` health fails on host with Docker | Gateway not ready | Wait for healthcheck / `docker compose ps`; increase `start_period` in [`demo/docker-compose.yml`](../demo/docker-compose.yml) on slow disks. |
| WebSocket connects but no messages | No UDP source | Run `chronus-hil-sim` (native path) or confirm `hil-feeder` in Compose exited **0** (`docker compose ps`); re-run compose or raise the frame count in [`demo/docker-compose.yml`](../demo/docker-compose.yml). |

| Dashboard shows “WebSocket error” | Wrong URL / gateway down | Confirm `curl` health; try `?ws=` override; check browser console. |
| `npm install` fails | Node too old / EOL | Install a **supported LTS** from [nodejs.org](https://nodejs.org/) (e.g. **22** or **24**). |

---

## Compliance

Use **synthetic** HIL traffic and **public reference** TLE defaults only for public demos — see [`AGENTS.md`](../AGENTS.md).

---

*Companion: [`SHOWCASE_PLAN.md`](SHOWCASE_PLAN.md) (S1–S2), [`Demo_Test.md`](Demo_Test.md).*
