use bytes::{Buf, BufMut, BytesMut};

use pim_core::{FrameCodec, NodeId, PimError};

/// Keepalive message between direct peers.
///
/// Layout:
///   sender_id(16) + timestamp(8) + gateway_hops(1) + load(1) + gw_x25519_pub(32) = 58 bytes
///
/// `gw_x25519_pub` is the X25519 public key of the nearest gateway (derived from
/// its Ed25519 seed).  Gateway nodes put their own key here; non-gateway nodes put
/// their nearest known gateway's key, or all-zero if unknown.  This lets any node
/// in the mesh learn the gateway's X25519 key for E2E encryption without extra
/// message types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatFrame {
    pub sender_id: NodeId,
    pub timestamp: u64,
    /// Hops to nearest known gateway. 0xFF = no gateway known.
    pub gateway_hops: u8,
    /// Current forwarding load (0-255).
    pub load: u8,
    /// X25519 public key of the nearest/own gateway; all-zero if unknown.
    pub gw_x25519_pub: [u8; 32],
}

const SIZE: usize = 58;

impl FrameCodec for HeartbeatFrame {
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_slice(self.sender_id.as_bytes());
        buf.put_u64(self.timestamp);
        buf.put_u8(self.gateway_hops);
        buf.put_u8(self.load);
        buf.put_slice(&self.gw_x25519_pub);
    }

    fn decode(buf: &mut BytesMut) -> Result<Self, PimError> {
        if buf.len() < SIZE {
            return Err(PimError::Protocol(format!(
                "heartbeat frame too short: need {SIZE}, have {}",
                buf.len()
            )));
        }

        let mut id_bytes = [0u8; 16];
        id_bytes.copy_from_slice(&buf[0..16]);
        let sender_id = NodeId::from_bytes(id_bytes);

        let timestamp = (&buf[16..24]).get_u64();
        let gateway_hops = buf[24];
        let load = buf[25];

        let mut gw_x25519_pub = [0u8; 32];
        gw_x25519_pub.copy_from_slice(&buf[26..58]);

        buf.advance(SIZE);

        Ok(HeartbeatFrame {
            sender_id,
            timestamp,
            gateway_hops,
            load,
            gw_x25519_pub,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let frame = HeartbeatFrame {
            sender_id: NodeId::from_bytes([0xAB; 16]),
            timestamp: 1711408200000,
            gateway_hops: 2,
            load: 128,
            gw_x25519_pub: [0xCC; 32],
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        assert_eq!(buf.len(), SIZE);
        let decoded = HeartbeatFrame::decode(&mut buf).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn no_gateway_known() {
        let frame = HeartbeatFrame {
            sender_id: NodeId::from_bytes([1; 16]),
            timestamp: 0,
            gateway_hops: 0xFF,
            load: 0,
            gw_x25519_pub: [0u8; 32],
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let decoded = HeartbeatFrame::decode(&mut buf).unwrap();
        assert_eq!(decoded.gateway_hops, 0xFF);
    }

    #[test]
    fn reject_truncated() {
        let mut buf = BytesMut::from(&[0u8; 10][..]);
        assert!(HeartbeatFrame::decode(&mut buf).is_err());
    }
}
