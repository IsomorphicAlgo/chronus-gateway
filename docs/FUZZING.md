# Fuzzing

Every parser that reads bytes from outside the gateway has a coverage-guided
[`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) target (libFuzzer with AddressSanitizer)
in [`fuzz/`](../fuzz/). Each target asserts properties of what it accepts, not only that nothing
panics (`Methodology.md` D-054). The 1.0 exit criterion is **24 hours clean per target**,
accumulated across runs and recorded in the ledger below.

## Targets

| Target | What it feeds | Properties asserted |
|---|---|---|
| `ccsds_packet` | one UDP datagram → `ccsds::parse_telemetry`, then CDS and CUC time decoding | payload inside the datagram; APID ≤ 0x7FF; user data inside the payload |
| `tm_deframer` | framing options + up to 64 datagrams (structured with `arbitrary`) → `frames::TmDeframer` | every packet out has VCID < 8 and exactly its declared length |
| `packet_pipeline` | one datagram → `pipeline::handle_datagram` (parse, co-validate, JSON, history) | history never exceeds its capacity |
| `hil_tm_v1` | packet data field → `hil_tm::decode_hil_tm_v1` | anything accepted re-encodes to the same 24 bytes |
| `rf_datagram` | `chronus.rf.v1` → `RfSampleV1::parse`, and the bytes as a stream into `RfStore` | parse → encode → parse is stable; the store stays within capacity |
| `oem_text` | text → `OemEphemeris::parse`, then interpolation inside the coverage | coverage start ≤ stop; no panic interpolating |
| `eop_finals` | text → `freshness::parse_finals` (fixed width or CSV), then interpolation | accepted files are non-empty; interpolated UT1−UTC, x, y within ±1 (s, ″) |
| `config_toml` | text → `config::load_gateway_from_str` → startup checks | every rejection has a `CG-` explanation; accepted configs run the startup checks |

## Run it

cargo-fuzz needs a nightly toolchain (`rustup toolchain install nightly`):

```bash
cargo install cargo-fuzz --version 0.13.2 --locked
python3 fuzz/make_seeds.py                     # seeds from the repository's fixtures
cd fuzz
cargo +nightly fuzz run -O ccsds_packet corpus/ccsds_packet seeds/ccsds_packet -- -max_total_time=600
cargo +nightly fuzz list                       # all targets
```

A crash leaves its input in `fuzz/artifacts/<target>/`. Reproduce it with
`cargo +nightly fuzz run <target> <file>`, fix the bug, and add a unit test with that input.

Without nightly, a stable toolchain can build the targets with `RUSTC_BOOTSTRAP=1` (how the local
campaign below was run); CI uses nightly.

In CI ([`.github/workflows/fuzz.yml`](../.github/workflows/fuzz.yml)):

- **Pull requests** that touch `crates/gateway/src` or `fuzz/`: 30 s per target.
- **Nightly**: 30 min per target, each in its own job, resuming from the corpus the previous run
  cached; the job summary records executions, peak memory and corpus growth.
- **Manual** (`workflow_dispatch`): choose the targets and up to 21 000 s each. Five dispatches at
  18 000 s give a target 25 h.

## Ledger (toward 24 h per target)

Each row is one run; *total* is the target's running sum. Runs on stable Rust used
`RUSTC_BOOTSTRAP=1`, an x86_64 Linux VM, one job, AddressSanitizer.

| Date | Where | Target | Seconds | Executions | Result | Total |
|---|---|---|---:|---:|---|---:|
| 2026-11-07 | local VM | `ccsds_packet` | 600 | 263 358 954 | clean | 600 s |
| 2026-11-07 | local VM | `hil_tm_v1` | 600 | 494 665 994 | clean | 600 s |
| 2026-11-07 | local VM | `oem_text` | 600 | 11 755 350 | clean | 600 s |
| 2026-11-07 | local VM | `config_toml` | 600 | 5 466 787 | clean | 600 s |
| 2026-11-07 | local VM | `rf_datagram` | ~64 | 2 914 704 | **crash**: float round trip (below), fixed | 0 s |
| 2026-11-07 | local VM | `rf_datagram` | 600 | 17 424 733 | clean (after the fix) | 600 s |
| 2026-11-07 | local VM | `tm_deframer` | 600 | 15 954 788 | clean | 600 s |
| 2026-11-07 | local VM | `eop_finals` | 600 | 23 185 503 | clean | 600 s |
| 2026-11-07 | local VM | `packet_pipeline` | 600 | 15 065 672 | clean | 600 s |

Add a row per nightly or manual run from the job summary of `fuzz.yml`. The 1.0 criterion is met
when every target's total reaches 86 400 s with no unfixed crash.

## Findings

| Found | Target | Bug | Fix |
|---|---|---|---|
| 2026-11-07 | `rf_datagram` | A `chronus.rf.v1` number with a long decimal mantissa (here a 49-digit `rx_power_dbm`) parsed one ULP away from the nearest `f64`, so parse → encode → parse changed the value. `serde_json` parses exactly only with its `float_roundtrip` feature. The test suite never saw it because the `jsonschema` dev-dependency turned that feature on for test builds only. | `float_roundtrip` on the workspace `serde_json`; regression test `rf::tests::long_decimals_parse_to_the_nearest_double_and_round_trip`; the input is kept in `fuzz/regressions/rf_datagram/` and seeded into every run |
