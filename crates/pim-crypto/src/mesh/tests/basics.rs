use super::super::*;

// Use a deliberately weak Argon2 profile in tests so the suite runs
// fast. The production default (m=64MiB, t=3) takes ~100ms each;
// tests use m=8KiB, t=1 which is ~milliseconds.
fn test_params() -> MeshKdfParams {
    MeshKdfParams {
        m_cost_kib: 8,
        t_cost: 1,
        p_cost: 1,
    }
}

#[test]
fn same_inputs_yield_same_secret() {
    let a = MeshSecret::derive("hunter2", Some("office"), test_params()).unwrap();
    let b = MeshSecret::derive("hunter2", Some("office"), test_params()).unwrap();
    assert_eq!(a.discovery_key(), b.discovery_key());
    assert_eq!(a.handshake_key(), b.handshake_key());
    assert_eq!(a.fingerprint(), b.fingerprint());
}

#[test]
fn different_passphrase_yields_different_secret() {
    let a = MeshSecret::derive("hunter2", Some("office"), test_params()).unwrap();
    let b = MeshSecret::derive("hunter3", Some("office"), test_params()).unwrap();
    assert_ne!(a.handshake_key(), b.handshake_key());
    assert_ne!(a.discovery_key(), b.discovery_key());
    assert_ne!(a.fingerprint(), b.fingerprint());
}

#[test]
fn mesh_id_separates_meshes_with_same_passphrase() {
    let a = MeshSecret::derive("hunter2", Some("home"), test_params()).unwrap();
    let b = MeshSecret::derive("hunter2", Some("office"), test_params()).unwrap();
    assert_ne!(a.handshake_key(), b.handshake_key());
    assert_ne!(a.fingerprint(), b.fingerprint());
}

#[test]
fn empty_and_none_mesh_id_are_equivalent() {
    let a = MeshSecret::derive("hunter2", None, test_params()).unwrap();
    let b = MeshSecret::derive("hunter2", Some(""), test_params()).unwrap();
    assert_eq!(a.handshake_key(), b.handshake_key());
    assert_eq!(a.fingerprint(), b.fingerprint());
}

#[test]
fn sub_keys_are_distinct_from_each_other() {
    let s = MeshSecret::derive("hunter2", Some("office"), test_params()).unwrap();
    // Different HKDF info strings must yield different sub-keys
    // (overlap would indicate broken derivation).
    assert_ne!(s.discovery_key(), s.handshake_key());
}

#[test]
fn empty_passphrase_rejected() {
    let err = MeshSecret::derive("", Some("office"), test_params()).unwrap_err();
    assert!(matches!(err, MeshKdfError::EmptyPassphrase));
}

#[test]
fn debug_does_not_leak_keys() {
    let s = MeshSecret::derive("hunter2", Some("office"), test_params()).unwrap();
    let dbg = format!("{s:?}");
    let hk_hex: String = s
        .handshake_key()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let dk_hex: String = s
        .discovery_key()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert!(!dbg.contains(&hk_hex), "Debug must not print handshake key");
    assert!(!dbg.contains(&dk_hex), "Debug must not print discovery key");
    // Fingerprint is non-secret and OK to expose.
    assert!(dbg.contains(&s.fingerprint_hex()));
}

#[test]
fn fingerprint_hex_is_16_chars() {
    let s = MeshSecret::derive("hunter2", Some("office"), test_params()).unwrap();
    assert_eq!(s.fingerprint_hex().len(), 16);
}

#[test]
fn rfcomm_hello_tag_is_deterministic_per_key_and_node() {
    let key = [0x42u8; 32];
    let a = compute_rfcomm_hello_tag(&key, "0123456789abcdef0123456789abcdef");
    let b = compute_rfcomm_hello_tag(&key, "0123456789abcdef0123456789abcdef");
    assert_eq!(a, b);
}

#[test]
fn rfcomm_hello_tag_changes_with_key() {
    let id = "0123456789abcdef0123456789abcdef";
    let a = compute_rfcomm_hello_tag(&[0x42u8; 32], id);
    let b = compute_rfcomm_hello_tag(&[0x43u8; 32], id);
    assert_ne!(a, b);
}

#[test]
fn rfcomm_hello_tag_changes_with_node_id() {
    let key = [0x42u8; 32];
    let a = compute_rfcomm_hello_tag(&key, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let b = compute_rfcomm_hello_tag(&key, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    assert_ne!(a, b);
}
