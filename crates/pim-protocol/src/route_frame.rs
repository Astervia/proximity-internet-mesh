//! Route advertisement frame definitions.

use bytes::{Buf, BufMut, BytesMut};

use pim_core::{FrameCodec, NodeId, PimError};

/// A single route entry in a route advertisement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteEntry {
    /// Destination node this route describes.
    pub destination: NodeId,
    /// Hop count to reach `destination`.
    pub hops: u8,
    /// bit 0: is_gateway
    pub flags: u8,
    /// Mesh IPv4 address assigned to `destination`.
    pub mesh_ip: [u8; 4],
}

impl RouteEntry {
    /// Return `true` if this route advertises gateway capability.
    pub fn is_gateway(&self) -> bool {
        self.flags & 0x01 != 0
    }
}

/// Route advertisement broadcast to direct peers.
///
/// Layout: origin_id(16) + sequence(8) + entry_count(2) + entries(N * 22) + signature(64)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteUpdateFrame {
    /// Node originating this advertisement.
    pub origin_id: NodeId,
    /// Monotonic sequence number used for replay protection.
    pub sequence: u64,
    /// Reachability entries being advertised.
    pub entries: Vec<RouteEntry>,
    /// Ed25519 signature over the frame contents.
    pub signature: [u8; 64],
}

const HEADER_SIZE: usize = 16 + 8 + 2; // 26
const ENTRY_SIZE: usize = 16 + 1 + 1 + 4; // 22
const SIGNATURE_SIZE: usize = 64;
const MAX_ENTRIES: u16 = 1000;

impl FrameCodec for RouteUpdateFrame {
    fn encode(&self, buf: &mut BytesMut) {
        buf.put_slice(self.origin_id.as_bytes());
        buf.put_u64(self.sequence);
        buf.put_u16(self.entries.len() as u16);
        for entry in &self.entries {
            buf.put_slice(entry.destination.as_bytes());
            buf.put_u8(entry.hops);
            buf.put_u8(entry.flags);
            buf.put_slice(&entry.mesh_ip);
        }
        buf.put_slice(&self.signature);
    }

    fn decode(buf: &mut BytesMut) -> Result<Self, PimError> {
        if buf.len() < HEADER_SIZE {
            return Err(PimError::Protocol(
                "route update too short for header".into(),
            ));
        }

        let mut origin_bytes = [0u8; 16];
        origin_bytes.copy_from_slice(&buf[0..16]);
        let origin_id = NodeId::from_bytes(origin_bytes);

        let sequence = (&buf[16..24]).get_u64();
        let entry_count = (&buf[24..26]).get_u16();

        if entry_count > MAX_ENTRIES {
            return Err(PimError::Protocol(format!(
                "too many route entries: {entry_count}, max {MAX_ENTRIES}"
            )));
        }

        let total = HEADER_SIZE + (entry_count as usize * ENTRY_SIZE) + SIGNATURE_SIZE;
        if buf.len() < total {
            return Err(PimError::Protocol(format!(
                "route update truncated: need {total}, have {}",
                buf.len()
            )));
        }

        let mut entries = Vec::with_capacity(entry_count as usize);
        let mut offset = HEADER_SIZE;
        for _ in 0..entry_count {
            let mut dest = [0u8; 16];
            dest.copy_from_slice(&buf[offset..offset + 16]);
            let hops = buf[offset + 16];
            let flags = buf[offset + 17];
            let mut mesh_ip = [0u8; 4];
            mesh_ip.copy_from_slice(&buf[offset + 18..offset + 22]);
            entries.push(RouteEntry {
                destination: NodeId::from_bytes(dest),
                hops,
                flags,
                mesh_ip,
            });
            offset += ENTRY_SIZE;
        }

        let mut signature = [0u8; 64];
        signature.copy_from_slice(&buf[offset..offset + SIGNATURE_SIZE]);

        buf.advance(total);

        Ok(RouteUpdateFrame {
            origin_id,
            sequence,
            entries,
            signature,
        })
    }
}

#[cfg(test)]
mod tests;
