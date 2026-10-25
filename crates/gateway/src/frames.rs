//! CCSDS **TM Transfer Frames** (Tranche 3, `Methodology.md` D-048).
//!
//! A real downlink rarely carries one bare Space Packet per UDP datagram. The spacecraft wraps
//! packets in fixed-length **TM Transfer Frames** (CCSDS 132.0-B-3, *TM Space Data Link
//! Protocol*), multiplexes several **virtual channels** on one link, lets packets **span frame
//! boundaries**, and the coding layer (CCSDS 131.0-B) puts an **Attached Sync Marker** in front of
//! each frame and XORs it with a pseudo-random sequence. This module undoes that, so the rest of the
//! gateway still sees one Space Packet at a time:
//!
//! ```text
//!  datagrams ──► [ASM search] ──► [derandomize] ──► [FECF check] ──► [header / SCID / VC filter]
//!                                                                         │
//!            Space Packets ◄── [per-VC reassembly via First Header Pointer] ◄┘
//! ```
//!
//! What is supported (all **managed parameters** in CCSDS terms are configuration, see
//! [`FramingConfig`]):
//!
//! | Feature | Reference | Notes |
//! |---|---|---|
//! | Primary header, master / VC frame counters | 132.0-B-3 §4.1.2 | gaps are counted and reset the affected VC |
//! | Secondary header (skipped), OCF (skipped) | §4.1.3, §4.1.5 | contents are not interpreted |
//! | Frame Error Control Field (CRC-16) | §4.1.6 | CRC-16-CCITT, poly `0x1021`, init `0xFFFF` |
//! | Packet extraction via First Header Pointer | §4.1.2.7 | packets may span any number of frames; idle frames (`0x7FE`) and idle packets (APID `0x7FF`) are dropped |
//! | Attached Sync Marker `1ACFFC1D` | 131.0-B-4 §9 | exact match; frames may be split across datagrams |
//! | Pseudo-randomizer | 131.0-B-4 §10 | `h(x) = x⁸+x⁷+x⁵+x³+1`, all-ones seed |
//!
//! Not supported: Reed-Solomon / LDPC / convolutional decoding (feed frames *after* the FEC
//! decoder, which is what `gr-satellites` and most SDR front ends emit), VCA-SDU frames
//! (synchronisation flag = 1), and the AOS / USLP frame formats.
//!
//! All input is untrusted: every length is checked before it is used, buffers are bounded by the
//! frame length and the 64 KiB CCSDS packet limit, and nothing here can panic on bad data.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::{FramingConfig, FramingMode};

/// The CCSDS Attached Sync Marker that precedes each frame on a coded link (131.0-B-4 §9.3).
pub const ASM: [u8; 4] = [0x1A, 0xCF, 0xFC, 0x1D];
/// Length of the TM Transfer Frame primary header.
pub const TM_PRIMARY_HEADER_LEN: usize = 6;
/// Largest TM Transfer Frame (132.0-B-3 §4.1.1: at most 2048 octets).
pub const TM_FRAME_MAX_LEN: usize = 2048;
/// Smallest frame length the gateway accepts: a primary header plus at least two data octets.
pub const TM_FRAME_MIN_LEN: usize = 8;
/// First Header Pointer value meaning "no packet starts in this frame".
pub const FHP_NO_PACKET_START: u16 = 0x7FF;
/// First Header Pointer value meaning "this frame carries only idle data" (OID frame).
pub const FHP_IDLE: u16 = 0x7FE;
/// The idle-packet APID (133.0-B-2 §4.1.3.3.4.4); idle packets are filler and never forwarded.
pub const IDLE_APID: u16 = 0x7FF;
/// Length of the CCSDS Space Packet primary header.
const PACKET_HEADER_LEN: usize = 6;
/// Largest CCSDS Space Packet: 6-byte header + 65536-byte data field.
const PACKET_MAX_LEN: usize = 65_542;
/// Length of the Operational Control Field, when present.
const OCF_LEN: usize = 4;
/// Length of the Frame Error Control Field, when present.
const FECF_LEN: usize = 2;

// ---------------------------------------------------------------------------------------------
// CRC-16 and the pseudo-randomizer
// ---------------------------------------------------------------------------------------------

/// CRC-16-CCITT as specified for the TM Frame Error Control Field (132.0-B-3 §4.1.6):
/// polynomial `x¹⁶+x¹²+x⁵+1` (`0x1021`), register preset to all ones, no reflection, no final XOR.
///
/// # Examples
///
/// ```
/// use chronus_gateway::frames::crc16_ccitt;
///
/// // The standard check value for this CRC ("CRC-16/IBM-3740", a.k.a. CCITT-FALSE).
/// assert_eq!(crc16_ccitt(b"123456789"), 0x29B1);
/// ```
#[must_use]
pub fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// One period (255 octets) of the CCSDS pseudo-random sequence, generated at compile time.
///
/// Fibonacci LFSR for `h(x) = x⁸+x⁷+x⁵+x³+1`, all-ones seed, most significant bit first
/// (131.0-B-4 §10.4). The sequence begins `FF 48 0E C0 9A 0D 70 BC`.
const PN_SEQUENCE: [u8; 255] = {
    let mut out = [0u8; 255];
    let mut state: [u8; 8] = [1; 8];
    let mut i = 0;
    while i < 255 * 8 {
        let bit = state[0];
        let feedback = state[0] ^ state[3] ^ state[5] ^ state[7];
        let mut j = 0;
        while j < 7 {
            state[j] = state[j + 1];
            j += 1;
        }
        state[7] = feedback;
        out[i / 8] |= bit << (7 - (i % 8));
        i += 1;
    }
    out
};

/// XORs `frame` with the CCSDS pseudo-random sequence, in place.
///
/// The operation is its own inverse: the same call randomizes a clean frame and derandomizes a
/// received one. It applies to the frame **after** the Attached Sync Marker.
///
/// # Examples
///
/// ```
/// use chronus_gateway::frames::ccsds_randomize;
///
/// let mut zeros = [0u8; 4];
/// ccsds_randomize(&mut zeros);
/// assert_eq!(zeros, [0xFF, 0x48, 0x0E, 0xC0]); // the sequence itself
/// ccsds_randomize(&mut zeros);
/// assert_eq!(zeros, [0; 4]);
/// ```
pub fn ccsds_randomize(frame: &mut [u8]) {
    for (byte, pn) in frame.iter_mut().zip(PN_SEQUENCE.iter().cycle()) {
        *byte ^= pn;
    }
}

// ---------------------------------------------------------------------------------------------
// Frame header parsing
// ---------------------------------------------------------------------------------------------

