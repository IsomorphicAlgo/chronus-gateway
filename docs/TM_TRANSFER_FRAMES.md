# CCSDS TM Transfer Frames

*Added in 0.5.0 (Tranche 3 of the 1.0 plan, Methodology D-048).*

Before 0.5.0 the gateway expected **one CCSDS Space Packet per UDP datagram**. That's what simulators
and many lab tools send, but a real downlink looks different:

- Packets travel inside fixed-length **TM Transfer Frames** (CCSDS 132.0-B-3).
- Several **virtual channels** share one link, for example housekeeping on VC 0, science on VC 1,
  and idle fill on VC 7.
- A packet can **span several frames**, and one frame can hold the end of one packet and the
  start of the next.
- The coding layer (CCSDS 131.0-B-4) puts a **sync marker** (`1ACFFC1D`) in front of each frame
  and XORs the frame with a **pseudo-random sequence**.

With `[framing] mode = "tm_frames"` the gateway undoes all of that and recovers the packets. The
physics checks, the Open MCT messages and the history then work exactly as before.

```text
 datagrams ──► sync-marker search ──► derandomize ──► CRC check ──► header / spacecraft / VC filter
                                                                       │
          Space Packets ◄── per-virtual-channel reassembly (First Header Pointer) ◄┘
```

The default is still `mode = "packets"`, so an existing deployment behaves exactly as before.

---

## 1. Turn it on

The settings are the link's **managed parameters**. They have to match how the spacecraft and your
front end were set up:

```toml
[framing]
mode = "tm_frames"          # "packets" (default) | "tm_frames"
frame_length = 223          # octets, frame header through CRC, WITHOUT the 4-byte sync marker
asm = true                  # frames are preceded by 1ACFFC1D (a CADU stream)
derandomize = true          # undo the CCSDS pseudo-randomizer
fecf = true                 # last 2 octets are a CRC-16 that is checked (default true)
# spacecraft_id = 42        # accept only this spacecraft (10 bits); unset = any
# virtual_channels = [0, 1] # accept only these VCs (0-7); unset = all
```

| Setting | When to change it |
|---|---|
| `frame_length` | Always set it to the mission's frame length. It is **required with `asm = true`**: the gateway searches for the marker and then takes this many bytes. Without the sync marker it can be left unset, and each datagram is then exactly one frame. |
| `asm` | `true` if your front end forwards whole CADUs (marker included). `false` if it strips the marker and sends one frame (or a whole number of frames) per datagram. |
| `derandomize` | `true` if the frames are still randomized. Most decoders that strip the marker also derandomize, so check what yours does. |
| `fecf` | `false` if the mission's frames carry no CRC. Many missions rely on Reed-Solomon alone. |
| `spacecraft_id`, `virtual_channels` | Use these to ignore other spacecraft or channels on a shared link. Filtered frames are counted, not reported as errors. |

**Feed the gateway frames that have already been FEC-decoded.** Reed-Solomon, convolutional and LDPC
decoding belong in the radio's software (gr-satellites and most SDR decoders do it). The gateway
starts at the frame layer.

## 2. What it does with each frame

1. **Sync.** With `asm = true` the byte stream is searched for `1ACFFC1D`. Junk between frames is
   skipped and counted. A frame may be split across datagrams. After a bad frame, the search
   resumes one byte past its marker, so the next good frame is not lost.
2. **Derandomize** (if configured) and **check the CRC** (if configured): CRC-16-CCITT, polynomial
   `0x1021`, preset `0xFFFF`.
3. **Header.** It requires version `00`, the spacecraft and virtual-channel filters, and a
   synchronisation flag of `0` (packet data). The secondary header and the Operational Control
   Field are skipped, not interpreted.
4. **Frame counters.** A gap in a virtual channel's counter means frames were lost upstream. The
   partly built packet on that channel is abandoned, and the gap is counted.
5. **Packet extraction.** The **First Header Pointer** says where the first packet header in the
   frame starts. Bytes before it complete the packet in progress, and packets are cut by their
   own length fields. When the pointer and the stream disagree, the partial packet is dropped and
   the channel re-syncs on the pointer.
6. **Idle data is discarded**: idle frames (pointer `0x7FE`) and idle packets (APID `0x7FF`).

Each recovered packet then goes through the normal path: parse, spacecraft time, receiver data,
physics, then Open MCT JSON. The JSON gains **`vcid`**, the virtual channel the packet came on.

All input is treated as hostile. Lengths are checked before use, the reassembly buffer per channel
is bounded by the 64 KiB packet limit, and garbage cannot cause a panic (there is a test that feeds
random bytes).

