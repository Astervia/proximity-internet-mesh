use super::super::*;
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
    let mut key: [u8; 32] = rng.next_u64().to_le_bytes().repeat(4).try_into().unwrap();
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
