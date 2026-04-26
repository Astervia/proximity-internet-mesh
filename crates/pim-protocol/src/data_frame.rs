//! Data-plane frame that carries mesh-routed IP payloads.

use bitflags::bitflags;
use bytes::{Buf, BufMut, BytesMut};

use pim_core::{FrameCodec, NodeId, PimError};

bitflags! {
    /// Flags on a mesh data frame.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DataFlags: u8 {
        /// Payload contains a [`FragmentFrame`](crate::FragmentFrame) body.
        const IS_FRAGMENT      = 0b0000_0001;
        /// Fragment reaches the end of the original packet.
        const IS_LAST_FRAGMENT = 0b0000_0010;
        /// Receiver should acknowledge delivery.
        const REQUIRES_ACK     = 0b0000_0100;
        /// Payload is an acknowledgement rather than ordinary data.
        const IS_ACK           = 0b0000_1000;
        /// Payload is destined for internet egress via a gateway.
        const IS_INTERNET      = 0b0001_0000;
        /// Payload is ECIES-encrypted (E2E) to the final destination gateway.
        /// Relay nodes must forward as-is; only the gateway decrypts.
        const IS_E2E           = 0b0010_0000;
        /// Payload carries a mesh-routed control-plane message.
        const IS_CONTROL       = 0b0100_0000;
    }
}

/// Carries an IP packet (or fragment) through the mesh.
///
/// Header: src_id(16) + dst_id(16) + session_id(4) + ttl(1) + flags(1) + payload_len(2) = 40 bytes
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshDataFrame {
    /// Original source node for the payload.
    pub src_id: NodeId,
    /// Final destination node for the payload.
    pub dst_id: NodeId,
    /// Session identifier chosen by the sender.
    pub session_id: u32,
    /// Remaining hop budget.
    pub ttl: u8,
    /// Delivery and fragmentation flags.
    pub flags: DataFlags,
    /// Encapsulated IP packet bytes or fragment payload.
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
mod tests;
