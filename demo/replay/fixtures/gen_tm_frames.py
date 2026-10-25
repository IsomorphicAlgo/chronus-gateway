#!/usr/bin/env python3
"""Generate `tm_frames_lab.hex`: a lab-made CCSDS TM Transfer Frame (CADU) stream.

This is an **independent** implementation of the downlink side of CCSDS 132.0-B-3 (TM Space Data
Link Protocol) and the sync/randomization layer of CCSDS 131.0-B-4, written in plain Python with
no shared code with the Rust gateway. The gateway's deframer (`crates/gateway/src/frames.rs`) must
recover exactly the packets listed in `tm_frames_lab.expected.json`; the cross-check lives in
`crates/gateway/tests/tm_frames_fixture.rs` (Methodology D-048).

Everything here is synthetic (AGENTS.md): invented spacecraft id, generic payload bytes, onboard
times during a public-TLE ISS pass (2020-07-13 05:08:30 UTC) so the physics checks have something
to evaluate. No operational mission data.

What the stream exercises:
  * 223-octet frames: ASM 1ACFFC1D, pseudo-randomized, CRC-16 FECF
  * three virtual channels: VC0 housekeeping (frames carry an OCF / CLCW word),
    VC1 science (frames carry a 4-octet transfer-frame secondary header), VC7 idle (OID) frames
  * packets that span up to four frames, a packet header split across a frame boundary,
    idle packets used as fill
  * one frame with a single bit error (fails the CRC; the packet it carried is dropped and the
    one after it is recovered through the First Header Pointer)
  * junk between frames, two CADUs in one datagram, one CADU split over two datagrams

Run from the repository root:  python3 demo/replay/fixtures/gen_tm_frames.py
"""

import datetime as dt
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
FRAME_LEN = 223
SCID = 0x2A
ASM = bytes.fromhex("1ACFFC1D")
EPOCH_1958 = dt.datetime(1958, 1, 1, tzinfo=dt.timezone.utc)
T0 = dt.datetime(2020, 7, 13, 5, 8, 30, tzinfo=dt.timezone.utc)


# --- CRC-16-CCITT (0x1021, init 0xFFFF), table-driven -------------------------------------------
def _crc_table():
    table = []
    for i in range(256):
        c = i << 8
        for _ in range(8):
            c = ((c << 1) ^ 0x1021) if c & 0x8000 else (c << 1)
        table.append(c & 0xFFFF)
    return table


CRC_TABLE = _crc_table()


def crc16(data: bytes) -> int:
    crc = 0xFFFF
    for b in data:
        crc = ((crc << 8) & 0xFFFF) ^ CRC_TABLE[((crc >> 8) ^ b) & 0xFF]
    return crc


assert crc16(b"123456789") == 0x29B1  # standard check value


# --- CCSDS pseudo-randomizer: h(x) = x^8 + x^7 + x^5 + x^3 + 1, all-ones seed ----------------------
def pn_bytes(n: int) -> bytes:
    reg = 0xFF  # x1..x8, MSB = oldest
    out = bytearray()
    for _ in range(n):
        byte = 0
        for _ in range(8):
            bit = (reg >> 7) & 1
            fb = ((reg >> 7) ^ (reg >> 4) ^ (reg >> 2) ^ reg) & 1
            reg = ((reg << 1) | fb) & 0xFF
            byte = (byte << 1) | bit
        out.append(byte)
    return bytes(out)


PN = pn_bytes(FRAME_LEN)
assert PN[:8] == bytes.fromhex("FF480EC09A0D70BC")


def randomize(frame: bytes) -> bytes:
    return bytes(a ^ b for a, b in zip(frame, PN))


# --- Space Packets ---------------------------------------------------------------------------------
def cds(t: dt.datetime) -> bytes:
    delta = t - EPOCH_1958
    days = delta.days
    ms = (delta.seconds * 1000) + delta.microseconds // 1000
    return days.to_bytes(2, "big") + ms.to_bytes(4, "big")


def space_packet(apid: int, seq: int, data: bytes, sec_hdr: bool) -> bytes:
    word1 = (0 << 13) | (0 << 12) | (int(sec_hdr) << 11) | apid
    word2 = (0b11 << 14) | (seq & 0x3FFF)
    return word1.to_bytes(2, "big") + word2.to_bytes(2, "big") + (len(data) - 1).to_bytes(2, "big") + data


