use super::super::*;

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
    assert_ne!(
        NodeId::from_public_key(&key1),
        NodeId::from_public_key(&key2)
    );
}

#[test]
fn node_id_display_format() {
    let bytes = [
        0xa3, 0xf1, 0xb2, 0xc4, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x9d, 0x32, 0xe0,
        0xf8,
    ];
    let id = NodeId::from_bytes(bytes);
    assert_eq!(format!("{id}"), "a3f1b2c4..9d32e0f8");
}

#[test]
fn node_id_hex_round_trip() {
    let id = NodeId::from_bytes([0xab; 16]);
    let hex = id.to_hex();
    assert_eq!(hex, "abababababababababababababababab");
    assert_eq!(hex.parse::<NodeId>().unwrap(), id);
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
