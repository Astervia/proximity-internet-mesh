use super::super::*;
use tempfile::TempDir;

#[test]
fn generate_produces_valid_identity() {
    let id = Identity::generate();
    // NodeId should be derived from the public key
    let expected = NodeId::from_public_key(&id.public_key_bytes());
    assert_eq!(id.node_id(), expected);
}

#[test]
fn save_and_load_round_trip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("node.key");

    let original = Identity::generate();
    original.save(&path).unwrap();

    let loaded = Identity::load(&path).unwrap();
    assert_eq!(original.node_id(), loaded.node_id());
    assert_eq!(original.public_key_bytes(), loaded.public_key_bytes());
}

#[test]
fn load_invalid_length_fails() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad.key");
    std::fs::write(&path, [0u8; 16]).unwrap();

    let result = Identity::load(&path);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("32 bytes"));
}

#[test]
fn load_nonexistent_fails() {
    let result = Identity::load(Path::new("/tmp/pim_nonexistent_key_12345"));
    assert!(result.is_err());
}

#[test]
fn load_or_generate_creates_new() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("node.key");

    assert!(!path.exists());
    let id = Identity::load_or_generate(&path).unwrap();
    assert!(path.exists());

    // Loading again should give the same identity
    let id2 = Identity::load_or_generate(&path).unwrap();
    assert_eq!(id.node_id(), id2.node_id());
}

#[test]
fn two_identities_are_different() {
    let a = Identity::generate();
    let b = Identity::generate();
    assert_ne!(a.node_id(), b.node_id());
    assert_ne!(a.public_key_bytes(), b.public_key_bytes());
}
