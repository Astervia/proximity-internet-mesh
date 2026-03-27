use bytes::BytesMut;
use std::fmt;
use std::net::Ipv4Addr;

use sha2::{Digest, Sha256};

use crate::PimError;

/// 16-byte node identifier derived from the SHA-256 hash of the node's public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId([u8; 16]);

impl NodeId {
    /// Create a NodeId from raw bytes.
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Derive a NodeId from an Ed25519 public key.
    pub fn from_public_key(public_key: &[u8; 32]) -> Self {
        let hash = Sha256::digest(public_key);
        let mut id = [0u8; 16];
        id.copy_from_slice(&hash[..16]);
        Self(id)
    }

    /// Return the raw bytes.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Show first 4 and last 4 bytes in hex: "a3f1b2c4..9d32e0f8"
        write!(
            f,
            "{:02x}{:02x}{:02x}{:02x}..{:02x}{:02x}{:02x}{:02x}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[12], self.0[13], self.0[14],
            self.0[15],
        )
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex: String = self.0.iter().map(|b| format!("{b:02x}")).collect();
        write!(f, "NodeId({hex})")
    }
}

/// Wrapper around Ipv4Addr for mesh-internal IP addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct MeshIp(Ipv4Addr);

impl MeshIp {
    pub fn new(addr: Ipv4Addr) -> Self {
        Self(addr)
    }

    pub fn addr(&self) -> Ipv4Addr {
        self.0
    }
}

impl From<Ipv4Addr> for MeshIp {
    fn from(addr: Ipv4Addr) -> Self {
        Self(addr)
    }
}

impl From<MeshIp> for Ipv4Addr {
    fn from(mesh_ip: MeshIp) -> Self {
        mesh_ip.0
    }
}

impl fmt::Display for MeshIp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Trait for encoding/decoding wire protocol frames.
pub trait FrameCodec: Sized {
    fn encode(&self, buf: &mut BytesMut);
    fn decode(buf: &mut BytesMut) -> Result<Self, PimError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_from_bytes() {
        let bytes = [1u8; 16];
        let id = NodeId::from_bytes(bytes);
        assert_eq!(id.as_bytes(), &bytes);
    }

    #[test]
    fn node_id_from_public_key_deterministic() {
        let key = [42u8; 32];
        let id1 = NodeId::from_public_key(&key);
        let id2 = NodeId::from_public_key(&key);
        assert_eq!(id1, id2);
    }

    #[test]
    fn node_id_different_keys_produce_different_ids() {
        let key1 = [1u8; 32];
        let key2 = [2u8; 32];
        assert_ne!(NodeId::from_public_key(&key1), NodeId::from_public_key(&key2));
    }

    #[test]
    fn node_id_display_format() {
        let bytes = [
            0xa3, 0xf1, 0xb2, 0xc4, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x9d, 0x32,
            0xe0, 0xf8,
        ];
        let id = NodeId::from_bytes(bytes);
        assert_eq!(format!("{id}"), "a3f1b2c4..9d32e0f8");
    }

    #[test]
    fn mesh_ip_round_trip() {
        let addr = Ipv4Addr::new(10, 77, 0, 5);
        let mesh = MeshIp::new(addr);
        assert_eq!(mesh.addr(), addr);
        let back: Ipv4Addr = mesh.into();
        assert_eq!(back, addr);
    }

    #[test]
    fn mesh_ip_display() {
        let mesh = MeshIp::new(Ipv4Addr::new(10, 77, 0, 1));
        assert_eq!(format!("{mesh}"), "10.77.0.1");
    }
}
