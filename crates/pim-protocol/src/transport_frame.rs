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

        buf.advance(HEADER_SIZE);
        let payload = buf.split_to(length as usize).freeze();

        let mut tag = [0u8; 16];
        tag.copy_from_slice(&buf[0..TAG_SIZE]);

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
mod tests;
