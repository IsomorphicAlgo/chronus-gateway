//! Synthetic HIL telemetry payload layout **CV-3** (`chronus.hil.tm.v1`).
//!
//! Versioned, fixed-width binary layout in the CCSDS Space Packet **data field** (no PUS secondary
//! header in v1). Used by `chronus-hil-sim` and tests so subsystem co-validation (**CV-4**, **CV-5**) does not
//! depend on ambiguous raw blobs. See **`docs/EXTENDED_COVALIDATION_PLAN.md`** and `docs/HIL.md`.
//!
//! ## Layout (`chronus.hil.tm.v1`)
//!
//! All multi-byte integers and IEEE-754 scalars are **big-endian**.
//!
//! | Offset | Size | Field |
//! |--------|------|--------|
//! | 0 | 4 | Magic ASCII **`CHI1`** |
//! | 4 | 1 | Schema version **`1`** |
//! | 5 | 3 | Reserved (must be **`0`** in v1) |
//! | 8 | 4 | `seq` — monotonic frame index (`u32`) |
//! | 12 | 4 | `eps_bus_voltage_v` (`f32`) |
//! | 16 | 4 | `thermal_panel_c` (`f32`) |
//! | 20 | 4 | `body_rate_deg_s` (`f32`) |
//!
//! **Total:** [`HIL_TM_V1_PAYLOAD_LEN`] = **24** bytes. Decoders must reject shorter slices and
//! oversize is ignored at the CCSDS layer (only the declared data field is visible).
//!
//! ## APID policy
//!
//! Synthetic HIL frames are expected on APIDs in [`crate::config::StationConfig`]'s inclusive
//! `hil_tm_v1_apid_min` … `hil_tm_v1_apid_max` (defaults **0x7B0…0x7BF**). This module does not
//! inspect APIDs; callers use [`crate::config::StationConfig::apid_allows_hil_tm_v1`].

/// ASCII magic for **chronus.hil.tm.v1** ("**Ch**ronus **HI**L **1**").
pub const HIL_TM_V1_MAGIC: &[u8; 4] = b"CHI1";
/// Schema version byte for v1.
pub const HIL_TM_V1_SCHEMA_VERSION: u8 = 1;
/// Fixed packet data field length for v1 (bytes).
pub const HIL_TM_V1_PAYLOAD_LEN: usize = 24;

/// Recommended default inclusive APID lower bound for HIL v1 (CV-3).
pub const DEFAULT_HIL_TM_V1_APID_MIN: u16 = 0x7B0;
/// Recommended default inclusive APID upper bound for HIL v1 (CV-3).
pub const DEFAULT_HIL_TM_V1_APID_MAX: u16 = 0x7BF;
/// Maximum CCSDS 11-bit APID.
pub const CCSDS_APID_MAX: u16 = 0x07FF;

/// Decoded **chronus.hil.tm.v1** user fields (stack-only [`Copy`] view).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecodedHilTmV1 {
    /// Monotonic frame index from the simulator.
    pub seq: u32,
    /// Abstract EPS bus voltage [V].
    pub eps_bus_voltage_v: f32,
    /// Abstract panel temperature [°C].
    pub thermal_panel_c: f32,
    /// Abstract body rate [deg/s].
    pub body_rate_deg_s: f32,
}

/// Recoverable decode failure (never panics on arbitrary bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HilTmV1DecodeError {
    /// Slice shorter than [`HIL_TM_V1_PAYLOAD_LEN`].
    TooShort {
        /// Bytes available.
        have: usize,
    },
    /// Magic is not `CHI1`.
    BadMagic {
        /// Bytes at offset 0..4.
        got: [u8; 4],
    },
    /// Version byte is not [`HIL_TM_V1_SCHEMA_VERSION`].
    WrongVersion {
        /// Value at offset 4.
        got: u8,
    },
    /// Reserved bytes 5..8 are not all zero in v1.
    NonZeroReserved,
}

/// Encodes one v1 payload into a fixed stack buffer (**no heap allocation**).
#[must_use]
pub fn encode_hil_tm_v1_payload(
    seq: u32,
    eps_bus_voltage_v: f32,
    thermal_panel_c: f32,
    body_rate_deg_s: f32,
) -> [u8; HIL_TM_V1_PAYLOAD_LEN] {
    let mut out = [0u8; HIL_TM_V1_PAYLOAD_LEN];
    out[0..4].copy_from_slice(HIL_TM_V1_MAGIC);
    out[4] = HIL_TM_V1_SCHEMA_VERSION;
    // bytes 5..8 reserved, already zero
    out[8..12].copy_from_slice(&seq.to_be_bytes());
    out[12..16].copy_from_slice(&eps_bus_voltage_v.to_bits().to_be_bytes());
    out[16..20].copy_from_slice(&thermal_panel_c.to_bits().to_be_bytes());
    out[20..24].copy_from_slice(&body_rate_deg_s.to_bits().to_be_bytes());
    out
}

