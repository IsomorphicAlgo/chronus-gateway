#!/usr/bin/env python3
"""Synthetic IQ for rehearsing the real-RF Doppler check (no antenna needed).

Reads the predicted carrier track `chronus-doppler --predict-start ...` prints and writes the IQ an
SDR would have recorded: a carrier following that track plus the errors real hardware adds — a
receiver oscillator offset (ppm), a slow drift, and white noise at a chosen SNR.

    chronus-doppler -c tools/rf/rehearsal.toml --predict-start 2020-07-13T05:04:00Z \\
        --predict-seconds 600 > predicted.jsonl
    python3 tools/rf/synth_iq.py predicted.jsonl rehearsal.cs8 --rate 48000 --ppm 4.2

Prints the matching `iq_carrier.py` command. Requires numpy.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import sys

import numpy as np


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("predicted", type=pathlib.Path, help="chronus.rf.v1 lines (carrier_hz, t)")
    p.add_argument("out", type=pathlib.Path, help="output IQ file (cs8 interleaved)")
    p.add_argument("--rate", type=float, default=48_000.0, help="sample rate (48000)")
    p.add_argument("--center", type=float, help="tuned centre (default: middle of the track, to the kHz)")
    p.add_argument("--ppm", type=float, default=4.2, help="receiver oscillator error, ppm (4.2)")
    p.add_argument("--drift-hz-s", type=float, default=0.0, help="oscillator drift, Hz/s (0)")
    p.add_argument("--snr-db", type=float, default=15.0,
                   help="carrier-to-noise in the full band, dB (15)")
    p.add_argument("--min-el", type=float, default=0.0, help="only write above this elevation")
    p.add_argument("--seed", type=int, default=1)
    a = p.parse_args()

    rows = [json.loads(line) for line in a.predicted.read_text().splitlines() if line.strip()]
    rows = [r for r in rows if r.get("el_deg", 90.0) >= a.min_el]
    if len(rows) < 2:
        raise SystemExit("fewer than two predicted points above the elevation limit")
    t = np.array([dt.datetime.fromisoformat(r["t"].replace("Z", "+00:00")).timestamp() for r in rows])
    f = np.array([r["carrier_hz"] for r in rows])
    center = a.center or round(float(f.min() + f.max()) / 2e3) * 1e3
    if (f.max() - f.min()) / 2 + abs(center * a.ppm * 1e-6) > 0.45 * a.rate:
        raise SystemExit("--rate is too low for this Doppler swing plus offset (aliasing)")
    lo_offset = center * a.ppm * 1e-6  # a fast oscillator reads every carrier this much low
    n = int((t[-1] - t[0]) * a.rate)
    rng = np.random.default_rng(a.seed)
    noise_sigma = 10 ** (-a.snr_db / 20) / np.sqrt(2)
    phase = 0.0
    chunk = int(a.rate)
    with a.out.open("wb") as out:
        for s0 in range(0, n, chunk):
            k = np.arange(s0, min(n, s0 + chunk))
            ts = t[0] + k / a.rate
            off = np.interp(ts, t, f) - center - lo_offset + a.drift_hz_s * (ts - t[0])
            ph = phase + 2 * np.pi * np.cumsum(off) / a.rate
            phase = float(ph[-1])
            x = np.exp(1j * ph) * 0.5
            x += noise_sigma * (rng.standard_normal(k.size) + 1j * rng.standard_normal(k.size))
            iq = np.empty(2 * k.size, dtype=np.int8)
            iq[0::2] = np.clip(np.round(x.real * 127), -128, 127)
            iq[1::2] = np.clip(np.round(x.imag * 127), -128, 127)
            out.write(iq.tobytes())
    start = dt.datetime.fromtimestamp(t[0], dt.timezone.utc).isoformat().replace("+00:00", "Z")
    print(f"wrote {n / a.rate:.0f} s; receiver offset {-lo_offset:+.1f} Hz", file=sys.stderr)
    print(f"python3 tools/rf/iq_carrier.py {a.out} --format cs8 --rate {a.rate:g} "
          f"--center {center:.0f} --start {start}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
