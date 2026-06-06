# ChronusGateway-RS — User guide

This document is for **operators and integrators**: people who want to run the gateway, feed it
data, and understand what it is checking — without first reading the whole codebase. It starts
here at a **story level**; later sections will follow the same stage-gated plans the project uses
internally so nothing drifts from the source of truth.

---

## Introduction — the story in plain terms

Imagine a satellite **beaming data down** to a ground station. On the wire you do not get a
spreadsheet; you get **short bursts of binary** — think of each burst as **one envelope** with a
printed label and **a letter inside**.

- **The label (the CCSDS part)** answers a few practical questions at a glance: *Is this
  telemetry (not a command)?* *Which logical stream does it belong to (a small numeric “APID”)?*
  *How long is the data field?* Chronus expects each UDP datagram it treats as a frame to look
  like **telemetry** in this sense: a **CCSDS Space Packet–style header** plus a **payload** (the
  data field).

- **The letter inside (the payload)** is whatever the mission put there. On a large commercial
  constellation (people often picture **Starlink** as the archetype), that inner letter is
  usually **dense, proprietary, and split across many packet types** — bus voltages, temperatures,
  software health, attitude, and so on — **not** something this project decodes or claims to mimic.

**What Chronus does today is narrower and deliberate.** It uses **open, international-style
framing (CCSDS TM)** so the pipeline is testable and documented. For **demos and hardware-in-the-
loop style tests**, the repo also defines a **tiny, synthetic “pretend subsystem” letter** called
**`chronus.hil.tm.v1`**: twenty-four bytes with a small magic tag, a version, a frame counter, and
three abstract numbers (bus voltage, panel temperature, body rate). That is **not** a real
Starlink packet; it is a **toy payload** so the gateway can show how **subsystem bytes** can be
checked **against physics computed on the ground** (orbit, Sun proxy, tolerances) without
importing anyone’s secret mission format.

So your mental split still works — **system / spacecraft-ish data** vs **physical reality** — but
mapped to Chronus like this:

| Your intuition | In Chronus |
|----------------|------------|
| **“What the satellite sent in the bits”** | The **UDP payload** after parsing: especially the **CCSDS data field**. If the APID is in the configured **HIL band**, those bytes should match **`chronus.hil.tm.v1`**. Otherwise they can be arbitrary TM payload for parse/distribute (co-validation on those bytes is limited). |
| **“What the ground measured from the radio”** (frequency, power, where the dish pointed) | The design has a place for that (**`RfMetadata`**: carrier Hz, received power, azimuth/elevation). **Wiring real receivers into those fields is a forward step**; the story in the validation engine is already written, but the live path may not populate them from every UDP frame yet. |
| **“Where is the bird, how is it moving, what does geometry say?”** | Comes from **configuration and time**: a **TLE** (two-line element set) plus your station’s location and nominal carrier, so the gateway can **compute** look angles, range-rate, and related checks **without** those numbers being repeated inside every telemetry packet. The **demo simulator** uses a **public ISS reference TLE** only so orbit and Sun geometry are **self-consistent and reproducible** — not because the payload is “ISS flight data.” |

**One sentence takeaway:** Chronus listens for **UDP telemetry frames**, understands **CCSDS TM
wrappers**, optionally decodes a **small synthetic HIL “letter”** for subsystem demos, and can
**compare** what it knows from **orbit + station + (when provided) RF measurements** to what the
frame claims — flagging disagreements in **`physics_flags`** for downstream dashboards (for
example Open MCT).

---

## First run — see it working on your laptop

This section tracks **Milestone 8** in [`docs/BUILD_PLAN.md`](BUILD_PLAN.md): file-backed config
exists, but you can still start with **built-in defaults** (same numbers the project used before
TOML landed).

### What you need first

- **Rust** at the workspace MSRV or newer (see root `Cargo.toml` / `README.md`).
- **Ephemerust** as a **sibling checkout** next to this repo (`../Ephemerust`), because the
  gateway links it by path for propagation. If that folder is missing, `cargo build` fails until
  you clone it — same layout the README illustrates.

### Build

From the workspace root:

```bash
cargo build -p chronus-gateway
```

(Optional) `cargo build -p chronus-hil-sim` if you want the synthetic “satellite” feeder below.

### Start the gateway (defaults)