/// Decoded TM Transfer Frame primary header (132.0-B-3 §4.1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TmFrameHeader {
    /// Transfer Frame Version Number (2 bits; `0` for TM).
    pub version: u8,
    /// Spacecraft Identifier (10 bits).
    pub spacecraft_id: u16,
    /// Virtual Channel Identifier (3 bits).
    pub vcid: u8,
    /// Whether the frame ends with a 4-octet Operational Control Field.
    pub ocf_present: bool,
    /// Master Channel Frame Count (mod 256).
    pub mc_frame_count: u8,
    /// Virtual Channel Frame Count (mod 256).
    pub vc_frame_count: u8,
    /// Whether a Transfer Frame Secondary Header follows the primary header.
    pub secondary_header: bool,
    /// Synchronisation flag: `false` = octet-synchronised packets (the only mode extracted here).
    pub sync_flag: bool,
    /// First Header Pointer (11 bits): offset of the first packet header in the data field.
    pub first_header_pointer: u16,
}

impl TmFrameHeader {
    /// Decodes the six-octet primary header. Returns `None` if `bytes` is shorter than six octets.
    ///
    /// # Examples
    ///
    /// ```
    /// use chronus_gateway::frames::TmFrameHeader;
    ///
    /// // SCID 0x073, VC 1, no OCF, MC count 5, VC count 9, FHP 0.
    /// let h = TmFrameHeader::parse(&[0x07, 0x32, 0x05, 0x09, 0x18, 0x00]).unwrap();
    /// assert_eq!((h.spacecraft_id, h.vcid, h.first_header_pointer), (0x073, 1, 0));
    /// ```
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let h = bytes.get(..TM_PRIMARY_HEADER_LEN)?;
        let id = u16::from_be_bytes([h[0], h[1]]);
        let status = u16::from_be_bytes([h[4], h[5]]);
        Some(Self {
            version: (h[0] >> 6) & 0b11,
            spacecraft_id: (id >> 4) & 0x3FF,
            vcid: ((id >> 1) & 0b111) as u8,
            ocf_present: id & 1 == 1,
            mc_frame_count: h[2],
            vc_frame_count: h[3],
            secondary_header: status & 0x8000 != 0,
            sync_flag: status & 0x4000 != 0,
            first_header_pointer: status & 0x07FF,
        })
    }

    /// Encodes the header back to its six octets (packet-order flag `0`, segment length id `11`).
    #[must_use]
    pub fn to_bytes(&self) -> [u8; TM_PRIMARY_HEADER_LEN] {
        let id = (u16::from(self.version & 0b11) << 14)
            | ((self.spacecraft_id & 0x3FF) << 4)
            | (u16::from(self.vcid & 0b111) << 1)
            | u16::from(self.ocf_present);
        let status = (u16::from(self.secondary_header) << 15)
            | (u16::from(self.sync_flag) << 14)
            | (0b11 << 11)
            | (self.first_header_pointer & 0x07FF);
        let [a, b] = id.to_be_bytes();
        let [c, d] = status.to_be_bytes();
        [a, b, self.mc_frame_count, self.vc_frame_count, c, d]
    }
}

/// Why a TM Transfer Frame was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FrameError {
    /// The frame is shorter than its header, trailer and configured fields require.
    #[error("frame is {len} bytes, too short for its header and trailer")]
    TooShort {
        /// Received length.
        len: usize,
    },
    /// The Transfer Frame Version Number is not `00` (not a TM frame).
    #[error("transfer frame version {0} is not TM (expected 0)")]
    NotTm(u8),
    /// The Frame Error Control Field does not match the frame contents.
    #[error("frame CRC mismatch (computed {computed:#06x}, frame says {received:#06x})")]
    FecfMismatch {
        /// CRC computed over the frame.
        computed: u16,
        /// CRC carried in the frame.
        received: u16,
    },
    /// The synchronisation flag is set: the data field is not a packet stream.
    #[error("frame carries VCA_SDU data (synchronisation flag set), not packets")]
    NotPacketData,
    /// The secondary header claims to be longer than the data field.
    #[error("secondary header length {0} exceeds the frame")]
    BadSecondaryHeader(usize),
}

/// A parsed TM Transfer Frame: the header and a view of its data field.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct TmFrame<'a> {
    /// The primary header.
    pub header: TmFrameHeader,
    /// The Transfer Frame Data Field (secondary header, OCF and FECF removed).
    pub data: &'a [u8],
    /// The Operational Control Field, when present (not interpreted by the gateway).
    pub ocf: Option<[u8; OCF_LEN]>,
}

/// Parses one complete TM Transfer Frame (already derandomized, ASM removed).
///
/// `has_fecf` says whether the last two octets are a Frame Error Control Field; when they are,
/// the CRC is checked first.
///
/// # Errors
///
/// A [`FrameError`] describing the first problem found.
///
/// # Examples
///
/// ```
/// use chronus_gateway::frames::{TmFrameBuilder, parse_tm_frame};
///
/// let mut b = TmFrameBuilder::new(0x73, 64);
/// assert!(b.pack(0, &[&chronus_gateway::encode_synthetic_tm(0x100, 1, b"hello")]).is_empty());
/// let frames = b.flush(0); // pad the partly filled frame with an idle packet
/// let f = parse_tm_frame(&frames[0], true).unwrap();
/// assert_eq!(f.header.spacecraft_id, 0x73);
/// assert_eq!(f.header.first_header_pointer, 0);
/// ```
pub fn parse_tm_frame(frame: &[u8], has_fecf: bool) -> Result<TmFrame<'_>, FrameError> {
    let trailer = if has_fecf { FECF_LEN } else { 0 };
    if frame.len() < TM_PRIMARY_HEADER_LEN + trailer {
        return Err(FrameError::TooShort { len: frame.len() });
    }
    let body_end = frame.len() - trailer;
    if has_fecf {
        let computed = crc16_ccitt(&frame[..body_end]);
        let received = u16::from_be_bytes([frame[body_end], frame[body_end + 1]]);
        if computed != received {
            return Err(FrameError::FecfMismatch { computed, received });
        }
    }
    let header = TmFrameHeader::parse(frame).ok_or(FrameError::TooShort { len: frame.len() })?;
    if header.version != 0 {
        return Err(FrameError::NotTm(header.version));
    }
    if header.sync_flag {
        return Err(FrameError::NotPacketData);
    }
    let mut start = TM_PRIMARY_HEADER_LEN;
    let mut end = body_end;
    let ocf = if header.ocf_present {
        if end < start + OCF_LEN {
            return Err(FrameError::TooShort { len: frame.len() });
        }
        end -= OCF_LEN;
        let mut o = [0u8; OCF_LEN];
        o.copy_from_slice(&frame[end..end + OCF_LEN]);
        Some(o)
    } else {
        None
    };
    if header.secondary_header {
        let id = *frame
            .get(start)
            .ok_or(FrameError::TooShort { len: frame.len() })?;
        // Secondary header length field = total secondary header length − 1 (§4.1.3.2.3).
        let total = usize::from(id & 0x3F) + 1;
        if start + total > end {
            return Err(FrameError::BadSecondaryHeader(total));
        }
        start += total;
    }
    if start >= end {
        return Err(FrameError::TooShort { len: frame.len() });
    }
    Ok(TmFrame {
        header,
        data: &frame[start..end],
        ocf,
    })
}

