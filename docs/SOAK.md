# Soak and load

`chronus-soak` (crate `chronus-evidence`, `Methodology.md` D-055) drives a running gateway:

- **Sender.** CCSDS probe packets at a fixed rate over UDP, from a dedicated thread. Each packet
  carries a sequence number and its send time.
- **Clients.** N WebSocket clients on `/telemetry/openmct` decode the probe from every message. The
  **end-to-end latency** they measure covers the whole path: UDP in, parse, physics
  co-validation, JSON, history, WebSocket out.
- **Sampler.** It reads the gateway's `/metrics` every few seconds: `process_resident_memory_bytes`
  and the drop counters.

The report gives:
- latency p50 / p90 / p99 / p99.9 / max;
- loss, meaning messages each client should have received after warm-up but did not;
- memory after warm-up, peak and end, and the **least-squares memory slope over the second half
  of the run**: a process with bounded memory sits at about 0 MiB/h once its buffers are full.

The 1.0 exit criterion (plan 5.2) is 24 h at the design load (10 000 packets/s, 50 clients) with
bounded memory and the p99 stated here.

## Run it

```bash
cargo build --release -p chronus-gateway -p chronus-evidence
target/release/chronus-gateway &                          # or any config; tokens: --token / CHRONUS_TOKEN
target/release/chronus-soak --rate 10000 --clients 50 --duration 24h \
    --report soak.json --markdown soak.md \
    --max-p99-ms 50 --max-rss-slope-mib-per-h 1 --max-loss 0.001   # optional gates (exit 1)
```

Memory comes from the gateway's `process_resident_memory_bytes` (Linux, Windows and macOS).
On Linux, `--pid <gateway pid>` reads `/proc` instead.

**Build both in release mode** (`--release`): a debug build is many times slower and will lag at
the design load. Sender, clients and gateway on one machine share its CPUs, so the result
describes that machine. The 10 000 pkt/s × 50 clients load is 500 000 WebSocket messages a
second, and the soak's own clients decode every one.

## Results

### 2026-11-07 — design load, 10 minutes (1.0.0-rc.1, cloud VM)

Default configuration (history in memory, 10 000 entries; no physics tracking beyond the default
station; no token). Gateway, sender and 50 clients on one 2-vCPU Linux VM, so the clients' own
JSON decoding competes with the gateway for CPU.

| | |
|---|---|
| host | linux x86_64 (2 CPUs) |
| packets sent | 5999999 (10000 pkt/s achieved) |
| clients connected | 50 / 50 |
| messages received | 285751000 of 285751000 expected after warm-up (loss 0.0000 %) |
| latency p50 / p90 / p99 / p99.9 / max (ms) | 1.92 / 8.45 / 13.05 / 15.49 / 46.38 |
| RSS after warm-up / peak / end (MiB) | 23.6 / 24.3 / 24.1 |
| RSS slope, second half (MiB/h) | +1.83 |
| `chronus_gateway_telemetry_frames_emitted_total` | 5999999.0 |
| `chronus_gateway_telemetry_parse_errors_total` | 0.0 |
| `chronus_gateway_ws_messages_sent_total` | 299999950.0 |
| `chronus_history_len` | 10000.0 |
| `chronus_ingest_frames_received_total` | 5999999.0 |
| `chronus_ingest_oversized_dropped_total` | 0.0 |
| `chronus_ingest_recv_errors_total` | 0.0 |
| `chronus_pipeline_datagrams_lagged_total` | 0.0 |
| `chronus_pipeline_ws_messages_dropped_total` | 0.0 |
| verdict | PASS |

**Reading it.**
- **No loss.** 300 million WebSocket messages went out and none was lost: no lag on the
  pipeline or on any client, and every probe arrived in order.
- **Latency.** p99 is 13 ms end to end on a shared 2-vCPU machine at 500 000 messages/s.
- **Memory.** It rose to 24 MiB while the history ring filled (the first 30 s, excluded as
  warm-up) and then held between 23.9 and 24.3 MiB for ten minutes.
- **Slope.** The +1.8 MiB/h is a 0.15 MiB wobble over five minutes, below what a run this short
  can resolve. A leak of even one byte per message would have added 300 MB here; the 24 h run
  settles the slope.

### 2026-11-07 — light load, 3 minutes (same build and VM)

| | |
|---|---|
| load | 1 000 pkt/s, 10 WebSocket clients |
| messages received | 1 507 010 of 1 507 010 expected after warm-up (loss 0 %) |
| latency p50 / p90 / p99 / p99.9 / max (ms) | 0.18 / 0.25 / 0.34 / 1.07 / 9.97 |
| RSS after warm-up / peak / end (MiB) | 16.9 / 19.4 / 19.4 |

Uncontended, the full path takes about 0.2 ms. The latency at the design load is queueing for the
two CPUs. Memory still grows at 3 minutes (allocator warm-up, 2.5 MiB over 180 000 frames). That
growth is not per message: the design run carried 33 times as many frames and stayed flat.

### Pending: 24 h at the design load on the release host

```bash
target/release/chronus-soak --rate 10000 --clients 50 --duration 24h --warmup 10m \
    --max-loss 0.001 --max-rss-slope-mib-per-h 0.5 --report soak-24h.json --markdown soak-24h.md
```

Paste `soak-24h.md` here.
