//! Discovery advertisement: wire format and capabilities.

use aes_gcm::aead::{Aead, KeyInit};
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
        let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key is valid for AES-256");
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(nonce, self.plaintext_bytes().as_ref())
            .expect("discovery advertisement encryption should succeed");

        let mut buf = [0u8; ENCRYPTED_PACKET_SIZE];
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4] = ENCRYPTED_VERSION;
        buf[5..17].copy_from_slice(&nonce_bytes);
        buf[17..].copy_from_slice(&ciphertext);
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
        let plaintext = cipher.decrypt(nonce, &buf[17..]).ok()?;
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
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn sample_ad() -> DiscoveryAdvertisement {
        DiscoveryAdvertisement {
            node_id: NodeId::from_bytes([0xAB; 16]),
            public_key: [0x55; 32],
            capabilities: NodeCapabilities::relay(),
            listen_port: 9100,
        }
    }

    fn test_key(seed: u64) -> [u8; 32] {
        let mut rng = StdRng::seed_from_u64(seed);
        // Initialize the key material without using a hard-coded byte pattern.
        // The contents are immediately overwritten by `fill_bytes`, so this
        // preserves the deterministic behavior for a given seed.
        let mut key: [u8; 32] = rng
            .next_u64()
            .to_le_bytes()
            .repeat(4)
            .try_into()
            .unwrap();
        rng.fill_bytes(&mut key);
        key
    }

    #[test]
    fn round_trip() {
        let ad = sample_ad();
        let bytes = ad.serialize();
        assert_eq!(bytes.len(), PACKET_SIZE);
        let decoded = DiscoveryAdvertisement::deserialize(&bytes).unwrap();
        assert_eq!(ad, decoded);
    }

    #[test]
    fn bad_magic_rejected() {
        let mut bytes = sample_ad().serialize();
        bytes[0] = 0xFF;
        assert!(DiscoveryAdvertisement::deserialize(&bytes).is_none());
    }

    #[test]
    fn bad_version_rejected() {
        let mut bytes = sample_ad().serialize();
        bytes[4] = 0xFF;
        assert!(DiscoveryAdvertisement::deserialize(&bytes).is_none());
    }

    #[test]
    fn too_short_rejected() {
        assert!(DiscoveryAdvertisement::deserialize(&[0u8; 10]).is_none());
    }

    #[test]
    fn client_capabilities() {
        let caps = NodeCapabilities::client();
        assert!(caps.is_client());
        assert!(!caps.is_relay());
        assert!(!caps.is_gateway());
    }

    #[test]
    fn relay_capabilities() {
        let caps = NodeCapabilities::relay();
        assert!(caps.is_client());
        assert!(caps.is_relay());
        assert!(!caps.is_gateway());
    }

    #[test]
    fn gateway_capabilities() {
        let caps = NodeCapabilities::gateway();
        assert!(caps.is_client());
        assert!(caps.is_relay());
        assert!(caps.is_gateway());
    }

    #[test]
    fn capabilities_round_trip() {
        let ad = DiscoveryAdvertisement {
            capabilities: NodeCapabilities::gateway(),
            ..sample_ad()
        };
        let decoded = DiscoveryAdvertisement::deserialize(&ad.serialize()).unwrap();
        assert!(decoded.capabilities.is_gateway());
    }

    #[test]
    fn encrypted_round_trip() {
        let ad = sample_ad();
        let key = test_key(1);
        let decoded =
            DiscoveryAdvertisement::deserialize_encrypted(&ad.serialize_encrypted(&key), &key)
                .unwrap();
        assert_eq!(ad, decoded);
    }

    #[test]
    fn encrypted_wrong_key_rejected() {
        let ad = sample_ad();
        let key = test_key(1);
        let wrong_key = test_key(2);
        let bytes = ad.serialize_encrypted(&key);
        assert!(DiscoveryAdvertisement::deserialize_encrypted(&bytes, &wrong_key).is_none());
    }
}
