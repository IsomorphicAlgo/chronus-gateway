//! CCSDS Space Packet framing & parsing (Milestone 2).
//!
//! Turns a raw [`RawFrame`] datagram into a validated [`TelemetryFrame`] by parsing the
//! CCSDS 133.0-B-2 Space Packet **primary header** (via the `spacepackets` crate) and bounds-
//! checking the declared packet data field against the bytes actually received.
//!
//! Parsing is kept behind this module boundary so the rest of the gateway depends on
//! [`TelemetryFrame`], not on `spacepackets` directly (see `Methodology.md` D-010). The payload
//! is exposed zero-copy: [`TelemetryFrame`] retains the original reference-counted datagram and
//! [`TelemetryFrame::payload`] returns a borrowed slice into it.

use std::net::SocketAddr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use spacepackets::{ByteConversionError, CcsdsPacket, PacketType, SpacePacketHeader};

use crate::ingest::RawFrame;

/// Length of the CCSDS Space Packet primary header, in bytes.
pub const CCSDS_PRIMARY_HEADER_LEN: usize = 6;

/// Errors produced while parsing a datagram into a telemetry frame.
///
/// All variants are recoverable: the caller drops the offending datagram and continues. No input
/// can cause a panic or an unbounded allocation.
#[derive(Debug, thiserror::Error)]
pub enum CcsdsError {
    /// The datagram is shorter than a CCSDS primary header.
    #[error("datagram too short for a CCSDS primary header: {len} byte(s) < {CCSDS_PRIMARY_HEADER_LEN}")]
    TooShort {
        /// Number of bytes actually present.
        len: usize,
    },

    /// The primary header could not be decoded.
    #[error("malformed CCSDS primary header: {0}")]
    Header(#[from] ByteConversionError),

    /// The header declares more data than the datagram contains.
    #[error("packet data field truncated: header declares {declared} byte(s) but datagram has {available}")]
    Truncated {
        /// Total packet length declared by the header (`6 + data_len + 1`).
        declared: usize,
        /// Bytes actually available in the datagram.
        available: usize,
    },

    /// A telecommand (TC) packet arrived on the telemetry ingestion path.
    #[error("expected telemetry (TM) but received telecommand (TC) for apid {apid}")]
    NotTelemetry {
        /// APID of the rejected packet.
        apid: u16,
    },
}

/// A validated CCSDS telemetry packet.
///
/// Field accessors expose the decoded primary-header values; [`TelemetryFrame::payload`] returns
/// the packet data field as a zero-copy borrow of the original datagram.
#[derive(Debug, Clone)]
pub struct TelemetryFrame {
    /// The full original datagram (header + data field), reference-counted.
    pub(crate) raw: Arc<[u8]>,
    /// Length of the packet data field (`data_len + 1`), in bytes.
    pub(crate) payload_len: usize,
    /// Application Process ID (11-bit).
    pub apid: u16,
    /// Packet sequence count (14-bit).
    pub seq_count: u16,
    /// Whether the packet declares a secondary header.
    pub has_secondary_header: bool,
    /// Capture timestamp, propagated from the raw frame.
    pub received_at: DateTime<Utc>,
    /// Source address, propagated from the raw frame.
    pub source: SocketAddr,
    /// Bitwise anomaly flags set by the Physics-Telemetry Co-Validation engine (Milestone 4).
    /// `0` means no anomaly detected.
    pub physics_flags: u8,
}

impl TelemetryFrame {
    /// The packet data field (secondary header + user data), borrowed zero-copy.
    pub fn payload(&self) -> &[u8] {
        &self.raw[CCSDS_PRIMARY_HEADER_LEN..CCSDS_PRIMARY_HEADER_LEN + self.payload_len]
    }

