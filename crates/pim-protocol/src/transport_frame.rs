//! Outer authenticated transport envelope exchanged on direct peer links.

use bytes::{Buf, BufMut, BytesMut};

use pim_core::{FrameCodec, PimError};

use crate::frame_type::FrameType;

/// Magic bytes identifying a PIM transport frame: "PM" (0x504D).
pub const MAGIC: u16 = 0x504D;

/// Current protocol version.
pub const VERSION: u8 = 1;

/// Maximum payload size (1 MB).
pub const MAX_PAYLOAD_SIZE: u32 = 1_048_576;

/// The outermost frame on the wire between directly connected peers.
///
/// Layout:
/// - magic: u16 (0x504D)
/// - version: u8
/// - frame_type: u8
/// - length: u32 (payload length)
/// - nonce: [u8; 12]
/// - payload: [u8; length] (encrypted)
/// - tag: [u8; 16]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportFrame {
    /// Payload discriminator for `payload`.
    pub frame_type: FrameType,
    /// AES-GCM nonce used for the encrypted payload.
    pub nonce: [u8; 12],
    /// Encrypted inner frame bytes.
    pub payload: bytes::Bytes,
    /// Authentication tag for the encrypted payload.
    pub tag: [u8; 16],
}

/// Header size: magic(2) + version(1) + frame_type(1) + length(4) + nonce(12) = 20
const HEADER_SIZE: usize = 20;
/// Tag size: 16
const TAG_SIZE: usize = 16;

impl FrameCodec for TransportFrame {
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_u16(MAGIC);
        buf.put_u8(VERSION);
        buf.put_u8(self.frame_type as u8);
        buf.put_u32(self.payload.len() as u32);
        buf.put_slice(&self.nonce);
        buf.put_slice(&self.payload);
        buf.put_slice(&self.tag);
    }

    fn decode(buf: &mut BytesMut) -> Result<Self, PimError> {
        if buf.len() < HEADER_SIZE {
            return Err(PimError::Protocol("frame too short for header".into()));
        }

        let magic = (&buf[0..2]).get_u16();
        if magic != MAGIC {
            return Err(PimError::Protocol(format!(
                "invalid magic: 0x{magic:04X}, expected 0x{MAGIC:04X}"
            )));
        }

        let version = buf[2];
        if version != VERSION {
            return Err(PimError::Protocol(format!(
                "unsupported version: {version}, expected {VERSION}"
            )));
        }

        let frame_type = FrameType::from_u8(buf[3])?;
        let length = (&buf[4..8]).get_u32();

        if length > MAX_PAYLOAD_SIZE {
            return Err(PimError::Protocol(format!(
                "payload too large: {length} bytes, max {MAX_PAYLOAD_SIZE}"
            )));
        }

        let total_size = HEADER_SIZE + length as usize + TAG_SIZE;
        if buf.len() < total_size {
            return Err(PimError::Protocol(format!(
                "frame truncated: need {total_size} bytes, have {}",
                buf.len()
            )));
        }

        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&buf[8..20]);

        buf.advance(20);
        let payload = buf.split_to(length as usize).freeze();

        let mut tag = [0u8; 16];
        tag.copy_from_slice(&buf[..TAG_SIZE]);
        buf.advance(TAG_SIZE);


        Ok(TransportFrame {
            frame_type,
            nonce,
            payload,
            tag,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_frame() -> TransportFrame {
        TransportFrame {
            frame_type: FrameType::Data,
            nonce: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
            payload: bytes::Bytes::from(vec![0xDE, 0xAD, 0xBE, 0xEF]),
            tag: [0xAA; 16],
        }
    }

    #[test]
    fn round_trip() {
        let frame = sample_frame();
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);

        let decoded = TransportFrame::decode(&mut buf).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn exact_byte_layout() {
        let frame = sample_frame();
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);

        // magic
        assert_eq!(buf[0], 0x50);
        assert_eq!(buf[1], 0x4D);
        // version
        assert_eq!(buf[2], 1);
        // frame_type = Data = 0x02
        assert_eq!(buf[3], 0x02);
        // length = 4 (big-endian)
        assert_eq!(&buf[4..8], &[0, 0, 0, 4]);
        // nonce
        assert_eq!(&buf[8..20], &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        // payload
        assert_eq!(&buf[20..24], &[0xDE, 0xAD, 0xBE, 0xEF]);
        // tag
        assert_eq!(&buf[24..40], &[0xAA; 16]);
    }

    #[test]
    fn reject_truncated() {
        let mut buf = BytesMut::from(&[0x50, 0x4D, 0x01][..]);
        assert!(TransportFrame::decode(&mut buf).is_err());
    }

    #[test]
    fn reject_invalid_magic() {
        let frame = sample_frame();
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        buf[0] = 0xFF;

        assert!(TransportFrame::decode(&mut buf)
            .unwrap_err()
            .to_string()
            .contains("invalid magic"));
    }

    #[test]
    fn reject_invalid_version() {
        let frame = sample_frame();
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        buf[2] = 99;

        assert!(TransportFrame::decode(&mut buf)
            .unwrap_err()
            .to_string()
            .contains("unsupported version"));
    }

    #[test]
    fn reject_oversized_payload() {
        let mut buf = BytesMut::new();
        buf.put_u16(MAGIC);
        buf.put_u8(VERSION);
        buf.put_u8(0x02); // Data
        buf.put_u32(MAX_PAYLOAD_SIZE + 1);
        buf.put_slice(&[0u8; 12]); // nonce

        assert!(TransportFrame::decode(&mut buf)
            .unwrap_err()
            .to_string()
            .contains("payload too large"));
    }

    #[test]
    fn empty_payload_round_trip() {
        let frame = TransportFrame {
            frame_type: FrameType::Heartbeat,
            nonce: [0; 12],
            payload: bytes::Bytes::from(vec![]),
            tag: [0; 16],
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let decoded = TransportFrame::decode(&mut buf).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn random_bytes_dont_panic() {
        for seed in 0..100u8 {
            let random: Vec<u8> = (0..64)
                .map(|i| seed.wrapping_add(i).wrapping_mul(37))
                .collect();
            let mut buf = BytesMut::from(random.as_slice());
            // Should return Err, never panic
            let _ = TransportFrame::decode(&mut buf);
        }
    }
}
