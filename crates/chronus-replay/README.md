# chronus-replay

Deterministic **UDP replay** of synthetic CCSDS TM datagrams from text fixtures, for
[ChronusGateway-RS](https://github.com/IsomorphicAlgo/chronus-gateway) demos and acceptance runs
(Showcase S3): the same bytes, in the same order, every time.

Fixture format — one datagram per non-empty, non-`#` line, either:

- **Hex:** whitespace-separated or contiguous hex of a full Space Packet, or
- **JSONL:** a JSON object with a `udp_hex` key.

## Usage

```bash
# Gateway first (listens on 127.0.0.1:7301 by default), then:
chronus-replay --file golden_tm.hex
chronus-replay --file anomalies.jsonl 127.0.0.1:7301 --delay-ms 50 --repeat 3
```

Fixtures must be **lab-generated synthetic bytes only** (no operational RF captures). Curated
examples and provenance rules live in the
[repository](https://github.com/IsomorphicAlgo/chronus-gateway) under `demo/replay/` and
`demo/fixtures/`.

## License

MIT.
