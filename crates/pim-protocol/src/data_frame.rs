use bitflags::bitflags;
use bytes::{Buf, BufMut, BytesMut};

use pim_core::{FrameCodec, NodeId, PimError};

bitflags! {
    /// Flags on a mesh data frame.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DataFlags: u8 {
        const IS_FRAGMENT      = 0b0000_0001;
        const IS_LAST_FRAGMENT = 0b0000_0010;
        const REQUIRES_ACK     = 0b0000_0100;
        const IS_ACK           = 0b0000_1000;
        const IS_INTERNET      = 0b0001_0000;
    }
}

/// Carries an IP packet (or fragment) through the mesh.
///
/// Header: src_id(16) + dst_id(16) + session_id(4) + ttl(1) + flags(1) + payload_len(2) = 40 bytes
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshDataFrame {
    pub src_id: NodeId,
    pub dst_id: NodeId,
    pub session_id: u32,
    pub ttl: u8,
    pub flags: DataFlags,
    pub payload: Vec<u8>,
}

const HEADER_SIZE: usize = 40;

impl FrameCodec for MeshDataFrame {
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_slice(self.src_id.as_bytes());
        buf.put_slice(self.dst_id.as_bytes());
        buf.put_u32(self.session_id);
        buf.put_u8(self.ttl);
        buf.put_u8(self.flags.bits());
        buf.put_u16(self.payload.len() as u16);
        buf.put_slice(&self.payload);
    }

    fn decode(buf: &mut BytesMut) -> Result<Self, PimError> {
        if buf.len() < HEADER_SIZE {
            return Err(PimError::Protocol(format!(
                "data frame too short: need {HEADER_SIZE} bytes, have {}",
                buf.len()
            )));
        }

        let mut src_bytes = [0u8; 16];
        src_bytes.copy_from_slice(&buf[0..16]);
        let src_id = NodeId::from_bytes(src_bytes);

        let mut dst_bytes = [0u8; 16];
        dst_bytes.copy_from_slice(&buf[16..32]);
        let dst_id = NodeId::from_bytes(dst_bytes);

        let session_id = (&buf[32..36]).get_u32();
        let ttl = buf[36];
        let flags = DataFlags::from_bits_truncate(buf[37]);
        let payload_len = (&buf[38..40]).get_u16() as usize;

        let total = HEADER_SIZE + payload_len;
        if buf.len() < total {
            return Err(PimError::Protocol(format!(
                "data frame truncated: need {total} bytes, have {}",
                buf.len()
            )));
        }

        let payload = buf[HEADER_SIZE..total].to_vec();
        buf.advance(total);

        Ok(MeshDataFrame {
            src_id,
            dst_id,
            session_id,
            ttl,
            flags,
            payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_frame() -> MeshDataFrame {
        MeshDataFrame {
            src_id: NodeId::from_bytes([1; 16]),
            dst_id: NodeId::from_bytes([2; 16]),
            session_id: 42,
            ttl: 10,
            flags: DataFlags::IS_INTERNET,
            payload: vec![0xCA, 0xFE],
        }
    }

    #[test]
    fn round_trip() {
        let frame = sample_frame();
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let decoded = MeshDataFrame::decode(&mut buf).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn flags_preserved() {
        let frame = MeshDataFrame {
            flags: DataFlags::IS_FRAGMENT | DataFlags::IS_LAST_FRAGMENT | DataFlags::REQUIRES_ACK,
            ..sample_frame()
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let decoded = MeshDataFrame::decode(&mut buf).unwrap();
        assert!(decoded.flags.contains(DataFlags::IS_FRAGMENT));
        assert!(decoded.flags.contains(DataFlags::IS_LAST_FRAGMENT));
        assert!(decoded.flags.contains(DataFlags::REQUIRES_ACK));
        assert!(!decoded.flags.contains(DataFlags::IS_INTERNET));
    }

    #[test]
    fn reject_truncated_header() {
        let mut buf = BytesMut::from(&[0u8; 20][..]);
        assert!(MeshDataFrame::decode(&mut buf).is_err());
    }

    #[test]
    fn reject_truncated_payload() {
        let frame = sample_frame();
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        buf.truncate(buf.len() - 1); // chop 1 byte of payload
        assert!(MeshDataFrame::decode(&mut buf).is_err());
    }

    #[test]
    fn empty_payload() {
        let frame = MeshDataFrame {
            payload: vec![],
            ..sample_frame()
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let decoded = MeshDataFrame::decode(&mut buf).unwrap();
        assert!(decoded.payload.is_empty());
    }
}
