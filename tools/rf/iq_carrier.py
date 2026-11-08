#!/usr/bin/env python3
"""Carrier frequency track from an SDR IQ recording, as chronus.rf.v1 lines.

Tranche 5.3 (Methodology D-056): the first step of the real-RF Doppler check. The output feeds
`chronus-doppler` (residuals against T-DOPPLER) or, sent over UDP, the gateway's RF side channel.

    python3 tools/rf/iq_carrier.py pass.cf32 --format cf32 --rate 48000 \\
        --center 437500000 --start 2026-11-10T05:04:00Z > pass.rf.jsonl

    # SigMF recordings carry rate, centre and start time in their metadata:
    python3 tools/rf/iq_carrier.py pass.sigmf-data

    # gqrx file names carry them too (gqrx_YYYYMMDD_HHMMSS_<centre>_<rate>_fc.raw):
    python3 tools/rf/iq_carrier.py gqrx_20261110_050400_437500000_48000_fc.raw

Method, per block (default 0.5 s): Hann window, FFT, the strongest bin inside the search window
(DC notch excluded: an RTL-SDR's own spike sits there), quadratic interpolation of the log power
for a sub-bin estimate, and a signal-to-noise gate against the median bin. Once locked, the next
search is centred on the last estimate (`--track-hz`), which rejects interferers elsewhere in the
band. Works for a continuous carrier or CW beacon; bursty FM/AFSK downlinks give gaps and
deviation noise, so pick a satellite with a steady carrier (docs/RF_VALIDATION.md).

The rx power field is left out: SDR levels are uncalibrated. Requires numpy.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import re
import sys

import numpy as np

DTYPES = {
    # name: (numpy dtype of one component, scale to ±1, offset)
    "cu8": (np.uint8, 1 / 127.5, -127.5),
    "cs8": (np.int8, 1 / 128.0, 0.0),
    "cs16": (np.int16, 1 / 32768.0, 0.0),
    "cf32": (np.float32, 1.0, 0.0),
}
SIGMF_TYPES = {"cu8": "cu8", "ci8": "cs8", "ci16_le": "cs16", "cf32_le": "cf32"}


def parse_time(s: str) -> dt.datetime:
    s = s.strip().replace("Z", "+00:00")
    t = dt.datetime.fromisoformat(s)
    if t.tzinfo is None:
        raise SystemExit(f"start time {s!r} needs a zone (use Z for UTC)")
    return t.astimezone(dt.timezone.utc)


def sniff(path: pathlib.Path, a: argparse.Namespace) -> None:
    """Fills format / rate / centre / start from SigMF metadata or a gqrx file name."""
    meta = path.with_suffix(".sigmf-meta")
    if path.suffix == ".sigmf-data" and meta.exists():
        m = json.loads(meta.read_text(encoding="utf-8"))
        g = m.get("global", {})
        cap = (m.get("captures") or [{}])[0]
        a.format = a.format or SIGMF_TYPES.get(g.get("core:datatype", ""))
        a.rate = a.rate or g.get("core:sample_rate")
        a.center = a.center or cap.get("core:frequency")
        if not a.start and cap.get("core:datetime"):
            a.start = cap["core:datetime"]
    g = re.match(r"gqrx_(\d{8})_(\d{6})_(\d+)_(\d+)_fc\.raw$", path.name)
    if g:
        a.format = a.format or "cf32"
        a.center = a.center or float(g.group(3))
        a.rate = a.rate or float(g.group(4))
        if not a.start:
            # gqrx names use local time unless the recording machine runs on UTC; say so.
            print("note: gqrx file names are in the recording machine's local time; "
                  "pass --start if that was not UTC", file=sys.stderr)
            a.start = dt.datetime.strptime(g.group(1) + g.group(2), "%Y%m%d%H%M%S").replace(
                tzinfo=dt.timezone.utc).isoformat()


def blocks(path: pathlib.Path, fmt: str, n: int):
    """Yields complex64 blocks of `n` samples (the last partial block is dropped)."""
    dtype, scale, offset = DTYPES[fmt]
    item = np.dtype(dtype).itemsize * 2
    with path.open("rb") as f:
        while True:
            raw = f.read(n * item)
            if len(raw) < n * item:
                return
            v = (np.frombuffer(raw, dtype=dtype).astype(np.float32) + offset) * scale
            yield (v[0::2] + 1j * v[1::2]).astype(np.complex64)


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("iq", type=pathlib.Path, help="raw interleaved IQ (or .sigmf-data)")
    p.add_argument("--format", choices=sorted(DTYPES), help="sample format")
    p.add_argument("--rate", type=float, help="sample rate, samples/s")
    p.add_argument("--center", type=float, help="tuned centre frequency, Hz")
    p.add_argument("--start", help="UTC time of the first sample, e.g. 2026-11-10T05:04:00Z")
    p.add_argument("--block", type=float, default=0.5, help="seconds per estimate (0.5)")
    p.add_argument("--expect", type=float, help="expected carrier, Hz (default: the centre)")
    p.add_argument("--search-hz", type=float, default=15_000.0,
                   help="first search: ± this around --expect (15 kHz covers UHF LEO Doppler)")
    p.add_argument("--track-hz", type=float, default=600.0,
                   help="after lock: ± this around the last estimate (600 Hz); 0 = never track")
    p.add_argument("--dc-notch-hz", type=float, default=200.0, help="ignore ± this around DC")
    p.add_argument("--snr-db", type=float, default=10.0, help="minimum peak / median (10 dB)")
    a = p.parse_args(argv)

    sniff(a.iq, a)
    missing = [k for k in ("format", "rate", "center", "start") if not getattr(a, k)]
    if missing:
        p.error("missing " + ", ".join("--" + m for m in missing)
                + " (not in SigMF metadata or a gqrx file name)")
    t0 = parse_time(str(a.start))
    rate, center = float(a.rate), float(a.center)
    n = max(64, int(round(rate * a.block)))
    nfft = 1 << (n - 1).bit_length()
    win = np.hanning(n).astype(np.float32)
    freqs = np.fft.fftshift(np.fft.fftfreq(nfft, 1.0 / rate))  # offsets from the centre, Hz
    df = rate / nfft
    expect = (a.expect if a.expect is not None else center) - center
    last: float | None = None
    kept = skipped = 0
    out = sys.stdout
    for i, x in enumerate(blocks(a.iq, a.format, n)):
        spec = np.fft.fftshift(np.abs(np.fft.fft(x * win, nfft)) ** 2)
        lo, hi = ((last - a.track_hz, last + a.track_hz) if last is not None and a.track_hz > 0
                  else (expect - a.search_hz, expect + a.search_hz))
        mask = (freqs >= lo) & (freqs <= hi) & (np.abs(freqs) > a.dc_notch_hz)
        idx = np.flatnonzero(mask)
        if idx.size < 3:
            skipped += 1
            continue
        k = idx[np.argmax(spec[idx])]
        noise = float(np.median(spec[mask])) or 1e-30
        snr = 10 * np.log10(spec[k] / noise)
        if snr < a.snr_db or k == 0 or k == nfft - 1:
            skipped += 1
            last = None  # lost lock: widen the search again
            continue
        # Quadratic (parabolic) interpolation on log power around the peak bin.
        ym, y0, yp = np.log(spec[k - 1:k + 2] + 1e-30)
        den = ym - 2 * y0 + yp
        delta = 0.5 * (ym - yp) / den if den != 0 else 0.0
        off = float(freqs[k] + delta * df)
        last = off
        t = t0 + dt.timedelta(seconds=(i + 0.5) * n / rate)
        out.write(json.dumps({
            "chronus_schema": "chronus.rf.v1",
            "t": t.isoformat(timespec="milliseconds").replace("+00:00", "Z"),
            "carrier_hz": round(center + off, 3),
        }) + "\n")
        kept += 1
    print(f"{kept} estimates, {skipped} blocks below {a.snr_db} dB or out of window "
          f"(bin {df:.2f} Hz, block {n / rate:.3f} s)", file=sys.stderr)
    return 0 if kept else 1


if __name__ == "__main__":
    sys.exit(main())
