use super::super::*;

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
