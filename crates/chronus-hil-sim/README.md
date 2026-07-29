# chronus-hil-sim

NeXosim-backed **synthetic** spacecraft telemetry driver for
[ChronusGateway-RS](https://github.com/IsomorphicAlgo/chronus-gateway): a discrete-event
"spacecraft" that emits CCSDS TM packets (`chronus.hil.tm.v1` data field: abstract EPS / thermal /
ADCS scalars) over UDP, for hardware-in-the-loop style testing of the gateway's
Physics–Telemetry Co-Validation checks (CV-3…CV-5).

Because the simulator computes its EPS voltage and panel temperature from the **same**
Sun-illumination function the gateway validates against (SGP4 + conical umbra/penumbra eclipse via
[Ephemerust](https://crates.io/crates/ephemerust)), nominal runs keep `physics_flags` clean —
and the optional **scripted anomaly** flags exactly the frames you ask it to corrupt.

## Usage

```bash
# Gateway first (listens on 127.0.0.1:7301 by default), then:
chronus-hil-sim 127.0.0.1:7301 2000

# Scripted CV-4 fault window: EPS voltage forced out-of-band for frames 40..65
chronus-hil-sim 127.0.0.1:7301 500 --scripted-anomaly eps-voltage \
    --anomaly-after-frame 40 --anomaly-frame-count 25
```

**Synthetic demo traffic only** — no mission-specific data. Uses the public ISS (ZARYA) reference
TLE. Full documentation, HIL profiling recipe, and compliance posture live in the
[repository](https://github.com/IsomorphicAlgo/chronus-gateway) (`docs/HIL.md`, `Methodology.md`).

## License

MIT.
