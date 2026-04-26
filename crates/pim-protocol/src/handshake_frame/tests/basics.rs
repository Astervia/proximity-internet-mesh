use super::super::*;

#[test]
fn init_round_trip() {
    let frame = HandshakeWireFrame::InitOrResponse {
        handshake_type: HandshakeFrameType::Init,
        sender_pub: [0xAA; 32],
        ephemeral_pub: [0xBB; 32],
        nonce: [0xCC; 32],
        signature: [0xDD; 64],
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = HandshakeWireFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn response_round_trip() {
    let frame = HandshakeWireFrame::InitOrResponse {
        handshake_type: HandshakeFrameType::Response,
        sender_pub: [0x11; 32],
        ephemeral_pub: [0x22; 32],
        nonce: [0x33; 32],
        signature: [0x44; 64],
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = HandshakeWireFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn confirm_round_trip() {
    let frame = HandshakeWireFrame::Confirm { hmac: [0xFF; 32] };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = HandshakeWireFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn reject_truncated_init() {
    let mut buf = BytesMut::from(&[0u8; 50][..]); // too short for init
    assert!(HandshakeWireFrame::decode(&mut buf).is_err());
}

#[test]
fn reject_empty() {
    let mut buf = BytesMut::new();
    assert!(HandshakeWireFrame::decode(&mut buf).is_err());
}

#[test]
fn reject_unknown_type() {
    let mut buf = BytesMut::from(&[0xFF][..]);
    assert!(HandshakeWireFrame::decode(&mut buf).is_err());
}