With **no config file**, the binary loads **`IngestConfig::default()`** and **`StationConfig::default()`**: validated numbers, plus the **inline public ISS (ZARYA) reference TLE** already embedded in the code (not a live network fetch). That gives you a propagator for demos.

```bash
cargo run -p chronus-gateway
```

You should see logs for two sockets:

| Role | Default address | Plain language |
|------|-----------------|----------------|
| **UDP ingest** | `127.0.0.1:7301` | “Throw telemetry datagrams here.” Loopback avoids surprise firewall prompts on a dev machine. |
| **HTTP + WebSocket** | `127.0.0.1:8080` | “Operators and dashboards connect here.” |

**Stop cleanly:** press **Ctrl+C**. The HTTP server shuts down gracefully and the UDP task is
cancelled; the log line `shutdown complete` includes ingest counters.

**Logs:** tracing listens for **`RUST_LOG`** (for example `RUST_LOG=debug`); if unset, the default
filter is **`info`**.

**If propagation does not start:** a **warning** is logged (`no orbital propagator`) and
WebSocket payloads **omit** computed look angles — usually a **bad or missing TLE** after you
change config. The UDP/CCSDS path still runs; only physics-rich fields disappear.

### Optional: TOML instead of defaults

Copy [`gateway.example.toml`](../gateway.example.toml), edit addresses or the TLE block, then either:

```bash
cargo run -p chronus-gateway -- --config path/to/gateway.toml
```

or set **`CHRONUS_GATEWAY_CONFIG=path/to/gateway.toml`** before running. **`--config` / `-c` wins
over the env var** if both are set.

Rules that trip people up the first time (they mirror validation errors):

- If **`[station]`** is present, set **exactly one** of **`tle_inline`** or **`tle_file`** — not both, not neither.
- **`tle_file`** paths are read **relative to the process working directory** unless you give an absolute path.

There is **no generated `--help`** on the binary today; treat **`gateway.example.toml`** plus this
guide as the operator surface for flags.

### Optional: feed synthetic HIL frames (two terminals)

Think of **`chronus-hil-sim`** as a **laboratory satellite** that only speaks the project’s
**`chronus.hil.tm.v1`** letter inside a CCSDS TM envelope. It does **not** model a real commercial
payload; it **does** push frames into the **same UDP ingest** the real gateway uses.

1. **Terminal A:** `cargo run -p chronus-gateway` (listening on `127.0.0.1:7301` by default).
2. **Terminal B:** `cargo run -p chronus-hil-sim` — defaults are **`127.0.0.1:7301`** and **100**
   frames. You can override: `cargo run -p chronus-hil-sim -- 127.0.0.1:7301 24`.

The sim’s CLI uses APID **`0x7B0`**, which sits inside the default **HIL TM v1** band
(**`0x7B0`…`0x7BF`**). That is why the gateway will **decode** the inner twenty-four bytes and run
the **subsystem-style** checks described in the next section.

**Quick health check without a full dashboard:** open **`http://127.0.0.1:8080/health`** in a
browser — expect **HTTP 200**. Metrics live under **`/api/v1/chronus/metrics`** (see
[`docs/HIL.md`](HIL.md)). Open MCT–oriented streaming uses the **WebSocket** at
**`/telemetry/openmct`**.

---

## The alarm field: `physics_flags`

After parsing a frame, the gateway may set **`physics_flags`** — a single **unsigned 8-bit**
integer treated as a **bit mask**. Think of it as a **row of eight sticky notes** on the operator’s
monitor: each bit is either **quiet (0)** or **raised (1)** for that frame. Downstream JSON (Open
MCT and friends) carries this field so alarms can light up without re-deriving physics in the
browser.

**`0` means “no co-validation complaint”** for the checks that ran. It does **not** guarantee the
mission is healthy — it only means **this gateway** did not flag those specific contracts.

### Which lights exist today?