    /// Length of the packet data field in bytes.
    pub fn payload_len(&self) -> usize {
        self.payload_len
    }
}

/// Parses a raw datagram into a validated telemetry frame.
///
/// Validation order: header length → header decode → declared-vs-available length → packet type.
/// Bytes beyond the declared packet length (if any) are ignored.
pub fn parse_telemetry(frame: &RawFrame) -> Result<TelemetryFrame, CcsdsError> {
    let raw = frame.bytes.as_ref();
    if raw.len() < CCSDS_PRIMARY_HEADER_LEN {
        return Err(CcsdsError::TooShort { len: raw.len() });
    }

    // Decodes only the 6-byte primary header; never reads past it.
    let (header, _rest) = SpacePacketHeader::from_be_bytes(raw)?;

    // CCSDS: the data-length field is (octets in data field) - 1, so the field is data_len + 1.
    let payload_len = header.data_len() as usize + 1;
    let declared = CCSDS_PRIMARY_HEADER_LEN + payload_len;
    if raw.len() < declared {
        return Err(CcsdsError::Truncated { declared, available: raw.len() });
    }

    if header.packet_type() != PacketType::Tm {
        return Err(CcsdsError::NotTelemetry { apid: header.apid().value() });
    }

    Ok(TelemetryFrame {
        raw: Arc::clone(&frame.bytes),
        payload_len,
        apid: header.apid().value(),
        seq_count: header.seq_count().value(),
        has_secondary_header: header.sec_header_flag(),
        received_at: frame.received_at,
        source: frame.source,
        physics_flags: 0,
    })
}

/// Encodes a **synthetic** CCSDS telemetry Space Packet (primary header + packet data field).
///
/// For lab/HIL tools (Milestone 7): `apid` / `seq_count` are arbitrary test identifiers;
/// `payload` must be non-empty per CCSDS. Not a full PUS/secondary-header encoder — see
/// `AGENTS.md` (synthetic data only).
pub fn encode_synthetic_tm(apid: u16, seq_count: u16, payload: &[u8]) -> Vec<u8> {
    assert!(!payload.is_empty(), "CCSDS data field must be at least 1 byte");
    let version = 0u16;
    let ptype = 0u16; // TM
    let sec_hdr = 0u16;
    let word1 = (version << 13) | (ptype << 12) | (sec_hdr << 11) | (apid & 0x07FF);
    let seq_flags = 0b11u16; // unsegmented
    let word2 = (seq_flags << 14) | (seq_count & 0x3FFF);
    let data_len = (payload.len() - 1) as u16;

    let mut v = Vec::with_capacity(CCSDS_PRIMARY_HEADER_LEN + payload.len());
    v.extend_from_slice(&word1.to_be_bytes());
    v.extend_from_slice(&word2.to_be_bytes());
    v.extend_from_slice(&data_len.to_be_bytes());
    v.extend_from_slice(payload);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn frame_from(bytes: Vec<u8>) -> RawFrame {
        RawFrame {
            bytes: Arc::from(bytes.as_slice()),
            received_at: Utc::now(),
            source: SocketAddr::from(([127, 0, 0, 1], 5000)),
        }
    }

    /// Builds a telecommand packet (type bit 1) for routing tests.
    fn build_tc_packet(apid: u16, seq_count: u16, payload: &[u8]) -> Vec<u8> {
        assert!(!payload.is_empty(), "CCSDS data field must be >= 1 byte");
        let version = 0u16;
        let ptype = 1u16; // TC
        let sec_hdr = 0u16;
        let word1 = (version << 13) | (ptype << 12) | (sec_hdr << 11) | (apid & 0x07FF);
        let seq_flags = 0b11u16;
        let word2 = (seq_flags << 14) | (seq_count & 0x3FFF);
        let data_len = (payload.len() - 1) as u16;
        let mut v = Vec::with_capacity(CCSDS_PRIMARY_HEADER_LEN + payload.len());
        v.extend_from_slice(&word1.to_be_bytes());
        v.extend_from_slice(&word2.to_be_bytes());
        v.extend_from_slice(&data_len.to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn parses_valid_tm_packet() {
        let raw = encode_synthetic_tm(0x2A, 7, b"hello");
        let tm = parse_telemetry(&frame_from(raw)).expect("valid TM parses");

        assert_eq!(tm.apid, 0x2A);
        assert_eq!(tm.seq_count, 7);
        assert!(!tm.has_secondary_header);
        assert_eq!(tm.payload(), b"hello");
        assert_eq!(tm.payload_len(), 5);
        assert_eq!(tm.physics_flags, 0);
    }

    #[test]
    fn parses_known_golden_bytes() {
        // Hand-written canonical bytes: TM, apid 0x02A, seq 7, unsegmented, 5-byte payload "hello".
        let golden = vec![
            0x00, 0x2A, // version/type/sec-hdr/apid
            0xC0, 0x07, // seq flags (unsegmented) + seq count 7
            0x00, 0x04, // data length = payload_len - 1 = 4
            b'h', b'e', b'l', b'l', b'o',
        ];
        let tm = parse_telemetry(&frame_from(golden)).expect("golden TM parses");
        assert_eq!((tm.apid, tm.seq_count), (0x2A, 7));
        assert_eq!(tm.payload(), b"hello");
    }

    #[test]
    fn round_trip_preserves_fields() {
        for (apid, seq, payload) in [(0u16, 0u16, &b"a"[..]), (0x7FF, 0x3FFF, &b"telemetry"[..])] {
            let raw = encode_synthetic_tm(apid, seq, payload);
            let tm = parse_telemetry(&frame_from(raw)).expect("parses");
            assert_eq!(tm.apid, apid);
            assert_eq!(tm.seq_count, seq);
            assert_eq!(tm.payload(), payload);
        }
    }

    #[test]
    fn telecommand_is_rejected() {
        let raw = build_tc_packet(0x10, 1, b"cmd");
        match parse_telemetry(&frame_from(raw)) {
            Err(CcsdsError::NotTelemetry { apid }) => assert_eq!(apid, 0x10),
            other => panic!("expected NotTelemetry, got {other:?}"),
        }
    }

    #[test]
    fn short_datagram_is_rejected() {
        for len in 0..CCSDS_PRIMARY_HEADER_LEN {
            let raw = vec![0u8; len];
            match parse_telemetry(&frame_from(raw)) {
                Err(CcsdsError::TooShort { len: reported }) => assert_eq!(reported, len),
                other => panic!("len {len}: expected TooShort, got {other:?}"),
            }
        }
    }

    #[test]
    fn truncated_payload_is_rejected() {
        // Header claims a 5-byte data field but only 2 bytes follow.
        let mut raw = encode_synthetic_tm(0x05, 0, b"hello"); // data_len encodes 5-byte field
        raw.truncate(CCSDS_PRIMARY_HEADER_LEN + 2);
        match parse_telemetry(&frame_from(raw)) {
            Err(CcsdsError::Truncated { declared, available }) => {
                assert_eq!(declared, CCSDS_PRIMARY_HEADER_LEN + 5);
                assert_eq!(available, CCSDS_PRIMARY_HEADER_LEN + 2);
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    #[test]
    fn garbage_does_not_panic() {
        // All-0xFF header declares a 65536-byte field that isn't present -> Truncated, never a panic.
        let raw = vec![0xFFu8; CCSDS_PRIMARY_HEADER_LEN];
        assert!(matches!(
            parse_telemetry(&frame_from(raw)),
            Err(CcsdsError::Truncated { .. })
        ));
    }

    proptest! {
        #[test]
        fn parse_random_bytes_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
            let frame = RawFrame {
                bytes: Arc::from(bytes.into_boxed_slice()),
                received_at: Utc::now(),
                source: SocketAddr::from(([127, 0, 0, 1], 5000)),
            };
            let _ = parse_telemetry(&frame);
        }
    }
}
