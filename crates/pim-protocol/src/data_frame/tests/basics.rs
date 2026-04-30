use super::super::*;

fn sample_frame() -> MeshDataFrame {
    MeshDataFrame {
        src_id: NodeId::from_bytes([1; 16]),
        dst_id: NodeId::from_bytes([2; 16]),
        session_id: 42,
        ttl: 10,
        flags: DataFlags::IS_INTERNET,
        payload: bytes::Bytes::from(vec![0xCA, 0xFE]),
    }
}

#[test]
fn round_trip() {
    let frame = sample_frame();
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = MeshDataFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn flags_preserved() {
    let frame = MeshDataFrame {
        flags: DataFlags::IS_FRAGMENT | DataFlags::IS_LAST_FRAGMENT | DataFlags::REQUIRES_ACK,
        ..sample_frame()
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = MeshDataFrame::decode(&mut buf).unwrap();
    assert!(decoded.flags.contains(DataFlags::IS_FRAGMENT));
    assert!(decoded.flags.contains(DataFlags::IS_LAST_FRAGMENT));
    assert!(decoded.flags.contains(DataFlags::REQUIRES_ACK));
    assert!(!decoded.flags.contains(DataFlags::IS_INTERNET));
}

#[test]
fn reject_truncated_header() {
    let mut buf = BytesMut::from(&[0u8; 20][..]);
    assert!(MeshDataFrame::decode(&mut buf).is_err());
}

#[test]
fn reject_truncated_payload() {
    let frame = sample_frame();
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf.truncate(buf.len() - 1); // chop 1 byte of payload
    assert!(MeshDataFrame::decode(&mut buf).is_err());
}

#[test]
fn empty_payload() {
    let frame = MeshDataFrame {
        payload: bytes::Bytes::new(),
        ..sample_frame()
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = MeshDataFrame::decode(&mut buf).unwrap();
    assert!(decoded.payload.is_empty());
}