## 3. Watching it

`GET /api/v1/chronus/metrics` gains a **`framing`** object:

| Counter | Meaning |
|---|---|
| `datagrams` | datagrams handed to the deframer |
| `frames_ok` | frames that passed every check |
| `frames_crc_error` | frames rejected by the CRC |
| `frames_invalid` | wrong length, not TM, VCA data, or a bad secondary header |
| `frames_other_spacecraft`, `frames_vc_filtered` | frames dropped by the filters |
| `frames_idle` | idle (OID) frames |
| `vc_frame_gaps`, `mc_frame_gaps` | frame-counter discontinuities (frames lost upstream) |
| `packets_extracted` | packets forwarded |
| `packets_idle` | idle packets discarded |
| `packets_dropped` | partial packets abandoned after a gap or an inconsistent pointer |
| `bytes_discarded` | bytes skipped while searching for the sync marker |

`/status` explains problems in plain English:

| Code | Meaning |
|---|---|
| `CG-513` (error) | Frames are arriving but **none** are usable: wrong `frame_length`, `fecf`, `asm` or `derandomize`, frames still carrying RS parity, or filters excluding everything |
| `CG-513` (warning) | More than 10% of frames are being rejected (a marginal link, or datagrams cut short) |
| `CG-513` (info) | Frame-counter gaps: frames were lost before reaching the gateway |
| `CG-134` | A `[framing]` setting is out of range (startup error) |
| `CG-136` | Without a sync marker, `frame_length` is larger than `ingest.max_datagram_size`, so frames would be cut off |

In packet mode, if the first rejected datagram begins with `1ACFFC1D`, the log says so and points
here.

## 4. Try it without a radio

**Replay a recorded lab stream.** `demo/replay/fixtures/tm_frames_lab.hex` holds 14 CADUs of 223
octets: three virtual channels, packets spanning frames, a packet header split across a boundary,
idle frames and idle packets, one frame with a bit error, junk before a frame, two CADUs in one
datagram, and one CADU split over two datagrams.

```bash
# gateway.toml: [framing] mode="tm_frames", frame_length=223, asm=true, derandomize=true
#               [time] format="cds", max_skew_s=0.0
cargo run -p chronus-gateway -- --config gateway.toml
cargo run -p chronus-replay -- --file demo/replay/fixtures/tm_frames_lab.hex --delay-ms 20
curl -s localhost:8080/api/v1/chronus/metrics | jq .framing
# packets_extracted: 15, frames_crc_error: 1, frames_idle: 2, vc_frame_gaps: 1
```

**Wrap any fixture in frames.** `chronus-replay --tm-frames` packs each line's packet into frames:

```bash
cargo run -p chronus-replay -- --file demo/replay/fixtures/rf_lab.jsonl --rf-dest 127.0.0.1:7302 \
    --tm-frames --frame-length 223 --asm --randomize --scid 42 --vcid 0
```

## 5. How it is verified

- **Cross-implementation check.** The lab stream was produced by
  `demo/replay/fixtures/gen_tm_frames.py`, an independent Python encoder (its own CRC table, PN
  generator and frame builder) that shares no code with the Rust deframer. Its list of the packets
  it put in, `tm_frames_lab.expected.json`, must match what the Rust deframer recovers byte for
  byte, in delivery order, including the one packet the corrupted frame destroys
  (`crates/gateway/tests/tm_frames_fixture.rs`).
- **Standard check values.** The CRC returns `0x29B1` for `"123456789"`, and the pseudo-random
  sequence starts `FF 48 0E C0 9A 0D 70 BC` with a period of 255 octets.
- **Unit tests** cover packets spanning many frames, interleaved virtual channels, lost frames,
  inconsistent pointers, filters, ASM re-sync after a corrupted frame, and random garbage.
- **End to end against the real binary.** `crates/gateway/tests/ground_segment_e2e.rs` streams the
  lab CADUs over UDP into the shipped executable and checks the WebSocket, the history API, the
  metrics and `/status`.

## 6. Not supported (yet)

- FEC decoding (Reed-Solomon, convolutional, LDPC): feed the gateway frames after decoding.
- AOS (CCSDS 732.0) and USLP (CCSDS 732.1) frames, VCA-SDU data (synchronisation flag = 1), and
  packet-order or segment-length variations.
- Interpreting the Operational Control Field (CLCW). It is only needed for commanding, which is
  out of scope (see [TELECOMMAND.md](TELECOMMAND.md)).
- A sync marker with bit errors: the marker must match exactly.
