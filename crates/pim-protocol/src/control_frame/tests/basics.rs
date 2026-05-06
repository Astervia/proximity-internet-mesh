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
fn plugin_payload_round_trip() {
    let frame = ControlFrame::PluginPayload {
        kind: "messaging.msg".into(),
        body: bytes::Bytes::from_static(&[0xDE, 0xAD, 0xBE, 0xEF]),
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = ControlFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn plugin_payload_empty_body_round_trip() {
    let frame = ControlFrame::PluginPayload {
        kind: "messaging.ack".into(),
        body: bytes::Bytes::new(),
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
fn reject_truncated_plugin_payload_header() {
    // tag + kind_len = 0x05 promising 5 bytes of kind, but no kind/body follows.
    let mut buf = BytesMut::from(&[0x08, 0x05][..]);
    assert!(ControlFrame::decode(&mut buf).is_err());
}

#[test]
fn reject_truncated_plugin_payload_body() {
    // tag + kind_len(1) + kind("a") + body_len(0x0008) + only 2 body bytes.
    let mut buf = BytesMut::from(&[0x08, 0x01, b'a', 0x00, 0x08, 0xAA, 0xBB][..]);
    assert!(ControlFrame::decode(&mut buf).is_err());
}
