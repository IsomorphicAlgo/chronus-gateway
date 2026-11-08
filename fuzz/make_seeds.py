#!/usr/bin/env python3
"""Build the starting corpus for each fuzz target from the repository's own fixtures.

Usage (from the repository root or from fuzz/):

    python3 fuzz/make_seeds.py            # writes fuzz/seeds/<target>/<name>

The seeds are regenerated rather than committed, so they always follow the fixtures. Every input
is synthetic or public (see demo/fixtures/README.md and demo/fixtures/iers/README.md); nothing here
is operational telemetry. Standard library only.
"""

from __future__ import annotations

import hashlib
import json
import pathlib
import struct

FUZZ = pathlib.Path(__file__).resolve().parent
ROOT = FUZZ.parent
SEEDS = FUZZ / "seeds"


def put(target: str, data: bytes) -> None:
    d = SEEDS / target
    d.mkdir(parents=True, exist_ok=True)
    (d / hashlib.sha1(data).hexdigest()).write_bytes(data)


def hex_lines(path: pathlib.Path) -> list[bytes]:
    """One datagram per non-comment line; spaces between hex digits are allowed."""
    out = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            out.append(bytes.fromhex(line.replace(" ", "")))
    return out


def jsonl(path: pathlib.Path) -> list[dict]:
    rows = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            rows.append(json.loads(line))
    return rows


def space_packet(apid: int, seq: int, data: bytes) -> bytes:
    """CCSDS TM space packet, no secondary header."""
    head = struct.pack(">HHH", apid & 0x7FF, 0xC000 | (seq & 0x3FFF), len(data) - 1)
    return head + data


def cds_secondary_header(days: int, ms: int, us: int = 0) -> bytes:
    return struct.pack(">HIH", days, ms, us)


def space_packet_sh(apid: int, seq: int, sh: bytes, data: bytes) -> bytes:
    body = sh + data
    head = struct.pack(">HHH", 0x0800 | (apid & 0x7FF), 0xC000 | (seq & 0x3FFF), len(body) - 1)
    return head + body


def hil_payload(seq: int, eps: float, thermal: float, rate: float) -> bytes:
    return b"CHI1" + bytes([1, 0, 0, 0]) + struct.pack(">Ifff", seq, eps, thermal, rate)


def main() -> None:
    fixtures = ROOT / "demo" / "fixtures"
    replay = ROOT / "demo" / "replay" / "fixtures"

    # --- CCSDS packets (ccsds_packet, packet_pipeline) -------------------------------------
    packets: list[bytes] = []
    for f in sorted(fixtures.glob("*/*.hex")):
        packets += hex_lines(f)
    packets += [bytes.fromhex(r["udp_hex"]) for r in jsonl(replay / "golden_tm.jsonl")]
    # Time-tagged packets (CDS days since 1958, CUC-shaped bytes reuse the same field).
    sh = cds_secondary_header(23_570, 18_510_000)
    packets.append(space_packet_sh(0x100, 1, sh, b"HELLO"))
    packets.append(space_packet_sh(0x7B0, 2, sh, hil_payload(2, 28.1, 19.5, 0.4)))
    packets.append(space_packet(0x7B1, 3, hil_payload(3, 27.9, 21.0, -0.2)))
    for p in packets:
        put("ccsds_packet", p)
        put("packet_pipeline", p)

    # --- HIL TM v1 payloads ----------------------------------------------------------------
    for i, (v, t, r) in enumerate([(28.0, 20.0, 0.5), (0.0, -40.0, 0.0), (36.5, 85.0, 12.0)]):
        put("hil_tm_v1", hil_payload(i, v, t, r))

    # --- OEM (first 40 lines of each fixture keep the header and a few states) --------------
    for f in sorted((ROOT / "crates" / "gateway" / "tests" / "data").glob("*.oem")):
        lines = f.read_text(encoding="utf-8").splitlines(keepends=True)
        put("oem_text", "".join(lines[:40]).encode())

    # --- gateway TOML ----------------------------------------------------------------------
    for f in [ROOT / "gateway.example.toml", ROOT / "fly.toml", *sorted((ROOT / "deploy").glob("*.toml")),
              ROOT / "demo" / "gateway.docker.toml"]:
        if f.exists() and f.name != "fly.toml":
            put("config_toml", f.read_bytes())
    put("config_toml", b"")
    put("config_toml", b'[time]\nformat = "cds"\nmax_skew_s = 0.0\n[rf]\nbind_addr = "127.0.0.1:7302"\n')

    # --- chronus.rf.v1 datagrams -----------------------------------------------------------
    for r in jsonl(replay / "rf_lab.jsonl")[:4]:
        rf = dict(r.get("rf") or {})
        rf["chronus_schema"] = "chronus.rf.v1"
        rf["t"] = r.get("onboard_time")
        put("rf_datagram", json.dumps(rf).encode())
    put("rf_datagram", b'{"chronus_schema":"chronus.rf.v1"}')

    # --- IERS finals (CSV head; fixed-width rows in the documented layout) -------------------
    csv = (fixtures / "iers" / "finals2000A.daily.csv").read_text(encoding="utf-8").splitlines(keepends=True)
    put("eop_finals", "".join(csv[:12]).encode())
    def finals_line(yy: int, mm: int, dd: int, mjd: int, x: float, y: float, dut1: float, flag: str) -> str:
        # Same layout as chronus_gateway::freshness::finals_line (IERS finals2000A fixed width).
        return (f"{yy:02}{mm:02}{dd:02} {mjd:8.2f} {flag} {x:9.6f}{0.000091:9.6f} {y:9.6f}{0.000088:9.6f}"
                f"  {flag}{dut1:10.7f}{0.000015:10.7f}\n")

    fixed = [
        finals_line(26, 8, 10, 61262, 0.223254, 0.358654, 0.0114721, "I"),
        finals_line(26, 8, 11, 61263, 0.222851, 0.357831, 0.0117123, "I"),
        finals_line(26, 8, 12, 61264, 0.222400, 0.357000, 0.0119000, "P"),
    ]
    put("eop_finals", "".join(fixed).encode())

    # tm_deframer takes a structured (Arbitrary) input, so raw frames are not useful seeds; one
    # small input gives libFuzzer a directory to start from and it builds the corpus itself.
    put("tm_deframer", bytes(range(64)))

    # Inputs that once crashed a target stay in its corpus for good (fuzz/regressions/<target>/).
    for f in sorted((FUZZ / "regressions").glob("*/*")):
        put(f.parent.name, f.read_bytes())
    n = sum(1 for _ in SEEDS.rglob("*") if _.is_file())
    print(f"wrote {n} seeds under {SEEDS.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
