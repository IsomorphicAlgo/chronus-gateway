# Showcase S4 — curated public CCSDS fixtures

**Tier-2 external fixtures** for portfolio demos and parser robustness checks. Default demos remain
**synthetic-first** (`demo/replay/fixtures/`, `chronus-hil-sim`) per [`AGENTS.md`](../../AGENTS.md).

## Important: what these fixtures are (and are not)

ChronusGateway ingests **CCSDS Space Packets** (133.0-B Space Packet Protocol) as **UDP datagrams**.
Common amateur downlinks are **not** that format on the wire:

| Public context | On-air protocol | In this repo |
|--------------|-----------------|--------------|
| **ISS** amateur digipeater (ARISS / RS0ISS, 145.825 MHz) | AX.25 / APRS | **Not** ingested; see [APRS ISS FAQ](https://www.aprs.org/iss-faq.html) |
| **AMSAT** FUNcube / AO-73 telemetry | Custom BPSK + AO-40-style FEC | **Not** raw UDP CCSDS; see [FUNcube working documents](http://funcube.org.uk/working-documents/) |

**S4 fixtures** are **small, documented CCSDS TM Space Packets** that support **education and
robustness demos** in the ISS / AMSAT narrative. They are **not** operational mission dumps, RF
captures, or export-controlled parameters.

---

## Fixture inventory

### ISS track (`iss/`)

| File | Role | SHA-256 (file) |
|------|------|----------------|
| [`iss/clean.hex`](iss/clean.hex) | One valid TM datagram (APID **0x155**, payload `ISS-EDU`) | `6a2dc834bd640e397eb2d3e5c7dda3fcdb3ec416cc05a7b6e3c2d2f5bb4d6230` |
| [`iss/robustness.hex`](iss/robustness.hex) | Truncated, TC-on-TM-path, short, and garbage lines | `715b3122939ea2265df409ba0af37bc19e76c352197e8256f4743626e6250d0d` |

**Provenance (clean)**

| Field | Value |
|-------|--------|
| **Source** | CCSDS Space Packet Protocol structure per [CCSDS 133.0-B-2](https://public.ccsds.org/Pubs/133x0b2e2.pdf) (public standard); byte layout cross-checked with [`spacepackets`](https://spacepackets.readthedocs.io/en/latest/examples.html) educational examples (Apache-2.0/MIT, us-irs). |
| **Mission context** | Public **ISS** (ZARYA) TLE from [CelesTrak](https://celestrak.org/NORAD/elements/stations.txt) is used in gateway defaults for **propagator** demos — not embedded in these UDP bytes. |
| **License** | Fixture **bytes**: project MIT + CCSDS public standard (no proprietary capture). CelesTrak TLE: [CelesTrak terms](https://celestrak.org/NORAD/documentation/gp-data-formats.php) for TLE use in demos only. |
| **Retrieved** | 2026-06-19 (authored in-repo; no network fetch during tests). |
| **Transformation** | None — hex line is the full UDP payload (one Space Packet per line). |
| **AGENTS.md** | Generic ASCII payload; no operational ISS TM dump; no controlled RF parameters. |

**Robustness (`iss/robustness.hex`)** — derived **locally** from `iss/clean.hex` (truncate, set TM→TC type bit, shorten, all-`0xFF` header). No external source.

---

### AMSAT track (`amsat/`)

| File | Role | SHA-256 (file) |
|------|------|----------------|
| [`amsat/clean.hex`](amsat/clean.hex) | One valid TM datagram (APID **0x073**, payload `AO-73EDU`) | `f84b025526cfe2af01a55627fa0ccd1fd7ba292aedbcd4c765532fb2738ef88a` |
| [`amsat/robustness.hex`](amsat/robustness.hex) | Truncated, TC-on-TM-path, short, and garbage lines | `d0a64c0963009bc038dc538519885c87539ae555b62c40d423d4ad731b622cab` |

**Provenance (clean)**

| Field | Value |
|-------|--------|
| **Source** | CCSDS 133.0-B Space Packet TM layout (same as ISS row); APID **0x073** echoes the educational **SCID 0x73** worked example in [gr-satellites `CCSDS_README.md`](https://github.com/daniestevez/gr-satellites/blob/main/CCSDS_README.md) (GPL-3.0). AMSAT outreach context: [AMSAT-UK CCSDS Outreach Initiative](https://amsat-uk.org/2025/12/08/ccsds-outreach-initiative-and-competition/). |
| **License** | Fixture bytes: MIT + public CCSDS standard; gr-satellites cited for **pedagogical** APID choice only (no GPL code copied). |
| **Retrieved** | 2026-06-19 (authored in-repo). |
| **Transformation** | None — full UDP payload per line. |
| **AGENTS.md** | Educational label payload only; not a FUNcube `.funcubebin` or warehouse archive extract. |

**Robustness (`amsat/robustness.hex`)** — locally derived from `amsat/clean.hex`.

---

## Replay (operator)

Gateway UDP ingest must be running (`docs/DEMO.md` Path A or B). From repo root:

```bash
cargo run -p chronus-replay -- --file demo/fixtures/iss/clean.hex --delay-ms 100
cargo run -p chronus-replay -- --file demo/fixtures/amsat/clean.hex --delay-ms 100
```

Robustness files: replay will **send** all lines; the gateway **drops** invalid datagrams without
panicking — use the dashboard or metrics endpoint to confirm only clean lines produce frames.
See **`docs/DEMO.md` → Path E** and **`docs/Demo_Test.md` §S4**.

## Automated test

`crates/gateway/tests/s4_fixtures.rs` loads `clean` fixtures (expect parse OK) and `robustness`
fixtures (expect structured errors, no panic). **Offline** — no network fetch.

## Owner compliance sign-off

**Gate S-4** approved **2026-06-19** (owner: Michael Hansen). Fixture rows satisfy [`AGENTS.md`](../../AGENTS.md)
/ ITAR-EAR posture (CCSDS educational bytes; no operational RF dumps).
