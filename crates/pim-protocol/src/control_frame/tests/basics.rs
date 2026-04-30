use super::super::*;

#[test]
fn ip_request_round_trip() {
    let frame = ControlFrame::IpRequest {
        requester_id: NodeId::from_bytes([0x42; 16]),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = ControlFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn ip_assign_round_trip() {
    let frame = ControlFrame::IpAssign {
        assigned_ip: [10, 77, 0, 5],
        subnet_mask: 16,
        gateway_ip: [10, 77, 0, 1],
        lease_seconds: 3600,
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = ControlFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn goodbye_round_trip() {
    let frame = ControlFrame::Goodbye {
        departing_id: NodeId::from_bytes([0xAB; 16]),
        reason: 0,
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = ControlFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn rekey_round_trip() {
    let frame = ControlFrame::Rekey;
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = ControlFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn ping_pong_round_trip() {
    let ping = ControlFrame::Ping { nonce: 12345 };
    let pong = ControlFrame::Pong { nonce: 12345 };

    for frame in [ping, pong] {
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let decoded = ControlFrame::decode(&mut buf).unwrap();
        assert_eq!(frame, decoded);
    }
}

#[test]
fn reject_empty() {
    let mut buf = BytesMut::new();
    assert!(ControlFrame::decode(&mut buf).is_err());
}

#[test]
fn reject_unknown_type() {
    let mut buf = BytesMut::from(&[0xFF][..]);
    assert!(ControlFrame::decode(&mut buf).is_err());
}

#[test]
fn reject_truncated_ip_assign() {
    let mut buf = BytesMut::from(&[0x02, 10, 77, 0][..]); // too short
    assert!(ControlFrame::decode(&mut buf).is_err());
}

#[test]
fn peer_info_round_trip() {
    let frame = ControlFrame::PeerInfo {
        x25519_pub: [0x11; 32],
        friendly_name: "client-a-macbook".into(),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = ControlFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn peer_info_empty_name_round_trip() {
    let frame = ControlFrame::PeerInfo {
        x25519_pub: [0x22; 32],
        friendly_name: String::new(),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = ControlFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn message_round_trip() {
    let frame = ControlFrame::Message {
        message_id: [0x55; 16],
        timestamp_ms: 1_745_960_000_000,
        ciphertext: vec![0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE],
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = ControlFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn message_ack_round_trip() {
    let frame = ControlFrame::MessageAck {
        message_id: [0x77; 16],
        ack_kind: 1,
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = ControlFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn reject_truncated_peer_info() {
    let mut buf = BytesMut::from(&[0x07, 0x00, 0x01][..]); // too short
    assert!(ControlFrame::decode(&mut buf).is_err());
}

#[test]
fn reject_truncated_message() {
    let mut buf = BytesMut::from(&[0x08, 0x00][..]); // too short
    assert!(ControlFrame::decode(&mut buf).is_err());
}
