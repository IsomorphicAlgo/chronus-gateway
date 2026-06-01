# ChronusGateway-RS Runbook

Source-backed operating notes for the implemented gateway path. This page covers the code that
exists today: UDP ingestion, CCSDS telemetry parsing, station configuration, and throttled
Ephemerust tracking (Milestones 1-3).

---

## Runtime path

```
UDP datagram
  -> ingest::run / RawFrame
  -> ccsds::parse_telemetry / TelemetryFrame
  -> TrackingProvider::tracking_state / TrackingState
  -> structured logs
```

Planned but not yet implemented: physics co-validation flags (Milestone 4), Axum/WebSocket
distribution, Open MCT payloads, and health/history HTTP endpoints (Milestone 5).

---

## Public interfaces and contracts

### UDP ingestion (`crates/gateway/src/ingest.rs`)

- `IngestConfig::default()` binds `127.0.0.1:7301`, uses broadcast capacity `1024`, and caps each
  received datagram at `65_542` bytes.
- `ingest::bind(&config)` returns a Tokio `UdpSocket`. Binding to port `0` is supported for tests;
  inspect `socket.local_addr()` to learn the OS-assigned port.
- `ingest::run(socket, tx, config, stats, shutdown)` runs until the supplied shutdown future
  resolves. The binary uses Ctrl-C; tests use a oneshot channel.
- `RawFrame` preserves the datagram bytes (`Arc<[u8]>`), UTC receive timestamp, and source address.
- Backpressure is intentionally lossy: the `tokio::sync::broadcast` channel drops old frames for
  lagging subscribers instead of blocking the socket loop.

Operational constraint: keep `max_datagram_size` bounded. The receive loop allocates one fixed
buffer of that size and never allocates based on attacker-controlled packet length.

### CCSDS parser (`crates/gateway/src/ccsds.rs`)

- `parse_telemetry(&RawFrame)` accepts CCSDS Space Packet telemetry (TM) packets only.
- Validation order is fixed: primary-header length, `spacepackets` header decode,
  declared-vs-available packet length, then packet type.
- The parser returns structured `CcsdsError` values for short, malformed, truncated, or
  telecommand (TC) packets. These are recoverable; callers should drop the bad datagram and keep
  running.
- `TelemetryFrame::payload()` borrows the packet data field from the original `Arc<[u8]>`; no
  payload copy is made.
- Bytes beyond the declared CCSDS packet length are ignored by the current parser.
- `physics_flags` is reserved for the Milestone 4 co-validation engine and is currently initialized
  to `0`.

### Station and tracking (`crates/gateway/src/config.rs`, `crates/gateway/src/propagator.rs`)

- `StationConfig::default()` uses a synthetic/public-development profile: latitude `35.0`,
  longitude `-116.0`, altitude `1000.0` m, nominal carrier `437_500_000.0` Hz, public ISS TLE, and
  a `10` ms recompute throttle.
- `StationConfig::validate()` range-checks latitude, longitude, altitude finiteness, positive
  carrier frequency, and non-empty inline TLEs.
- `TleSource::Inline` and `TleSource::File` are supported. Live CelesTrak or Space-Track fetching
  is not implemented.
- `EphemerustPropagator::from_station(&config)` validates the station, resolves TLE text, and
  builds the default SGP4-backed propagator.
- `TrackingProvider` wraps `Arc<dyn OrbitalPropagator>` and caches the last state for requests
  within `min_recompute_interval_ms`. Propagation work runs outside the cache mutex.

Compliance constraint: use only public, synthetic, or otherwise non-controlled telemetry examples
and public reference TLEs in this repository.

---

## Developer workflow

1. Install Rust 1.88 or newer.
2. Check out Ephemerust as a sibling path dependency:

   ```
   parent/
   ├── chronus-gateway/
   └── Ephemerust/
   ```

3. Build and test:

   ```bash
   cargo build
   cargo test
   cargo clippy --all-targets
   ```

4. Run the gateway:

   ```bash
   RUST_LOG=info cargo run -p chronus-gateway
   ```

5. Send a synthetic CCSDS TM packet:

   ```bash
   python3 - <<'PY'
   import socket

   packet = bytes([0x00, 0x2A, 0xC0, 0x07, 0x00, 0x04]) + b"hello"
   sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
   sock.sendto(packet, ("127.0.0.1", 7301))
   PY
   ```

Expected result: a `telemetry frame parsed` log. If the tracking provider was built successfully,
the log includes azimuth, elevation, range, and range-rate.

---

## Troubleshooting

| Symptom | Likely cause | What to check |
|---------|--------------|---------------|
| Cargo cannot find `../Ephemerust` | Sibling dependency missing or wrong checkout layout | Place Ephemerust next to this repository, matching `Cargo.toml`. |
| Rust version error | Toolchain older than workspace MSRV | Install/use Rust 1.88+ (`rustup toolchain install stable`). |
| `address already in use` on startup | Another process has `127.0.0.1:7301` | Stop the process or change `IngestConfig::bind_addr` in code; no CLI/env config exists yet. |
| Gateway runs but no telemetry logs | No valid UDP packet reached the socket | Send to loopback port `7301`; use a CCSDS TM packet with a consistent data-length field. |
| `dropping invalid/non-telemetry datagram` | Parser rejected bytes | Check for packets shorter than 6 bytes, truncated data fields, malformed headers, or TC packets. |
| Slow consumer misses frames | Expected lossy broadcast behavior | Subscribers should handle `RecvError::Lagged`; the receive loop prioritizes fresh telemetry. |
| Oversized packet behavior differs by OS | Kernel behavior differs | Windows can return `WSAEMSGSIZE` and increment `oversized_dropped`; Unix may truncate to the fixed buffer and let parser validation reject it. |
| No physics fields in logs | Tracking provider failed to initialize | Validate station fields and TLE text/file path. The binary keeps ingesting without physics state. |
| `cargo fmt --check` reports unrelated Rust diffs | Existing formatting noise | Avoid mixing formatting-only churn into documentation changes unless the task calls for it. |

---

## References

- `README.md` - project overview, setup quickstart, architecture, acknowledgements.
- `BUILD_PLAN.md` - milestone scope and stage gates.
- `TEST_PLAN.md` - deterministic/offline test strategy, status counts, and tolerance register.
- `Methodology.md` - decision log and attribution table.
- `crates/gateway/src/ingest.rs` - UDP ingestion, lossy broadcast, statistics, shutdown.
- `crates/gateway/src/ccsds.rs` - CCSDS parser validation order and `TelemetryFrame`.
- `crates/gateway/src/config.rs` - ingestion/station configuration and TLE sources.
- `crates/gateway/src/propagator.rs` - `OrbitalPropagator`, Ephemerust backend, and
  `TrackingProvider`.
- CCSDS public standards - open packet framing context.
- `spacepackets` crate - CCSDS/ECSS parser used behind the local module boundary.
- Ephemerust - sibling astrodynamics crate providing SGP4 look-angle and range-rate calculations.

---

## Acknowledgements

Thanks to Ephemerust and its SGP4 dependency for the orbital mechanics foundation; Rusty_Server for
the maintainer's earlier async service patterns; Tokio for the runtime model; the `spacepackets`
project and CCSDS community for open packet standards support; and NASA Open MCT for the target
mission-control integration shape.
