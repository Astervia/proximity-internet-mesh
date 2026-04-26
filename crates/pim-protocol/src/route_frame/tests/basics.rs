use super::super::*;

#[test]
fn round_trip_with_entries() {
    let frame = RouteUpdateFrame {
        origin_id: NodeId::from_bytes([0x01; 16]),
        sequence: 42,
        entries: vec![
            RouteEntry {
                destination: NodeId::from_bytes([0x02; 16]),
                hops: 1,
                flags: 0x01, // is_gateway
                mesh_ip: [10, 77, 0, 1],
            },
            RouteEntry {
                destination: NodeId::from_bytes([0x03; 16]),
                hops: 3,
                flags: 0x00,
                mesh_ip: [10, 77, 0, 10],
            },
        ],
        signature: [0xAA; 64],
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = RouteUpdateFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
    assert!(decoded.entries[0].is_gateway());
    assert!(!decoded.entries[1].is_gateway());
}

#[test]
fn round_trip_empty_entries() {
    let frame = RouteUpdateFrame {
        origin_id: NodeId::from_bytes([0xFF; 16]),
        sequence: 0,
        entries: vec![],
        signature: [0x00; 64],
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = RouteUpdateFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn reject_truncated() {
    let mut buf = BytesMut::from(&[0u8; 10][..]);
    assert!(RouteUpdateFrame::decode(&mut buf).is_err());
}

#[test]
fn reject_truncated_entries() {
    let frame = RouteUpdateFrame {
        origin_id: NodeId::from_bytes([1; 16]),
        sequence: 1,
        entries: vec![RouteEntry {
            destination: NodeId::from_bytes([2; 16]),
            hops: 1,
            flags: 0,
            mesh_ip: [10, 77, 0, 2],
        }],
        signature: [0; 64],
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf.truncate(buf.len() - 10); // chop off some of signature
    assert!(RouteUpdateFrame::decode(&mut buf).is_err());
}