// ---------------------------------------------------------------------------------------------
// Counters
// ---------------------------------------------------------------------------------------------

/// Cumulative deframing counters (exposed as `framing` in `GET /api/v1/chronus/metrics`).
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct FramingStats {
    /// Datagrams handed to the deframer.
    pub datagrams: AtomicU64,
    /// Frames that passed every check.
    pub frames_ok: AtomicU64,
    /// Frames dropped for a CRC (FECF) mismatch.
    pub frames_crc_error: AtomicU64,
    /// Frames dropped as malformed (wrong length, not TM, VCA_SDU, bad secondary header).
    pub frames_invalid: AtomicU64,
    /// Frames from a spacecraft other than `framing.spacecraft_id`.
    pub frames_other_spacecraft: AtomicU64,
    /// Frames on a virtual channel not listed in `framing.virtual_channels`.
    pub frames_vc_filtered: AtomicU64,
    /// Idle (OID) frames, which carry no packets.
    pub frames_idle: AtomicU64,
    /// Discontinuities in a virtual channel's frame counter (frames lost upstream).
    pub vc_frame_gaps: AtomicU64,
    /// Discontinuities in the master channel frame counter.
    pub mc_frame_gaps: AtomicU64,
    /// Space Packets extracted and forwarded.
    pub packets_extracted: AtomicU64,
    /// Idle packets (APID `0x7FF`) discarded.
    pub packets_idle: AtomicU64,
    /// Partial packets abandoned after a frame gap or an inconsistent First Header Pointer.
    pub packets_dropped: AtomicU64,
    /// Bytes skipped while searching for the Attached Sync Marker.
    pub bytes_discarded: AtomicU64,
}

/// Serializable [`FramingStats`] snapshot.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[non_exhaustive]
pub struct FramingSnapshot {
    /// `"packets"` (one Space Packet per datagram) or `"tm_frames"`.
    pub mode: &'static str,
    /// Datagrams handed to the deframer.
    pub datagrams: u64,
    /// Frames that passed every check.
    pub frames_ok: u64,
    /// Frames dropped for a CRC mismatch.
    pub frames_crc_error: u64,
    /// Frames dropped as malformed.
    pub frames_invalid: u64,
    /// Frames from another spacecraft.
    pub frames_other_spacecraft: u64,
    /// Frames on a filtered-out virtual channel.
    pub frames_vc_filtered: u64,
    /// Idle frames.
    pub frames_idle: u64,
    /// Virtual-channel frame-counter gaps.
    pub vc_frame_gaps: u64,
    /// Master-channel frame-counter gaps.
    pub mc_frame_gaps: u64,
    /// Packets extracted and forwarded.
    pub packets_extracted: u64,
    /// Idle packets discarded.
    pub packets_idle: u64,
    /// Partial packets abandoned.
    pub packets_dropped: u64,
    /// Bytes skipped while hunting for the sync marker.
    pub bytes_discarded: u64,
}

impl FramingStats {
    /// A `Relaxed` snapshot, labelled with the framing `mode`.
    pub fn snapshot(&self, mode: FramingMode) -> FramingSnapshot {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        FramingSnapshot {
            mode: match mode {
                FramingMode::TmFrames => "tm_frames",
                _ => "packets",
            },
            datagrams: l(&self.datagrams),
            frames_ok: l(&self.frames_ok),
            frames_crc_error: l(&self.frames_crc_error),
            frames_invalid: l(&self.frames_invalid),
            frames_other_spacecraft: l(&self.frames_other_spacecraft),
            frames_vc_filtered: l(&self.frames_vc_filtered),
            frames_idle: l(&self.frames_idle),
            vc_frame_gaps: l(&self.vc_frame_gaps),
            mc_frame_gaps: l(&self.mc_frame_gaps),
            packets_extracted: l(&self.packets_extracted),
            packets_idle: l(&self.packets_idle),
            packets_dropped: l(&self.packets_dropped),
            bytes_discarded: l(&self.bytes_discarded),
        }
    }
}

