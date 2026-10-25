# Hardened deployment

*Added in 0.6.0 (Tranche 4 of the 1.0 plan, Methodology D-051, D-052 and D-053).*

This page turns a laptop demo into a station service. It covers who can reach the gateway, how
its data stays current, how to watch it, and how to run it as a container or a systemd service.
Each step can be checked with `chronus-gateway --check-config`, which prints what the gateway
would run with and opens no sockets.

---

## 1. What is exposed, and to whom

| Surface | Default | Protection |
|---|---|---|
| UDP telemetry (`ingest.bind_addr`, 7301) | loopback | `security.udp_allowed_sources` (UDP can't carry a token) |
| UDP receiver data (`rf.bind_addr`, 7302) | off | `security.rf_allowed_sources` |
| HTTP + WebSocket API (`ingest.http_bind`, 8080) | loopback | API tokens, WebSocket `Origin` check, client cap |
| Outbound connections | **none** | the gateway never connects out, unless built with `refresh` and given `tle_url` / `eop_url` |
| Uplink to the spacecraft | **none, by design** | [`TELECOMMAND.md`](TELECOMMAND.md) |

The API is read-only. The risks are who can **read** telemetry, who can **inject** it over UDP,
and whether the service stays **available**.

### API tokens

```bash
chronus-gateway --generate-token > /etc/chronus/api_tokens   # 43 random characters
chmod 600 /etc/chronus/api_tokens
```

```toml
[security]
api_tokens_file = "/etc/chronus/api_tokens"   # one token per line, # comments allowed
```

Tokens can also come from the `CHRONUS_API_TOKENS` environment variable (comma-separated). They
never appear in the config file. With at least one token, **every route except `GET /health`**
needs one:

```bash
curl -H "Authorization: Bearer $TOKEN" https://ops.example.org/api/v1/chronus/status
# A browser cannot set headers on a WebSocket, so it may pass the token in the query instead:
#   wss://ops.example.org/telemetry/openmct?access_token=$TOKEN
```

How tokens are handled:
- Tokens shorter than 32 characters are refused at startup (`CG-138`).
- Comparison is constant-time.
- Query strings are not written to the log.
- Refused requests are counted and explained on `/status` (`CG-515`).

**Bind policy.** The gateway **refuses to start** (`CG-137`) if the API listens on a non-loopback
address with no token configured. If an authenticating proxy is in front of it, say so
explicitly with `security.allow_unauthenticated_remote = true`.

### WebSocket origins and client cap

Browsers let any web page open a WebSocket to any address, including one on the operator's
private network. So the gateway checks the `Origin` header:
- **Allowed:** pages served from this computer, and non-browser clients (no `Origin` header).
- **Also allowed:** the dashboards listed in `allowed_origins`.
- **Refused with `403`:** everything else.

At most `max_ws_clients` sockets can be open at once (default 64). Further connections get `503`.

```toml
[security]
allowed_origins = ["https://ops.example.org"]
max_ws_clients = 64
```

### UDP sender allow-lists

```toml
[security]
udp_allowed_sources = ["192.168.10.20"]                 # the SDR host
rf_allowed_sources  = ["192.168.10.20", "10.0.0.0/24"]  # CIDR works too
```

Datagrams from other addresses are dropped before they are counted or parsed. The first one is
logged; the rest are counted (`security.udp_rejected` / `rf_rejected`). UDP source addresses can
be spoofed, so this is a filter, not authentication. Keep the telemetry ports on a network you
control.

### TLS: use a reverse proxy

TLS is not built into the gateway. Keep `http_bind = "127.0.0.1:8080"` and terminate TLS in a
proxy. This keeps a C-compiled crypto library out of the default build, and certificate renewal
stays with tools designed for it.

**Caddy** (automatic HTTPS):

```caddyfile
ops.example.org {
    reverse_proxy 127.0.0.1:8080
}
```

**nginx:**

```nginx
server {
    listen 443 ssl;
    server_name ops.example.org;
    ssl_certificate     /etc/letsencrypt/live/ops.example.org/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/ops.example.org/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;      # WebSocket
        proxy_set_header Connection "upgrade";
        proxy_set_header Origin $http_origin;
        proxy_read_timeout 1h;
    }
}
```

Both pass `Authorization` and `Origin` through unchanged, so tokens and the origin check still
apply behind the proxy.

## 2. Keeping orbit and Earth-orientation data current

| Data | Goes stale by | Setting |
|---|---|---|
| TLE | 1–3 km/day along track | `station.tle_file` |
| OEM | after its coverage window ends | `station.oem_file` |
| Earth orientation (UT1−UTC, polar motion) | about 1 ms/day (~0.5 m of Earth rotation) | `freshness.eop_file` (an IERS `finals2000A` file such as `finals.daily`) |

Every `freshness.reload_interval_s` seconds (default 60), the gateway checks these files. When
one has changed, it builds a new propagator and swaps it in without dropping a frame:
- With an Earth-orientation file, it also re-applies **today's** values when the UTC day changes.
- A file that fails to load is reported (`CG-516`), and the previous data stays in use.

**Option A: a scheduler (default build).** [`deploy/fetch-data.sh`](../deploy/fetch-data.sh)
downloads one element set from CelesTrak and `finals.daily` from the IERS:
- It writes them only if they look right, so an error page never replaces a good file.
- It sends `If-Modified-Since` and never retries.
- Run it from cron or the systemd timer below.

**Option B: the gateway fetches (the `refresh` feature).**

```bash
cargo build --release -p chronus-gateway --features refresh
```

```toml
[station]
tle_file = "/var/lib/chronus-gateway/tle.txt"

[freshness]
tle_url = "https://celestrak.org/NORAD/elements/gp.php?CATNR=25544&FORMAT=tle"
tle_name = "ISS (ZARYA)"          # which set to keep from a multi-satellite bulletin
eop_file = "/var/lib/chronus-gateway/finals.daily"
eop_url = "https://datacenter.iers.org/data/latestVersion/finals.daily.iau2000.txt"
fetch_interval_h = 6              # minimum 2: CelesTrak's published guidance
```

The fetch behaves as follows:
- **What it accepts:** HTTPS only, at most 2 MiB per response.
- **Retries:** none (CelesTrak blocks clients that hammer it).
- **When it fetches:** at startup only if a file is missing or older than the interval, then on
  the interval.
- **Validation:** a download is checked before it replaces the file.

The feature pulls in `ureq` and `rustls`, which need a C compiler for their crypto backend. That
is why it is off by default.

**Data age is on `/status`.** The `data` object shows:
- the TLE epoch and its age in days;
- the OEM coverage and hours remaining;
- the Earth-orientation source (`file`, `config` or `none`), the values in use, the last day the
  file covers, and the last observed (non-predicted) day;
- reload and fetch counts, plus the last error of each.

The same information drives these findings:

| Code | Raised when |
|---|---|
| `CG-301` / `CG-302` | the TLE is too old for Doppler/pointing, or for anything |
| `CG-322` | the time is outside the OEM coverage |
| `CG-517` | the Earth-orientation file has run out |
| `CG-516` | a reload or fetch failed |

Prometheus gets `chronus_data_tle_age_seconds`, `chronus_data_oem_remaining_seconds` and
`chronus_data_eop_remaining_seconds`.

## 3. Watching it

| Endpoint | For |
|---|---|
| `GET /health` | liveness probes (no token needed) |
| `GET /api/v1/chronus/status` | humans: overall status, plain-English findings with fixes, data age |
| `GET /metrics` | Prometheus text format: every counter, `chronus_health_status` (0 ok … 3 problem), data ages |
| `GET /api/v1/chronus/metrics` | the same counters as JSON |

```yaml
# prometheus.yml
scrape_configs:
  - job_name: chronus
    scheme: https
    authorization:
      credentials_file: /etc/prometheus/chronus_token
    static_configs:
      - targets: ["ops.example.org"]
```

Two alerts worth having:
- `chronus_health_status >= 3`
- `chronus_data_tle_age_seconds > 3 * 86400`

**Logs.** `--log-format json` (or `CHRONUS_LOG_FORMAT=json`) writes one JSON object per line for
Loki, Elasticsearch or journald. `RUST_LOG` sets the level.

## 4. Running it

### Command line

```text
chronus-gateway [--config FILE] [--check-config] [--generate-token] [--log-format text|json]
chronus-gateway --help | --version
```

`--check-config` loads everything the gateway would load: config, tokens, TLE/OEM and the
Earth-orientation file. It prints a summary and every warning, then exits with **0** (ready) or
**2** (must be fixed). Use it in CI, before a restart, or as `ExecStartPre=`.

### Container

[`deploy/Dockerfile`](../deploy/Dockerfile) builds a gateway-only image:
- It runs on distroless (`cc-debian12`), with no shell or package manager.
- It runs as uid 65532.
- It writes JSON logs.
- History persists in the `/var/lib/chronus` volume.

```bash
docker build -f deploy/Dockerfile -t chronus-gateway .
docker run -d --name chronus \
  -p 127.0.0.1:8080:8080 -p 7301:7301/udp \
  -e CHRONUS_API_TOKENS="$(docker run --rm chronus-gateway --generate-token)" \
  -v chronus-data:/var/lib/chronus \
  chronus-gateway
```

Inside the container the gateway listens on all interfaces, so the image **needs a token**.
Without one it exits with `CG-137`. To use your own config, mount it at
`/etc/chronus/gateway.toml`. Tagged releases publish the image as
`ghcr.io/isomorphicalgo/chronus-gateway`. For orchestrators, point the HTTP probe at `/health`.

### systemd

[`deploy/`](../deploy/) has everything for a native install:

| File | Purpose |
|---|---|
| `chronus-gateway.service` | the service: `--check-config` before start, JSON logs, restart on failure, tokens delivered with `LoadCredential=`, state in `/var/lib/chronus-gateway`, full sandboxing (`systemd-analyze security` exposure **1.1, "OK"**) |
| `chronus.sysusers` | the `chronus` system account |
| `gateway.production.toml` | a starting config: API on loopback behind a proxy, tokens, origins, UDP allow-lists, history file, reloaded data files |
| `chronus-fetch-data.service` + `.timer` | runs `fetch-data.sh` every six hours (randomized) |
| `fetch-data.sh` | the downloader (install as `/usr/local/bin/chronus-fetch-data`) |

```bash
sudo install -m 0755 chronus-gateway /usr/local/bin/
sudo install -m 0755 deploy/fetch-data.sh /usr/local/bin/chronus-fetch-data
sudo install -m 0644 deploy/chronus.sysusers /usr/lib/sysusers.d/chronus.conf && sudo systemd-sysusers
sudo install -d -m 0750 /etc/chronus
sudo install -m 0640 deploy/gateway.production.toml /etc/chronus/gateway.toml   # edit it
chronus-gateway --generate-token | sudo tee /etc/chronus/api_tokens >/dev/null
sudo chmod 0600 /etc/chronus/api_tokens
sudo install -m 0644 deploy/chronus-gateway.service deploy/chronus-fetch-data.* /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl start chronus-fetch-data.service      # first download, so the TLE file exists
sudo systemctl enable --now chronus-fetch-data.timer chronus-gateway
journalctl -u chronus-gateway -f
```

On `systemctl stop` (SIGTERM) the gateway flushes the history file and exits.

### Release binaries

Pushing a `v*` tag runs [`release.yml`](../.github/workflows/release.yml), which publishes:
- archives for Linux x86_64, Windows x86_64 and macOS (Apple silicon), each holding
  `chronus-gateway`, `chronus-replay`, `chronus-hil-sim`, the docs, the example configs and
  `deploy/`, with SHA-256 sums;
- the container image.

## 5. Checklist

- [ ] `chronus-gateway --check-config` reports **Ready** on the production config.
- [ ] The API listens on loopback behind a TLS proxy, or tokens are configured. The gateway
      enforces one or the other.
- [ ] `allowed_origins` lists exactly the dashboards that need the WebSocket.
- [ ] `udp_allowed_sources` / `rf_allowed_sources` name the front-end hosts.
- [ ] TLE (or OEM) and `finals.daily` are refreshed by the timer or the `refresh` feature, and
      `/status` shows their age.
- [ ] Prometheus scrapes `/metrics` with a token, and alerts on `chronus_health_status` and data
      age.
- [ ] The history file lives on persistent storage (`/var/lib/chronus-gateway` or the container
      volume).
