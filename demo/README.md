# Demo & showcase assets (workspace root)

This directory holds **Showcase S1–S3** assets: Docker spine, **Vite dashboard**, **UDP replay** fixtures, and Open MCT backlog notes.
It is **not** shipped inside crates.io packages (`Methodology.md` **D-025**).

## Quick links

| Doc / file | Purpose |
|------------|---------|
| [`../docs/DEMO.md`](../docs/DEMO.md) | **Operator runbook** — native two-terminal flow, Docker Compose, curls, WebSocket expectations, troubleshooting |
| [`docker-compose.yml`](docker-compose.yml) | `gateway` + one-shot `hil-feeder` (Compose network) |
| [`dashboard/README.md`](dashboard/README.md) | **S2** Vite + TypeScript UI (`npm run dev`) |
| [`replay/README.md`](replay/README.md) | **S3** synthetic TM UDP replay (`chronus-replay`, hex/JSONL fixtures) |
| [`openmct/README.md`](openmct/README.md) | Open MCT adapter backlog (**Track A**) |
| [`Dockerfile`](Dockerfile) | Multi-stage image: clone upstream Ephemerust, build both binaries |
| [`gateway.docker.toml`](gateway.docker.toml) | Ingest + HTTP bind on `0.0.0.0` for container networking |

## One-liner (Docker)

From **repository root**:

```bash
docker compose -f demo/docker-compose.yml up -d --build --wait
```

Then `curl http://127.0.0.1:8080/health` and open `ws://127.0.0.1:8080/telemetry/openmct`.

## Separate download (future)

Optional **GitHub Release** zip containing only `demo/**` + pointers to crates.io — see
*Crates.io vs showcase distribution* in [`../docs/SHOWCASE_PLAN.md`](../docs/SHOWCASE_PLAN.md).
