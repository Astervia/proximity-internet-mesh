//! Discovery advertisement: wire format and capabilities.

use aes_gcm::aead::KeyInit;
use aes_gcm::{Aes256Gcm, Nonce};
use pim_core::NodeId;
use rand::rngs::OsRng;
use rand::RngCore;

/// Magic bytes that prefix every discovery packet.
pub const MAGIC: [u8; 4] = *b"PIMD";
/// Current discovery advertisement wire-format version.
pub const VERSION: u8 = 0x01;
/// Encrypted discovery advertisement wire-format version.
pub const ENCRYPTED_VERSION: u8 = 0x02;
/// Total serialized advertisement size in bytes.
pub const PACKET_SIZE: usize = 56; // magic(4)+version(1)+node_id(16)+pub_key(32)+caps(1)+port(2)
/// Total encrypted advertisement size in bytes.
pub const ENCRYPTED_PACKET_SIZE: usize = 89; // magic(4)+version(1)+nonce(12)+ciphertext(56)+tag(16)

// ── NodeCapabilities ──────────────────────────────────────────────────────────

/// Role(s) a node can fulfil simultaneously.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeCapabilities(u8);

impl NodeCapabilities {
    /// Bit indicating generic client capability.
    pub const CLIENT: u8 = 0x01;
    /// Bit indicating relay capability.
    pub const RELAY: u8 = 0x02;
    /// Bit indicating internet gateway capability.
    pub const GATEWAY: u8 = 0x04;

    /// Construct capabilities directly from a raw bitset.
    pub fn new(bits: u8) -> Self {
        Self(bits)
    }

    /// Create a client-only capability set.
    pub fn client() -> Self {
        Self(Self::CLIENT)
    }

    /// Create a relay capability set.
    pub fn relay() -> Self {
        Self(Self::CLIENT | Self::RELAY)
    }

    /// Create a gateway capability set.
    pub fn gateway() -> Self {
        Self(Self::CLIENT | Self::RELAY | Self::GATEWAY)
    }

    /// Return `true` if the node can originate client traffic.
    pub fn is_client(self) -> bool {
        self.0 & Self::CLIENT != 0
    }
    /// Return `true` if the node can relay mesh traffic.
    pub fn is_relay(self) -> bool {
        self.0 & Self::RELAY != 0
    }
    /// Return `true` if the node can provide internet gateway service.
    pub fn is_gateway(self) -> bool {
        self.0 & Self::GATEWAY != 0
    }
    /// Return the raw capability bitset.
    pub fn bits(self) -> u8 {
        self.0
    }
}

// ── DiscoveryAdvertisement ────────────────────────────────────────────────────

/// A parsed discovery advertisement broadcast by a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryAdvertisement {
    /// Stable node identifier of the advertiser.
    pub node_id: NodeId,
    /// Advertiser's Ed25519 public key.
    pub public_key: [u8; 32],
    /// Advertised node roles.
    pub capabilities: NodeCapabilities,
    /// TCP listen port (the IP is taken from the UDP source address).
    pub listen_port: u16,
}

impl DiscoveryAdvertisement {
    /// Serialize to 56-byte wire format.
    pub fn serialize(&self) -> [u8; PACKET_SIZE] {
        self.plaintext_bytes()
    }

    /// Serialize to the encrypted wire format.
    pub fn serialize_encrypted(&self, key: &[u8; 32]) -> [u8; ENCRYPTED_PACKET_SIZE] {
        use aes_gcm::aead::AeadInPlace;

        let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key is valid for AES-256");
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let mut buf = [0u8; ENCRYPTED_PACKET_SIZE];
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4] = ENCRYPTED_VERSION;
        buf[5..17].copy_from_slice(&nonce_bytes);
        buf[17..17 + PACKET_SIZE].copy_from_slice(&self.plaintext_bytes());

        let tag = cipher
            .encrypt_in_place_detached(nonce, b"", &mut buf[17..17 + PACKET_SIZE])
            .expect("discovery advertisement encryption should succeed");

        buf[17 + PACKET_SIZE..].copy_from_slice(&tag);
        buf
    }

    fn plaintext_bytes(&self) -> [u8; PACKET_SIZE] {
        let mut buf = [0u8; PACKET_SIZE];
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4] = VERSION;
        buf[5..21].copy_from_slice(self.node_id.as_bytes());
        buf[21..53].copy_from_slice(&self.public_key);
        buf[53] = self.capabilities.bits();
        let port_bytes = self.listen_port.to_be_bytes();
        buf[54..56].copy_from_slice(&port_bytes);
        buf
    }

    /// Deserialize from a byte slice.  Returns `None` if magic/version mismatch
    /// or the buffer is too short.
    pub fn deserialize(buf: &[u8]) -> Option<Self> {
        if buf.len() < PACKET_SIZE {
            return None;
        }
        if buf[0..4] != MAGIC {
            return None;
        }
        if buf[4] != VERSION {
            return None;
        }
        Self::deserialize_plaintext_payload(&buf[..PACKET_SIZE])
    }

    /// Decrypt and deserialize from a byte slice using the shared discovery key.
    pub fn deserialize_encrypted(buf: &[u8], key: &[u8; 32]) -> Option<Self> {
        use aes_gcm::aead::{AeadInPlace, Tag};

        if buf.len() < ENCRYPTED_PACKET_SIZE {
            return None;
        }
        if buf[0..4] != MAGIC {
            return None;
        }
        if buf[4] != ENCRYPTED_VERSION {
            return None;
        }

        let mut nonce_bytes = [0u8; 12];
        nonce_bytes.copy_from_slice(&buf[5..17]);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let cipher = Aes256Gcm::new_from_slice(key).ok()?;

        let mut plaintext = [0u8; PACKET_SIZE];
        plaintext.copy_from_slice(&buf[17..17 + PACKET_SIZE]);
        let tag = Tag::<Aes256Gcm>::from_slice(&buf[17 + PACKET_SIZE..17 + PACKET_SIZE + 16]);

        cipher
            .decrypt_in_place_detached(nonce, b"", &mut plaintext, tag)
            .ok()?;

        Self::deserialize_plaintext_payload(&plaintext)
    }

    fn deserialize_plaintext_payload(buf: &[u8]) -> Option<Self> {
        if buf.len() < PACKET_SIZE {
            return None;
        }

        let mut id_bytes = [0u8; 16];
        id_bytes.copy_from_slice(&buf[5..21]);
        let node_id = NodeId::from_bytes(id_bytes);

        let mut public_key = [0u8; 32];
        public_key.copy_from_slice(&buf[21..53]);

        let capabilities = NodeCapabilities::new(buf[53]);

        let listen_port = u16::from_be_bytes([buf[54], buf[55]]);

        Some(Self {
            node_id,
            public_key,
            capabilities,
            listen_port,
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