def idle_packet(length: int) -> bytes:
    assert length >= 7
    return space_packet(0x7FF, 0, b"\x55" * (length - 6), False)


# --- TM Transfer Frames ----------------------------------------------------------------------------
class VirtualChannel:
    def __init__(self, vcid: int, ocf: bool, sec_hdr_len: int):
        self.vcid = vcid
        self.ocf = ocf
        self.sec_hdr_len = sec_hdr_len  # total TF secondary header octets (0 = none)
        self.count = 0
        self.buf = bytearray()
        self.starts = []  # absolute offsets of packet starts
        self.emitted = 0

    def data_len(self) -> int:
        return FRAME_LEN - 6 - self.sec_hdr_len - (4 if self.ocf else 0) - 2


class Mux:
    def __init__(self):
        self.mc = 0
        self.frames = []  # list of (vcid, frame bytes)

    def frame(self, vc: VirtualChannel, fhp: int, data: bytes) -> bytes:
        ident = (0 << 14) | (SCID << 4) | (vc.vcid << 1) | int(vc.ocf)
        status = (int(vc.sec_hdr_len > 0) << 15) | (0 << 14) | (0 << 13) | (0b11 << 11) | fhp
        f = ident.to_bytes(2, "big") + bytes([self.mc, vc.count]) + status.to_bytes(2, "big")
        if vc.sec_hdr_len:
            # version 00 + length field (= total length - 1), then mission-specific bytes.
            f += bytes([vc.sec_hdr_len - 1]) + bytes(range(1, vc.sec_hdr_len))
        f += data
        if vc.ocf:
            f += bytes.fromhex("01000000") if vc.count % 2 else bytes.fromhex("01000100")  # CLCW-like
        f += crc16(f).to_bytes(2, "big")
        assert len(f) == FRAME_LEN, len(f)
        self.mc = (self.mc + 1) % 256
        vc.count = (vc.count + 1) % 256
        return f

    def queue(self, vc: VirtualChannel, packet: bytes):
        vc.starts.append(vc.emitted + len(vc.buf))
        vc.buf += packet
        while len(vc.buf) >= vc.data_len():
            self.emit(vc)

    def emit(self, vc: VirtualChannel):
        n = vc.data_len()
        lo = vc.emitted
        vc.starts = [s for s in vc.starts if s >= lo]
        fhp = (vc.starts[0] - lo) if vc.starts and vc.starts[0] < lo + n else 0x7FF
        data, vc.buf = bytes(vc.buf[:n]), vc.buf[n:]
        vc.emitted += n
        self.frames.append((vc.vcid, self.frame(vc, fhp, data)))

    def flush(self, vc: VirtualChannel):
        if not vc.buf:
            return
        room = vc.data_len() - len(vc.buf) % vc.data_len()
        self.queue(vc, idle_packet(room if room >= 7 else room + vc.data_len()))

    def idle_frame(self, vc: VirtualChannel):
        self.frames.append((vc.vcid, self.frame(vc, 0x7FE, b"\x55" * vc.data_len())))


