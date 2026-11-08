# Real-RF Doppler validation

Everything else in the test suite checks the Doppler alarm (**T-DOPPLER**, `physics_flags` bit 0)
against numbers the project generated itself. This page is the procedure for checking it against
a real satellite signal received with your own antenna, and the record of the results
(Tranche 5.3, `Methodology.md` D-056). The tools are `tools/rf/iq_carrier.py` (carrier track from
an SDR recording) and `chronus-doppler` (residuals against the gateway's own prediction), both in
this repository.

## What counts as a pass

On one pass, with samples at or above 5° elevation:

1. **≥ 95 %** of residuals inside the band the gateway applies (±150 Hz by default), after
   removing a constant receiver/transmitter offset and, optionally, a linear drift;
2. **shape gain within 1 ± 0.05**: the measured Doppler S-curve has the predicted amplitude;
3. at least **30** samples.

Why the fit: a consumer SDR is off by a few ppm (1 ppm = 437 Hz at 437 MHz) and a satellite's
transmitter is rarely exactly on its published frequency. Those are hardware constants, not
physics, and `chronus-doppler` reports them (the fitted offset in Hz and ppm is what you would put
into `nominal_carrier_hz` to run the live alarm on that receiver). The shape gain cannot be fitted
away: a wrong TLE, a wrong station position or a clock error changes the curve, not its offset.

## Error budget (UHF, high LEO pass)

| Source | Size | Note |
|---|---|---|
| TLE age, 1 day | ≈ 25–75 Hz near closest approach | use a TLE from the day of the pass |
| Recording clock, 0.1 s | ≤ 18 Hz | the Doppler changes up to ~180 Hz/s on an overhead pass; keep the PC on NTP |
| Station position, 10 m | < 1 Hz | a phone GPS fix is plenty |
| Ionosphere at UHF | < 1 Hz | |
| Carrier estimate (0.5 s FFT, interpolated) | ≈ 1–3 Hz on a clean CW carrier | worse on FM / bursty downlinks |
| Propagator range rate (T-RANGERATE) | < 1.5 Hz | |
| Oscillator offset and drift | kHz, Hz/s | removed by the fit, reported |

The ±150 Hz band has room for all of it once the clock and TLE are right.

## Rehearsal on synthetic IQ (no antenna)

Run this first; it checks the whole chain on your machine, and CI runs the same thing on every
change (`.github/workflows/ci.yml`, "RF validation rehearsal").

```bash
cargo build --release -p chronus-evidence
D=target/release/chronus-doppler

# 1. The predicted carrier of a reference pass (public ISS elements, the default lab station).
$D -c tools/rf/rehearsal.toml --predict-start 2020-07-13T05:04:00Z --predict-seconds 660 > pred.jsonl

# 2. The IQ a receiver 4.2 ppm fast, drifting 0.05 Hz/s, would have recorded (SNR 0 dB in band).
python3 tools/rf/synth_iq.py pred.jsonl pass.cs8 --rate 32000 --ppm 4.2 --drift-hz-s 0.05 --snr-db 0 --min-el 5
#    -> prints the iq_carrier.py command with the right rate, centre and start time

# 3. Carrier track, then the verdict.
python3 tools/rf/iq_carrier.py pass.cs8 --format cs8 --rate 32000 --center 437500000 \
    --start 2020-07-13T05:05:45Z > pass.rf.jsonl
$D -c tools/rf/rehearsal.toml --rf pass.rf.jsonl --fit offset-drift
```

Recorded result (2026-11-07, this commit):

| | |
|---|---|
| samples ≥ 5° | 1014 (max elevation 89.6°), predicted Doppler swing 20 082 Hz |
| fit (offset + drift, 1010 of 1014 samples) | −1837.5 Hz (−4.20 ppm), +0.050 Hz/s — exactly the injected receiver |
| residuals after fit: mean / RMS / p95 / max | +11.4 / 230.7 / 2.9 / 5169.5 Hz |
| inside ±150 Hz | 99.6 % |
| shape gain | 1.0000 (r² 1.00000) |
| verdict | PASS |

The four outliers are blocks where the carrier crossed the receiver's DC notch at closest
approach and the peak finder briefly locked onto noise; they are still counted against the 95 %
rule. The fit ignores them, so they cannot drag the offset.

## A real pass

**Satellite.** Pick one with a steady carrier: a CW beacon or a continuous BPSK/FSK telemetry
downlink in the 435–438 MHz amateur-satellite band, listed as active in the
[SatNOGS DB](https://db.satnogs.org/). FM voice and APRS downlinks are bursty and give a
noisier track. Note its published downlink frequency.

**Equipment.** Any SDR that records raw IQ (an RTL-SDR is enough) and a UHF antenna; a simple
handheld Yagi or a turnstile works for a beacon on a pass above ~30°.

1. **Station file.** Copy `tools/rf/rehearsal.toml` to a file outside the repository (it will hold
   your position). Set `latitude_deg`, `longitude_deg`, `altitude_m`, `nominal_carrier_hz` (the
   published downlink) and a TLE from the day of the pass (CelesTrak, by NORAD catalogue number).
2. **Plan.** `chronus-doppler -c mypass.toml --predict-start <UTC> --predict-seconds 900` prints
   elevation and predicted carrier each second. Choose a pass above 30°.
3. **Clock.** Make sure the recording computer is NTP-synchronised (`timedatectl` /
   `w32tm /query /status`): 0.1 s matters near closest approach.
4. **Record.** Tune 25–50 kHz away from the carrier so it never crosses the DC spike, at a rate
   that covers ±15 kHz of Doppler on top of that (an RTL-SDR's 250 kS/s is fine), and write down the
   UTC start time to the tenth of a second:

   ```bash
   date -u +%FT%T.%1NZ > pass.start; rtl_sdr -f 437525000 -s 250000 -g 40 pass.cu8
   ```

   (SDR++, gqrx and other recorders that write SigMF or gqrx-named files carry the start time
   themselves; `iq_carrier.py` reads it.)
5. **Track the carrier.**

   ```bash
   python3 tools/rf/iq_carrier.py pass.cu8 --format cu8 --rate 250000 --center 437525000 \
       --start "$(cat pass.start)" --expect 437500000 > pass.rf.jsonl
   ```

6. **Judge.**

   ```bash
   chronus-doppler -c mypass.toml --rf pass.rf.jsonl --fit offset-drift \
       --csv pass.csv --markdown pass.md --report pass.json
   ```

   A failed shape gain or residuals that swing one way before closest approach and the other way
   after point at the clock (shift `--start`) or the TLE; a constant residual is the offset the fit
   removes.
7. **Live, optionally.** To see the alarm in Open MCT, run the gateway with
   `nominal_carrier_hz` corrected by the fitted offset and send the track to its RF side channel
   (`docs/RF_SIDE_CHANNEL.md`); bit 0 should stay clear for the whole pass.

**What to publish.** The `pass.md` summary and, if you like, the CSV. Keep the IQ recording and
your station file private: the summary does not contain your position, and nothing else is needed
to review the result. Only public amateur-satellite beacons belong here (owner's AGENTS.md:
synthetic or public signals only).

## Results

| Date | Satellite / downlink | Max el. | Samples | Fit | Inside band | Shape gain | Verdict |
|---|---|---|---|---|---|---|---|
| 2026-11-07 | rehearsal (synthetic IQ, public ISS elements) | 89.6° | 1014 | −4.20 ppm, +0.05 Hz/s | 99.6 % | 1.0000 | PASS |
| — | owner's own capture | | | | | | pending |