| Bit | Value (hex) | Meaning in plain terms | When it is usually skipped |
|-----|-------------|------------------------|----------------------------|
| **0** | `0x01` | **Doppler** — “The measured carrier frequency I was given does not match what I expect from the current line-of-sight range rate and your nominal carrier, beyond **T-DOPPLER**.” | Skipped if **no measured carrier** is supplied, or it is not finite. **Today’s default HTTP path supplies none**, so bit **0** stays quiet unless you integrate **`RfMetadata`** from your receiver chain. |
| **1** | `0x02` | **Horizon** — “From the TLE and your station, this spacecraft is **below** your configured minimum elevation.” | Needs a working propagator and valid geometry. |
| **2** | `0x04` | **Link budget (free-space)** — “The measured received power I was given disagrees with a **simple free-space** prediction by more than **T-RSSI**.” | Skipped if **no measured Rx power**, or non-finite, or impossible geometry (e.g. zero range). Same story as bit **0**: **needs `RfMetadata` wiring** to light in production. |
| **3** | `0x08` | **Pointing** — “The measured azimuth/elevation I was given point more than **T-POINT** away from where I think the dish should aim.” | Skipped unless **both** azimuth **and** elevation are supplied and finite. Again: **default path = skip**. |
| **4** | `0x10` | **EPS (toy)** — “The **HIL v1** bus voltage does not match the toy Sun-illumination map within **T-EPS**.” | Only when the frame is **HIL v1** (allowed APID + valid decode) and illumination is usable. |
| **5** | `0x20` | **Thermal (toy)** — “The **HIL v1** panel temperature is outside the toy thermal band (**T-THERMAL**).” | Same gate as bit **4**. |
| **6** | `0x40` | **Body rate (toy)** — “The **HIL v1** spin-rate scalar exceeds your configured ceiling (**T-BODYRATE**).” | Skipped if the ceiling is misconfigured or the rate is non-finite. |
| **7** | `0x80` | **Reserved** — do not interpret; should stay **0** until a future charter assigns it. | — |

If you ever need **more than eight** independent alarms, the project charter says to add a **new
field** (for example `physics_flags_v2`) in the JSON contract — **not** to silently repurpose bit
**7**. See **`Methodology.md` D-016** and [`docs/EXTENDED_COVALIDATION_PLAN.md`](EXTENDED_COVALIDATION_PLAN.md).

### Where the numbers come from

The **`T-*`** names (**T-DOPPLER**, **T-RSSI**, **T-POINT**, **T-EPS**, **T-THERMAL**,
**T-BODYRATE**) are the **tolerance register** in [`TEST_PLAN.md`](../TEST_PLAN.md). When you tune
a station or argue about pass/fail in test or ops, that table is the shared vocabulary.

---

## How the rest of this guide will grow (follow the plans)

Deeper chapters will align with the repository’s existing plans so operators and developers read
the same milestones and tolerances the project tests against:

| When you want to… | Start here |
|-------------------|------------|
| Understand **what was built in which order** and what “done” means per stage | [`docs/BUILD_PLAN.md`](BUILD_PLAN.md) |
| See **what is tested**, **default tolerances** (T-DOPPLER, T-RSSI, …), and test commands | [`TEST_PLAN.md`](../TEST_PLAN.md) |
| Read the **extended co-validation** story after M8 (CV milestones, flags, charter) | [`docs/EXTENDED_COVALIDATION_PLAN.md`](EXTENDED_COVALIDATION_PLAN.md) |
| Run the **NeXosim HIL** driver and interpret gateway metrics during a soak | [`docs/HIL.md`](HIL.md) |
| Run a **repeatable demo** (native or Docker) | [`docs/DEMO.md`](DEMO.md) |
| Use the **Vite demo dashboard** (live `physics_flags` + geometry) | [`demo/dashboard/README.md`](../demo/dashboard/README.md), [`docs/DEMO.md`](DEMO.md) Path C |
| Plan **portfolio demos** (Compose, dashboard, replay) and manual acceptance gates | [`docs/SHOWCASE_PLAN.md`](SHOWCASE_PLAN.md), [`docs/Demo_Test.md`](Demo_Test.md) |
| Understand **why** a major choice was made (dependencies, seams, deferrals) | [`Methodology.md`](../Methodology.md) |
| Configure a real run (bind addresses, station, TLE path) | [`gateway.example.toml`](../gateway.example.toml), `--config` / `-c`, and `CHRONUS_GATEWAY_CONFIG` (see **First run** above) |

**Status:** **Introduction**, **First run**, and **`physics_flags`** are drafted. Still to add:
hex-level packet walkthrough, deeper Open MCT operator notes, and a troubleshooting chapter — each
tied to the same plan files.

---

*Teaching tone: envelope vs letter, Starlink as analogy only, synthetic HIL demarcated from real*
*constellations; operator defaults and alarm bits aligned with `BUILD_PLAN` M8, `TEST_PLAN`, and*
*`validate` docs.*