def main():
    hk = VirtualChannel(0, ocf=True, sec_hdr_len=0)
    sci = VirtualChannel(1, ocf=False, sec_hdr_len=4)
    idle = VirtualChannel(7, ocf=False, sec_hdr_len=0)
    mux = Mux()
    expected = []  # (vcid, packet) in delivery order

    ends = []  # (vc, absolute end offset) per expected packet, to compute delivery order

    def send(vc, apid, seq, t, payload):
        pkt = space_packet(apid, seq, cds(t) + payload, True)
        start = vc.emitted + len(vc.buf)
        mux.queue(vc, pkt)
        expected.append((vc.vcid, pkt))
        ends.append((vc, start + len(pkt)))
        return pkt

    hk_seq = 0
    sci_seq = 0
    t = T0
    for i in range(12):
        # Housekeeping: a 40-byte packet every half second.
        payload = bytes([i]) + b"CHRONUS-LAB-HK" + bytes((i * 7 + k) % 256 for k in range(19))
        send(hk, 0x120, hk_seq, t, payload)
        hk_seq += 1
        if i % 4 == 1:
            # Science: a 500-byte packet spanning three frames on VC1.
            body = bytes((i * 31 + k * 3) % 256 for k in range(488))
            send(sci, 0x200, sci_seq, t, body)
            sci_seq += 1
        if i % 5 == 4:
            mux.idle_frame(idle)
        t += dt.timedelta(milliseconds=500)
    # A short packet whose 6-byte header will straddle a frame boundary on VC0.
    fill = hk.data_len() - (len(hk.buf) % hk.data_len()) - 3
    if fill >= 7:
        mux.queue(hk, idle_packet(fill))
    send(hk, 0x121, 0, t, b"SPLIT-HEADER")
    mux.flush(hk)
    mux.flush(sci)

    # A packet is delivered when the frame holding its last byte arrives: order by that frame.
    frame_index = {}
    for vcid in (0, 1, 7):
        frame_index[vcid] = [k for k, (v, _) in enumerate(mux.frames) if v == vcid]
    order = [
        frame_index[vc.vcid][(end - 1) // vc.data_len()] for vc, end in ends
    ]
    expected = [e for _, e in sorted(zip(order, expected), key=lambda pair: pair[0])]

    # Corrupt one bit in the second science frame: the CRC rejects it, and the science packet that
    # crossed it can no longer be rebuilt.
    sci_frames = [k for k, (vc, _) in enumerate(mux.frames) if vc == 1]
    bad = sci_frames[1]
    vc, f = mux.frames[bad]
    f = bytearray(f)
    f[100] ^= 0x10
    mux.frames[bad] = (vc, bytes(f))
    # Which science packets touch the corrupted frame? Rebuild the VC1 byte ranges to find out.
    sci_data = sci.data_len()
    k_in_vc = sci_frames.index(bad)
    lo, hi = k_in_vc * sci_data, (k_in_vc + 1) * sci_data
    offset = 0
    dropped = set()
    for vcid, pkt in [e for e in expected if e[0] == 1]:
        if offset < hi and offset + len(pkt) > lo:
            dropped.add(pkt)
        offset += len(pkt)
    expected = [e for e in expected if not (e[0] == 1 and e[1] in dropped)]

    # Wrap as CADUs and shape them into datagrams.
    cadus = [ASM + randomize(f) for _, f in mux.frames]
    datagrams = []
    k = 0
    while k < len(cadus):
        if k == 3:  # junk before a frame (sync slip)
            datagrams.append(bytes.fromhex("DEADBEEF00") + cadus[k])
            k += 1
        elif k == 6 and k + 1 < len(cadus):  # two CADUs in one datagram
            datagrams.append(cadus[k] + cadus[k + 1])
            k += 2
        elif k == 9:  # one CADU split over two datagrams
            datagrams.append(cadus[k][:100])
            datagrams.append(cadus[k][100:])
            k += 1
        else:
            datagrams.append(cadus[k])
            k += 1

    with open(os.path.join(HERE, "tm_frames_lab.hex"), "w") as out:
        out.write("# Lab-generated CCSDS TM Transfer Frame stream (CADUs) -- see gen_tm_frames.py\n")
        out.write(f"# {len(mux.frames)} frames of {FRAME_LEN} octets, SCID {SCID:#x}, ASM + randomized + FECF;\n")
        out.write("# one datagram per line. Gateway: [framing] mode=\"tm_frames\", frame_length=223,\n")
        out.write("# asm=true, derandomize=true; [time] format=\"cds\", max_skew_s=0.0\n")
        for d in datagrams:
            out.write(d.hex().upper() + "\n")

    def apid_seq(p):
        return (int.from_bytes(p[0:2], "big") & 0x7FF, int.from_bytes(p[2:4], "big") & 0x3FFF)

    doc = {
        "description": "Packets the gateway must extract from tm_frames_lab.hex, in order.",
        "frames": len(mux.frames),
        "frames_crc_error": 1,
        "frames_idle": sum(1 for vc, _ in mux.frames if vc == 7),
        "datagrams": len(datagrams),
        "packets": [
            {
                "vcid": vcid,
                "apid": apid_seq(p)[0],
                "seq": apid_seq(p)[1],
                "len": len(p),
                "hex": p.hex().upper(),
            }
            for vcid, p in expected
        ],
    }
    with open(os.path.join(HERE, "tm_frames_lab.expected.json"), "w") as out:
        json.dump(doc, out, indent=1)
        out.write("\n")
    print(f"{len(mux.frames)} frames, {len(datagrams)} datagrams, {len(expected)} packets expected "
          f"({len(dropped)} science packet(s) lost to the CRC error)")


if __name__ == "__main__":
    main()