fn bump(a: &AtomicU64) {
    a.fetch_add(1, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------------------------
// The deframer
// ---------------------------------------------------------------------------------------------

/// A Space Packet recovered from the frame stream.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ExtractedPacket<'a> {
    /// Virtual channel the packet travelled on.
    pub vcid: u8,
    /// Spacecraft identifier from the frame header.
    pub spacecraft_id: u16,
    /// The complete Space Packet (primary header included).
    pub bytes: &'a [u8],
}

/// Reassembly state of one virtual channel.
#[derive(Debug, Default)]
struct VcState {
    /// Bytes of the packet(s) currently being reassembled.
    pending: Vec<u8>,
    /// Whether `pending` is aligned on a packet boundary (we have seen a First Header Pointer).
    in_sync: bool,
    /// Last VC frame count seen.
    last_count: Option<u8>,
}

impl VcState {
    fn lose_sync(&mut self, stats: &FramingStats) {
        if !self.pending.is_empty() {
            bump(&stats.packets_dropped);
        }
        self.pending.clear();
        self.in_sync = false;
    }
}

/// Stateful TM Transfer Frame deframer and packet extractor.
///
/// Feed it every datagram in arrival order with [`push`](Self::push); it calls back once per
/// complete Space Packet. One instance must see the whole stream (reassembly is stateful), which
/// is why the gateway runs it in its single processing pipeline.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use chronus_gateway::config::{FramingConfig, FramingMode};
/// use chronus_gateway::frames::{FramingStats, TmDeframer, TmFrameBuilder};
/// use chronus_gateway::encode_synthetic_tm;
///
/// let mut cfg = FramingConfig::default();
/// cfg.mode = FramingMode::TmFrames;
/// cfg.frame_length = Some(64);
///
/// // A 100-byte packet does not fit one 64-byte frame: it spans two.
/// let packet = encode_synthetic_tm(0x100, 7, &[0xAB; 94]);
/// let mut builder = TmFrameBuilder::new(0x73, 64);
/// let mut frames = builder.pack(0, &[&packet]);
/// frames.extend(builder.flush(0));
///
/// let mut deframer = TmDeframer::new(&cfg, Arc::new(FramingStats::default()));
/// let mut got = Vec::new();
/// for f in &frames {
///     deframer.push(f, |p| got.push(p.bytes.to_vec()));
/// }
/// assert_eq!(got, vec![packet]);
/// ```
#[derive(Debug)]
pub struct TmDeframer {
    frame_length: Option<usize>,
    asm: bool,
    derandomize: bool,
    fecf: bool,
    spacecraft_id: Option<u16>,
    virtual_channels: Option<Vec<u8>>,
    stats: Arc<FramingStats>,
    /// Byte stream awaiting sync (ASM mode only).
    stream: Vec<u8>,
    /// Scratch copy of one frame (derandomization works in place).
    scratch: Vec<u8>,
    vcs: [VcState; 8],
    last_mc: Option<u8>,
}

impl TmDeframer {
    /// Builds a deframer from validated configuration. `stats` is shared with the metrics endpoint.
    pub fn new(cfg: &FramingConfig, stats: Arc<FramingStats>) -> Self {
        Self {
            frame_length: cfg.frame_length,
            asm: cfg.asm,
            derandomize: cfg.derandomize,
            fecf: cfg.fecf,
            spacecraft_id: cfg.spacecraft_id,
            virtual_channels: cfg.virtual_channels.clone(),
            stats,
            stream: Vec::new(),
            scratch: Vec::new(),
            vcs: Default::default(),
            last_mc: None,
        }
    }

    /// The shared counters.
    pub fn stats(&self) -> &Arc<FramingStats> {
        &self.stats
    }

    /// Processes one datagram and calls `emit` for every complete Space Packet it finishes.
    ///
    /// - **ASM mode:** the datagram is appended to a byte stream that is searched for the sync
    ///   marker, so a frame may be split across datagrams and junk between frames is skipped.
    /// - **Without ASM:** the datagram must hold a whole number of frames (exactly one when
    ///   `frame_length` is unset).
    pub fn push(&mut self, datagram: &[u8], mut emit: impl FnMut(ExtractedPacket<'_>)) {
        bump(&self.stats.datagrams);
        if self.asm {
            self.push_stream(datagram, &mut emit);
            return;
        }
        match self.frame_length {
            None => {
                self.handle_frame(datagram, &mut emit);
            }
            Some(len) if !datagram.is_empty() && datagram.len().is_multiple_of(len) => {
                for frame in datagram.chunks_exact(len) {
                    self.handle_frame(frame, &mut emit);
                }
            }
            Some(_) => bump(&self.stats.frames_invalid),
        }
    }

    fn push_stream(&mut self, datagram: &[u8], emit: &mut impl FnMut(ExtractedPacket<'_>)) {
        // Config validation guarantees a frame length in ASM mode; be defensive anyway.
        let Some(frame_len) = self.frame_length else {
            bump(&self.stats.frames_invalid);
            return;
        };
        self.stream.extend_from_slice(datagram);
        let mut pos = 0;
        let mut stream = std::mem::take(&mut self.stream);
        loop {
            let Some(rel) = find_asm(&stream[pos..]) else {
                // Keep the last three bytes: they may be the start of a marker split across
                // datagrams.
                let keep_from = stream.len().saturating_sub(ASM.len() - 1).max(pos);
                self.stats
                    .bytes_discarded
                    .fetch_add((keep_from - pos) as u64, Ordering::Relaxed);
                pos = keep_from;
                break;
            };
            self.stats
                .bytes_discarded
                .fetch_add(rel as u64, Ordering::Relaxed);
            let asm_at = pos + rel;
            let frame_start = asm_at + ASM.len();
            if stream.len() < frame_start + frame_len {
                pos = asm_at; // wait for the rest of the frame
                break;
            }
            let ok = self.handle_frame(&stream[frame_start..frame_start + frame_len], emit);
            // A bad frame may have swallowed the next marker: resume the search just past this one.
            pos = if ok {
                frame_start + frame_len
            } else {
                asm_at + 1
            };
        }
        stream.drain(..pos);
        self.stream = stream;
    }

    /// Returns `true` if the frame was well formed (even if filtered or idle).
    fn handle_frame(&mut self, raw: &[u8], emit: &mut impl FnMut(ExtractedPacket<'_>)) -> bool {
        if raw.len() < TM_FRAME_MIN_LEN
            || raw.len() > TM_FRAME_MAX_LEN
            || self.frame_length.is_some_and(|l| l != raw.len())
        {
            bump(&self.stats.frames_invalid);
            return false;
        }
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        scratch.extend_from_slice(raw);
        if self.derandomize {
            ccsds_randomize(&mut scratch);
        }
        let ok = match parse_tm_frame(&scratch, self.fecf) {
            Ok(frame) => {
                self.accept(&frame, emit);
                true
            }
            Err(FrameError::FecfMismatch { .. }) => {
                bump(&self.stats.frames_crc_error);
                false
            }
            Err(_) => {
                bump(&self.stats.frames_invalid);
                false
            }
        };
        self.scratch = scratch;
        ok
    }

    fn accept(&mut self, frame: &TmFrame<'_>, emit: &mut impl FnMut(ExtractedPacket<'_>)) {
        let h = frame.header;
        if self.spacecraft_id.is_some_and(|id| id != h.spacecraft_id) {
            bump(&self.stats.frames_other_spacecraft);
            return;
        }
        if let Some(last) = self.last_mc
            && h.mc_frame_count != last.wrapping_add(1)
        {
            bump(&self.stats.mc_frame_gaps);
        }
        self.last_mc = Some(h.mc_frame_count);
        if self
            .virtual_channels
            .as_ref()
            .is_some_and(|vcs| !vcs.contains(&h.vcid))
        {
            bump(&self.stats.frames_vc_filtered);
            return;
        }
        bump(&self.stats.frames_ok);

        let stats = Arc::clone(&self.stats);
        let vc = &mut self.vcs[usize::from(h.vcid)];
        if let Some(last) = vc.last_count
            && h.vc_frame_count != last.wrapping_add(1)
        {
            bump(&stats.vc_frame_gaps);
            vc.lose_sync(&stats);
        }
        vc.last_count = Some(h.vc_frame_count);

        let fhp = h.first_header_pointer;
        if fhp == FHP_IDLE {
            bump(&stats.frames_idle);
            return;
        }
        let data = frame.data;
        if fhp != FHP_NO_PACKET_START && usize::from(fhp) >= data.len() {
            // A pointer past the end of the data field: the frame is internally inconsistent.
            vc.lose_sync(&stats);
            return;
        }
        let emit_ctx = (h.vcid, h.spacecraft_id);

        if fhp == FHP_NO_PACKET_START {
            if vc.in_sync {
                vc.pending.extend_from_slice(data);
                drain_packets(vc, &stats, emit_ctx, emit);
            }
            // Out of sync and no header in this frame: nothing to anchor on, discard.
            return;
        }

        let fhp = usize::from(fhp);
        if vc.in_sync {
            // The bytes before the pointer finish the packet in progress...
            vc.pending.extend_from_slice(&data[..fhp]);
            drain_packets(vc, &stats, emit_ctx, emit);
            // ...exactly. Anything left over means the stream and the pointer disagree.
            if !vc.pending.is_empty() {
                vc.lose_sync(&stats);
            }
        }
        vc.pending.clear();
        vc.pending.extend_from_slice(&data[fhp..]);
        vc.in_sync = true;
        drain_packets(vc, &stats, emit_ctx, emit);
    }

    /// Drops all buffered state (stream and partial packets), keeping the counters.
    pub fn reset(&mut self) {
        self.stream.clear();
        self.vcs = Default::default();
        self.last_mc = None;
    }
}

/// Emits every complete packet at the front of `vc.pending`.
fn drain_packets(
    vc: &mut VcState,
    stats: &FramingStats,
    (vcid, spacecraft_id): (u8, u16),
    emit: &mut impl FnMut(ExtractedPacket<'_>),
) {
    while vc.pending.len() >= PACKET_HEADER_LEN {
        // Packet version must be 000; anything else means we are not on a packet boundary.
        if vc.pending[0] >> 5 != 0 {
            vc.lose_sync(stats);
            return;
        }
        let len =
            PACKET_HEADER_LEN + 1 + usize::from(u16::from_be_bytes([vc.pending[4], vc.pending[5]]));
        debug_assert!(len <= PACKET_MAX_LEN);
        if vc.pending.len() < len {
            return;
        }
        let apid = u16::from_be_bytes([vc.pending[0], vc.pending[1]]) & 0x07FF;
        if apid == IDLE_APID {
            bump(&stats.packets_idle);
        } else {
            bump(&stats.packets_extracted);
            emit(ExtractedPacket {
                vcid,
                spacecraft_id,
                bytes: &vc.pending[..len],
            });
        }
        vc.pending.drain(..len);
    }
}

fn find_asm(hay: &[u8]) -> Option<usize> {
    hay.windows(ASM.len()).position(|w| w == ASM)
}

// ---------------------------------------------------------------------------------------------
// The framer (encoder) — for tests, the replay tool and simulators
// ---------------------------------------------------------------------------------------------

/// Packs Space Packets into fixed-length TM Transfer Frames — the inverse of [`TmDeframer`].
///
/// Used by `chronus-replay --tm-frames`, the end-to-end tests and anyone building a lab stream.
/// Frames carry a FECF by default and no OCF or secondary header. Packets fill frames back to back
/// and span boundaries; [`flush`](Self::flush) pads the last frame with an **idle packet**.
#[derive(Debug, Clone)]
pub struct TmFrameBuilder {
    spacecraft_id: u16,
    frame_length: usize,
    fecf: bool,
    mc_count: u8,
    vc: [BuilderVc; 8],
}

/// Per-VC queue of packet bytes awaiting a frame.
#[derive(Debug, Clone, Default)]
struct BuilderVc {
    frame_count: u8,
    queued: std::collections::VecDeque<u8>,
    /// Absolute stream offsets (bytes ever queued on this VC) where a packet starts.
    starts: std::collections::VecDeque<usize>,
    /// Bytes already emitted in frames.
    emitted: usize,
}

impl TmFrameBuilder {
    /// A builder for `frame_length`-octet frames from spacecraft `spacecraft_id`, with FECF.
    ///
    /// `frame_length` is clamped to `TM_FRAME_MIN_LEN + 2..=TM_FRAME_MAX_LEN`.
    #[must_use]
    pub fn new(spacecraft_id: u16, frame_length: usize) -> Self {
        Self {
            spacecraft_id: spacecraft_id & 0x3FF,
            frame_length: frame_length.clamp(TM_FRAME_MIN_LEN + FECF_LEN, TM_FRAME_MAX_LEN),
            fecf: true,
            mc_count: 0,
            vc: Default::default(),
        }
    }

    /// Turns the Frame Error Control Field off (or back on).
    #[must_use]
    pub fn with_fecf(mut self, fecf: bool) -> Self {
        self.fecf = fecf;
        self
    }

    /// Bytes of packet data each frame carries.
    #[must_use]
    pub fn data_field_len(&self) -> usize {
        self.frame_length - TM_PRIMARY_HEADER_LEN - if self.fecf { FECF_LEN } else { 0 }
    }

    /// Queues `packets` on virtual channel `vcid` and returns every frame that is now full.
    /// Bytes that do not fill a frame stay queued for the next call or [`flush`](Self::flush).
    pub fn pack(&mut self, vcid: u8, packets: &[&[u8]]) -> Vec<Vec<u8>> {
        let vcid = vcid & 0b111;
        let vc = &mut self.vc[usize::from(vcid)];
        for p in packets {
            vc.starts.push_back(vc.emitted + vc.queued.len());
            vc.queued.extend(p.iter().copied());
        }
        let mut out = Vec::new();
        while self.vc[usize::from(vcid)].queued.len() >= self.data_field_len() {
            out.push(self.emit_frame(vcid));
        }
        out
    }

    /// Pads the partially filled frame on `vcid` with an idle packet and returns the frame(s)
    /// (none if nothing is queued).
    pub fn flush(&mut self, vcid: u8) -> Vec<Vec<u8>> {
        let n = self.data_field_len();
        let queued = self.vc[usize::from(vcid & 0b111)].queued.len();
        if queued == 0 {
            return Vec::new();
        }
        let room = n - queued % n;
        // An idle packet needs at least 7 bytes; with less room it spills into one more frame.
        let idle_len = if room > PACKET_HEADER_LEN {
            room
        } else {
            room + n
        };
        let idle = idle_packet(idle_len);
        self.pack(vcid, &[&idle])
    }

    /// An idle (OID) frame: FHP `0x7FE`, data field all `0x55`, on `vcid` (commonly 7).
    pub fn idle_frame(&mut self, vcid: u8) -> Vec<u8> {
        let data = vec![0x55; self.data_field_len()];
        self.frame(vcid & 0b111, FHP_IDLE, &data)
    }

    fn emit_frame(&mut self, vcid: u8) -> Vec<u8> {
        let n = self.data_field_len();
        let vc = &mut self.vc[usize::from(vcid)];
        let lo = vc.emitted;
        while vc.starts.front().is_some_and(|&s| s < lo) {
            vc.starts.pop_front();
        }
        let fhp = match vc.starts.front() {
            Some(&s) if s < lo + n => (s - lo) as u16,
            _ => FHP_NO_PACKET_START,
        };
        let data: Vec<u8> = vc.queued.drain(..n).collect();
        vc.emitted += n;
        self.frame(vcid, fhp, &data)
    }

    fn frame(&mut self, vcid: u8, fhp: u16, data: &[u8]) -> Vec<u8> {
        let vc = &mut self.vc[usize::from(vcid)];
        let header = TmFrameHeader {
            version: 0,
            spacecraft_id: self.spacecraft_id,
            vcid,
            ocf_present: false,
            mc_frame_count: self.mc_count,
            vc_frame_count: vc.frame_count,
            secondary_header: false,
            sync_flag: false,
            first_header_pointer: fhp,
        };
        self.mc_count = self.mc_count.wrapping_add(1);
        vc.frame_count = vc.frame_count.wrapping_add(1);
        let mut f = Vec::with_capacity(self.frame_length);
        f.extend_from_slice(&header.to_bytes());
        f.extend_from_slice(data);
        if self.fecf {
            let crc = crc16_ccitt(&f);
            f.extend_from_slice(&crc.to_be_bytes());
        }
        f
    }
}

/// An idle Space Packet (APID `0x7FF`) of exactly `len` bytes (`len >= 7`).
#[must_use]
pub fn idle_packet(len: usize) -> Vec<u8> {
    let len = len.max(PACKET_HEADER_LEN + 1);
    let data_len = (len - PACKET_HEADER_LEN - 1) as u16;
    let mut p = vec![0x07, 0xFF, 0xC0, 0x00];
    p.extend_from_slice(&data_len.to_be_bytes());
    p.resize(len, 0x55);
    p
}

/// Prepends the Attached Sync Marker and optionally randomizes: turns a frame into a CADU.
///
/// # Examples
///
/// ```
/// use chronus_gateway::frames::{ASM, to_cadu};
///
/// let cadu = to_cadu(&[0u8; 8], false);
/// assert_eq!(&cadu[..4], &ASM);
/// ```
#[must_use]
pub fn to_cadu(frame: &[u8], randomize: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(ASM.len() + frame.len());
    out.extend_from_slice(&ASM);
    out.extend_from_slice(frame);
    if randomize {
        ccsds_randomize(&mut out[ASM.len()..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccsds::encode_synthetic_tm;

    fn cfg(frame_length: Option<usize>) -> FramingConfig {
        FramingConfig {
            mode: FramingMode::TmFrames,
            frame_length,
            ..FramingConfig::default()
        }
    }

    fn run(d: &mut TmDeframer, datagrams: &[Vec<u8>]) -> Vec<(u8, Vec<u8>)> {
        let mut out = Vec::new();
        for dg in datagrams {
            d.push(dg, |p| out.push((p.vcid, p.bytes.to_vec())));
        }
        out
    }

    fn deframer(c: &FramingConfig) -> TmDeframer {
        TmDeframer::new(c, Arc::new(FramingStats::default()))
    }

    #[test]
    fn crc_matches_the_standard_check_value_and_detects_a_flip() {
        assert_eq!(crc16_ccitt(b"123456789"), 0x29B1);
        assert_eq!(crc16_ccitt(b""), 0xFFFF);
        assert_ne!(crc16_ccitt(b"123456788"), 0x29B1);
    }

    #[test]
    fn pn_sequence_matches_ccsds_and_has_period_255() {
        assert_eq!(
            &PN_SEQUENCE[..8],
            &[0xFF, 0x48, 0x0E, 0xC0, 0x9A, 0x0D, 0x70, 0xBC]
        );
        let mut buf = vec![0u8; 600];
        ccsds_randomize(&mut buf);
        assert_eq!(&buf[255..510], &buf[..255]);
    }

    #[test]
    fn header_round_trips() {
        let h = TmFrameHeader {
            version: 0,
            spacecraft_id: 0x3AB,
            vcid: 5,
            ocf_present: true,
            mc_frame_count: 200,
            vc_frame_count: 17,
            secondary_header: true,
            sync_flag: false,
            first_header_pointer: 0x123,
        };
        assert_eq!(TmFrameHeader::parse(&h.to_bytes()), Some(h));
        assert_eq!(TmFrameHeader::parse(&[0; 5]), None);
    }

    #[test]
    fn parse_rejects_crc_version_sync_and_short_frames() {
        let mut b = TmFrameBuilder::new(1, 32);
        let mut f = b.idle_frame(7);
        assert!(parse_tm_frame(&f, true).is_ok());
        f[10] ^= 1;
        assert!(matches!(
            parse_tm_frame(&f, true),
            Err(FrameError::FecfMismatch { .. })
        ));
        // Without FECF checking the corrupted frame parses (the CRC bytes become data).
        assert!(parse_tm_frame(&f, false).is_ok());

        let mut v = TmFrameBuilder::new(1, 32).with_fecf(false).idle_frame(0);
        v[0] |= 0x40; // version 1
        assert_eq!(parse_tm_frame(&v, false).unwrap_err(), FrameError::NotTm(1));
        v[0] &= 0x3F;
        v[4] |= 0x40; // sync flag
        assert_eq!(
            parse_tm_frame(&v, false).unwrap_err(),
            FrameError::NotPacketData
        );
        assert!(matches!(
            parse_tm_frame(&[0; 4], false),
            Err(FrameError::TooShort { .. })
        ));
    }

    #[test]
    fn parse_skips_secondary_header_and_ocf() {
        let h = TmFrameHeader {
            version: 0,
            spacecraft_id: 9,
            vcid: 2,
            ocf_present: true,
            mc_frame_count: 0,
            vc_frame_count: 0,
            secondary_header: true,
            sync_flag: false,
            first_header_pointer: 0,
        };
        let mut f = h.to_bytes().to_vec();
        f.extend_from_slice(&[0x02, 0xAA, 0xBB]); // secondary header: 3 bytes total
        f.extend_from_slice(b"DATA");
        f.extend_from_slice(&[1, 2, 3, 4]); // OCF
        let crc = crc16_ccitt(&f);
        f.extend_from_slice(&crc.to_be_bytes());
        let p = parse_tm_frame(&f, true).unwrap();
        assert_eq!(p.data, b"DATA");
        assert_eq!(p.ocf, Some([1, 2, 3, 4]));

        // A secondary header longer than the frame is rejected, not sliced.
        f[6] = 0x3F;
        let crc = crc16_ccitt(&f[..f.len() - 2]);
        let n = f.len();
        f[n - 2..].copy_from_slice(&crc.to_be_bytes());
        assert!(matches!(
            parse_tm_frame(&f, true),
            Err(FrameError::BadSecondaryHeader(64))
        ));
    }

    #[test]
    fn packets_spanning_many_frames_and_sharing_frames_are_recovered() {
        let pkts: Vec<Vec<u8>> = vec![
            encode_synthetic_tm(0x100, 1, &[1; 3]),
            encode_synthetic_tm(0x101, 2, &[2; 150]), // spans ~4 frames of 48
            encode_synthetic_tm(0x102, 3, &[3; 10]),
            encode_synthetic_tm(0x103, 4, &[4; 37]),
        ];
        let refs: Vec<&[u8]> = pkts.iter().map(Vec::as_slice).collect();
        let mut b = TmFrameBuilder::new(0x73, 48);
        let mut frames = b.pack(0, &refs);
        frames.extend(b.flush(0));
        assert!(frames.len() >= 5);
        let mut d = deframer(&cfg(Some(48)));
        let got: Vec<Vec<u8>> = run(&mut d, &frames).into_iter().map(|(_, p)| p).collect();
        assert_eq!(got, pkts);
        let s = d.stats().snapshot(FramingMode::TmFrames);
        assert_eq!(s.packets_extracted, 4);
        assert_eq!(s.packets_idle, 1);
        assert_eq!(s.frames_ok as usize, frames.len());
        assert_eq!((s.packets_dropped, s.vc_frame_gaps), (0, 0));
    }

    #[test]
    fn packet_header_split_across_frames_is_recovered() {
        // Choose sizes so a 6-byte header straddles the boundary of a 30-byte data field.
        let mut b = TmFrameBuilder::new(1, 38); // data field = 30
        let p1 = encode_synthetic_tm(0x10, 1, &[9; 21]); // 27 bytes -> next header at 27
        let p2 = encode_synthetic_tm(0x11, 2, &[8; 40]);
        let mut frames = b.pack(0, &[&p1, &p2]);
        frames.extend(b.flush(0));
        let mut d = deframer(&cfg(Some(38)));
        let got: Vec<Vec<u8>> = run(&mut d, &frames).into_iter().map(|(_, p)| p).collect();
        assert_eq!(got, vec![p1, p2]);
    }

    #[test]
    fn virtual_channels_reassemble_independently_when_interleaved() {
        let a = encode_synthetic_tm(0x200, 1, &[0xA; 70]);
        let b2 = encode_synthetic_tm(0x300, 1, &[0xB; 70]);
        let mut b = TmFrameBuilder::new(5, 40);
        let fa = b.pack(1, &[&a]);
        let fb = b.pack(2, &[&b2]);
        let mut frames = Vec::new();
        for (x, y) in fa.iter().zip(fb.iter()) {
            frames.push(x.clone());
            frames.push(y.clone());
        }
        frames.extend(b.flush(1));
        frames.extend(b.flush(2));
        let mut d = deframer(&cfg(Some(40)));
        let mut got = run(&mut d, &frames);
        got.sort();
        assert_eq!(got, vec![(1, a), (2, b2)]);
        // Each VC's own counter is continuous even though the frames were interleaved (the master
        // counter, assigned in build order, is not, and is merely counted).
        let s = d.stats().snapshot(FramingMode::TmFrames);
        assert_eq!(s.vc_frame_gaps, 0);
        assert_eq!(s.packets_dropped, 0);
    }

    #[test]
    fn a_lost_frame_drops_only_the_packet_it_cut() {
        let p1 = encode_synthetic_tm(0x20, 1, &[1; 60]);
        let p2 = encode_synthetic_tm(0x21, 2, &[2; 5]);
        let mut b = TmFrameBuilder::new(1, 40);
        let mut frames = b.pack(0, &[&p1, &p2]);
        frames.extend(b.flush(0));
        // Frame 1 holds the tail of p1 and the start of p2... drop frame 0 instead: p1's head.
        frames.remove(0);
        let mut d = deframer(&cfg(Some(40)));
        let got: Vec<Vec<u8>> = run(&mut d, &frames).into_iter().map(|(_, p)| p).collect();
        assert_eq!(got, vec![p2], "p1 is unrecoverable, p2 resyncs via the FHP");
    }

    #[test]
    fn a_counter_gap_mid_packet_abandons_it_and_is_counted() {
        let p1 = encode_synthetic_tm(0x20, 1, &[1; 100]);
        let p2 = encode_synthetic_tm(0x21, 2, &[2; 5]);
        let mut b = TmFrameBuilder::new(1, 40);
        let mut frames = b.pack(0, &[&p1]);
        frames.extend(b.pack(0, &[&p2]));
        frames.extend(b.flush(0));
        frames.remove(1); // middle of p1
        let mut d = deframer(&cfg(Some(40)));
        let got: Vec<Vec<u8>> = run(&mut d, &frames).into_iter().map(|(_, p)| p).collect();
        assert_eq!(got, vec![p2]);
        let s = d.stats().snapshot(FramingMode::TmFrames);
        assert_eq!(s.vc_frame_gaps, 1);
        assert_eq!(s.mc_frame_gaps, 1);
        assert_eq!(s.packets_dropped, 1);
    }

    #[test]
    fn asm_stream_with_randomization_split_across_datagrams_and_junk() {
        let pkts: Vec<Vec<u8>> = (0..6)
            .map(|i| encode_synthetic_tm(0x100 + i, i, &vec![i as u8; 20 + 13 * i as usize]))
            .collect();
        let refs: Vec<&[u8]> = pkts.iter().map(Vec::as_slice).collect();
        let mut b = TmFrameBuilder::new(0x2A, 64);
        let mut frames = b.pack(3, &refs);
        frames.extend(b.flush(3));
        let mut stream = vec![0x00, 0x1A, 0xCF]; // junk, including a partial marker
        for f in &frames {
            stream.extend(to_cadu(f, true));
            stream.extend_from_slice(&[0xEE; 3]); // inter-frame junk
        }
        // Chop the stream into awkward datagram sizes.
        let datagrams: Vec<Vec<u8>> = stream.chunks(37).map(<[u8]>::to_vec).collect();
        let mut c = cfg(Some(64));
        c.asm = true;
        c.derandomize = true;
        let mut d = deframer(&c);
        let got: Vec<Vec<u8>> = run(&mut d, &datagrams)
            .into_iter()
            .map(|(_, p)| p)
            .collect();
        assert_eq!(got, pkts);
        let s = d.stats().snapshot(FramingMode::TmFrames);
        assert_eq!(s.frames_crc_error, 0);
        assert!(s.bytes_discarded >= 3 * frames.len() as u64);
    }

    #[test]
    fn asm_mode_resyncs_after_a_corrupted_frame() {
        let p: Vec<Vec<u8>> = (0..3)
            .map(|i| encode_synthetic_tm(0x50, i, &[i as u8; 30]))
            .collect();
        let mut b = TmFrameBuilder::new(1, 48);
        let mut frames = Vec::new();
        for pk in &p {
            frames.extend(b.pack(0, &[pk]));
            frames.extend(b.flush(0));
        }
        let mut stream = Vec::new();
        for (i, f) in frames.iter().enumerate() {
            let mut cadu = to_cadu(f, false);
            if i == 1 {
                cadu.truncate(cadu.len() - 10); // short frame: next ASM lands inside it
            }
            stream.extend(cadu);
        }
        let mut c = cfg(Some(48));
        c.asm = true;
        let mut d = deframer(&c);
        let got: Vec<Vec<u8>> = run(&mut d, &[stream]).into_iter().map(|(_, p)| p).collect();
        assert_eq!(got.first(), Some(&p[0]));
        assert_eq!(
            got.last(),
            Some(&p[2]),
            "frame after the damaged one is recovered"
        );
        assert_eq!(
            d.stats().snapshot(FramingMode::TmFrames).frames_crc_error,
            1
        );
    }

    #[test]
    fn filters_count_other_spacecraft_and_virtual_channels() {
        let pk = encode_synthetic_tm(0x10, 1, b"x");
        let mut c = cfg(Some(32));
        c.spacecraft_id = Some(7);
        c.virtual_channels = Some(vec![0]);
        let mut d = deframer(&c);
        let mut other = TmFrameBuilder::new(8, 32);
        let mut mine = TmFrameBuilder::new(7, 32);
        let mut frames = other.pack(0, &[&pk]);
        frames.extend(other.flush(0));
        frames.extend(mine.pack(1, &[&pk]));
        frames.extend(mine.flush(1));
        frames.extend(mine.pack(0, &[&pk]));
        frames.extend(mine.flush(0));
        frames.push(mine.idle_frame(0));
        let got = run(&mut d, &frames);
        assert_eq!(got, vec![(0, pk)]);
        let s = d.stats().snapshot(FramingMode::TmFrames);
        assert_eq!(s.frames_other_spacecraft, 1);
        assert_eq!(s.frames_vc_filtered, 1);
        assert_eq!(s.frames_idle, 1);
    }

    #[test]
    fn datagram_length_must_be_a_multiple_of_the_frame_length() {
        let mut b = TmFrameBuilder::new(1, 32);
        let pk = encode_synthetic_tm(0x10, 1, b"ok");
        let mut two = b.pack(0, &[&pk]);
        two.extend(b.flush(0));
        two.push(b.idle_frame(7));
        let joined: Vec<u8> = two.concat();
        let mut d = deframer(&cfg(Some(32)));
        assert_eq!(run(&mut d, std::slice::from_ref(&joined)).len(), 1);
        assert_eq!(run(&mut d, &[joined[..40].to_vec()]).len(), 0);
        assert_eq!(d.stats().snapshot(FramingMode::TmFrames).frames_invalid, 1);
    }

    #[test]
    fn garbage_never_panics_and_memory_stays_bounded() {
        let mut c = cfg(Some(64));
        c.asm = true;
        c.derandomize = true;
        let mut d = deframer(&c);
        let mut seed: u32 = 12345;
        for _ in 0..2000 {
            let n = (seed % 300) as usize;
            let dg: Vec<u8> = (0..n)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (seed >> 16) as u8
                })
                .collect();
            d.push(&dg, |_| {});
            assert!(d.stream.len() < 64 + ASM.len() + 300);
        }
        let mut d = deframer(&cfg(None));
        for n in 0..80 {
            d.push(&vec![0u8; n], |_| {});
            d.push(&vec![0xFFu8; n], |_| {});
        }
    }

    #[test]
    fn inconsistent_first_header_pointer_drops_the_partial_packet() {
        // Frame 0 starts a 100-byte packet; frame 1 claims a new header at offset 2 (too early).
        let p = encode_synthetic_tm(0x30, 1, &[7; 94]);
        let mut b = TmFrameBuilder::new(1, 48).with_fecf(false);
        let frames = b.pack(0, &[&p]);
        let mut f1 = frames[1].clone();
        let status = 0x1800u16 | 2;
        f1[4..6].copy_from_slice(&status.to_be_bytes());
        let mut c = cfg(Some(48));
        c.fecf = false;
        let mut d = deframer(&c);
        let got = run(&mut d, &[frames[0].clone(), f1]);
        assert!(got.is_empty());
        assert_eq!(d.stats().snapshot(FramingMode::TmFrames).packets_dropped, 1);
    }

    #[test]
    fn idle_packet_has_the_requested_length() {
        for n in 7..40 {
            let p = idle_packet(n);
            assert_eq!(p.len(), n);
            assert_eq!(u16::from_be_bytes([p[0], p[1]]) & 0x7FF, IDLE_APID);
            assert_eq!(usize::from(u16::from_be_bytes([p[4], p[5]])) + 7, n);
        }
    }

    #[test]
    fn builder_frames_have_the_configured_length_and_valid_pointers() {
        let mut b = TmFrameBuilder::new(3, 50);
        let pkts: Vec<Vec<u8>> = (0..20)
            .map(|i| encode_synthetic_tm(0x40, i, &vec![0; (i as usize * 7) % 41 + 1]))
            .collect();
        for p in &pkts {
            for f in b.pack(0, &[p]) {
                assert_eq!(f.len(), 50);
                let parsed = parse_tm_frame(&f, true).unwrap();
                let fhp = parsed.header.first_header_pointer;
                assert!(fhp == FHP_NO_PACKET_START || usize::from(fhp) < parsed.data.len());
            }
        }
    }
}