/// Decodes **chronus.hil.tm.v1** from a packet data field slice. **No allocation.**
///
/// Returns [`Err`] if `payload.len()` < [`HIL_TM_V1_PAYLOAD_LEN`] or header checks fail. Extra
/// trailing bytes are ignored by the caller (this function only reads the first 24 bytes when
/// `payload` is longer).
pub fn decode_hil_tm_v1(payload: &[u8]) -> Result<DecodedHilTmV1, HilTmV1DecodeError> {
    if payload.len() < HIL_TM_V1_PAYLOAD_LEN {
        return Err(HilTmV1DecodeError::TooShort {
            have: payload.len(),
        });
    }
    let p = payload;
    let mut magic = [0u8; 4];
    magic.copy_from_slice(&p[0..4]);
    if &magic != HIL_TM_V1_MAGIC {
        return Err(HilTmV1DecodeError::BadMagic { got: magic });
    }
    if p[4] != HIL_TM_V1_SCHEMA_VERSION {
        return Err(HilTmV1DecodeError::WrongVersion { got: p[4] });
    }
    if p[5] != 0 || p[6] != 0 || p[7] != 0 {
        return Err(HilTmV1DecodeError::NonZeroReserved);
    }
    let seq = u32::from_be_bytes([p[8], p[9], p[10], p[11]]);
    let eps_bus_voltage_v = f32::from_bits(u32::from_be_bytes([p[12], p[13], p[14], p[15]]));
    let thermal_panel_c = f32::from_bits(u32::from_be_bytes([p[16], p[17], p[18], p[19]]));
    let body_rate_deg_s = f32::from_bits(u32::from_be_bytes([p[20], p[21], p[22], p[23]]));
    Ok(DecodedHilTmV1 {
        seq,
        eps_bus_voltage_v,
        thermal_panel_c,
        body_rate_deg_s,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_encode_then_decode_round_trip() {
        let buf = encode_hil_tm_v1_payload(42, 28.5, 22.25, -0.001);
        let d = decode_hil_tm_v1(&buf).expect("decode");
        assert_eq!(d.seq, 42);
        assert!((d.eps_bus_voltage_v - 28.5).abs() < 1e-5);
        assert!((d.thermal_panel_c - 22.25).abs() < 1e-5);
        assert!((d.body_rate_deg_s - (-0.001)).abs() < 1e-6);
    }

    #[test]
    fn truncated_payload_rejected() {
        let buf = encode_hil_tm_v1_payload(0, 1.0, 2.0, 3.0);
        let err = decode_hil_tm_v1(&buf[..10]).unwrap_err();
        assert_eq!(err, HilTmV1DecodeError::TooShort { have: 10 });
    }

    #[test]
    fn empty_payload_rejected() {
        assert_eq!(
            decode_hil_tm_v1(&[]).unwrap_err(),
            HilTmV1DecodeError::TooShort { have: 0 }
        );
    }

    #[test]
    fn wrong_magic_rejected() {
        let mut buf = encode_hil_tm_v1_payload(1, 1.0, 1.0, 1.0);
        buf[0] = b'X';
        assert!(matches!(
            decode_hil_tm_v1(&buf),
            Err(HilTmV1DecodeError::BadMagic { .. })
        ));
    }

    #[test]
    fn wrong_version_rejected() {
        let mut buf = encode_hil_tm_v1_payload(1, 1.0, 1.0, 1.0);
        buf[4] = 2;
        assert_eq!(
            decode_hil_tm_v1(&buf).unwrap_err(),
            HilTmV1DecodeError::WrongVersion { got: 2 }
        );
    }

    #[test]
    fn non_zero_reserved_rejected() {
        let mut buf = encode_hil_tm_v1_payload(1, 1.0, 1.0, 1.0);
        buf[5] = 1;
        assert_eq!(
            decode_hil_tm_v1(&buf).unwrap_err(),
            HilTmV1DecodeError::NonZeroReserved
        );
    }

    #[test]
    fn longer_slice_decodes_first_24_only() {
        let mut v = encode_hil_tm_v1_payload(9, 3.0, 4.0, 5.0).to_vec();
        v.push(0xFF);
        v.push(0xEE);
        let d = decode_hil_tm_v1(&v).expect("uses prefix");
        assert_eq!(d.seq, 9);
    }
}
