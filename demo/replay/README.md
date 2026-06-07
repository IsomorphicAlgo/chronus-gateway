# Synthetic TM replay (Showcase S3)

Deterministic **UDP replay** of lab-generated CCSDS TM datagrams — same bytes every time for narrative demos and screenshots.

## Fixture format

- **Hex lines:** one Space Packet per line (whitespace-separated or contiguous hex).
- **JSONL:** one JSON object per line with a **`udp_hex`** string (contiguous hex).

Blank lines and lines starting with `#` are skipped.

## Run

From repo root (with gateway UDP ingest on `127.0.0.1:7301`):

```bash
cargo run -p chronus-replay -- --file demo/replay/fixtures/golden_tm.hex --delay-ms 100 --repeat 2
```

Options:

| Flag | Meaning |
|------|---------|
| `-f` / `--file` | Fixture path (required) |
| positional `DEST` | `HOST:PORT` (default `127.0.0.1:7301`) |
| `--delay-ms` | Pause between datagrams |
| `--repeat` | Send the whole sequence N times |

## Compliance

Use **only synthetic / lab-generated** payloads (`AGENTS.md`). Do not replay operational or export-controlled data.
