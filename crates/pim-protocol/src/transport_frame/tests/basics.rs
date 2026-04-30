use super::super::*;

fn sample_frame() -> TransportFrame {
    TransportFrame {
        frame_type: FrameType::Data,
        nonce: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        payload: bytes::Bytes::from(vec![0xDE, 0xAD, 0xBE, 0xEF]),
        tag: [0xAA; 16],
    }
}

#[test]
fn round_trip() {
    let frame = sample_frame();
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);

    let decoded = TransportFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn exact_byte_layout() {
    let frame = sample_frame();
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);

    // magic
    assert_eq!(buf[0], 0x50);
    assert_eq!(buf[1], 0x4D);
    // version
    assert_eq!(buf[2], 1);
    // frame_type = Data = 0x02
    assert_eq!(buf[3], 0x02);
    // length = 4 (big-endian)
    assert_eq!(&buf[4..8], &[0, 0, 0, 4]);
    // nonce
    assert_eq!(&buf[8..20], &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    // payload
    assert_eq!(&buf[20..24], &[0xDE, 0xAD, 0xBE, 0xEF]);
    // tag
    assert_eq!(&buf[24..40], &[0xAA; 16]);
}

#[test]
fn reject_truncated() {
    let mut buf = BytesMut::from(&[0x50, 0x4D, 0x01][..]);
    assert!(TransportFrame::decode(&mut buf).is_err());
}

#[test]
fn reject_invalid_magic() {
    let frame = sample_frame();
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf[0] = 0xFF;

    assert!(TransportFrame::decode(&mut buf)
        .unwrap_err()
        .to_string()
        .contains("invalid magic"));
}

#[test]
fn reject_invalid_version() {
    let frame = sample_frame();
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf[2] = 99;

    assert!(TransportFrame::decode(&mut buf)
        .unwrap_err()
        .to_string()
        .contains("unsupported version"));
}

#[test]
fn reject_oversized_payload() {
    let mut buf = BytesMut::new();
    buf.put_u16(MAGIC);
    buf.put_u8(VERSION);
    buf.put_u8(0x02); // Data
    buf.put_u32(MAX_PAYLOAD_SIZE + 1);
    buf.put_slice(&[0u8; 12]); // nonce

    assert!(TransportFrame::decode(&mut buf)
        .unwrap_err()
        .to_string()
        .contains("payload too large"));
}

#[test]
fn empty_payload_round_trip() {
    let frame = TransportFrame {
        frame_type: FrameType::Heartbeat,
        nonce: [0; 12],
        payload: bytes::Bytes::new(),
        tag: [0; 16],
    };
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let decoded = TransportFrame::decode(&mut buf).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn random_bytes_dont_panic() {
    for seed in 0..100u8 {
        let random: Vec<u8> = (0..64)
            .map(|i| seed.wrapping_add(i).wrapping_mul(37))
            .collect();
        let mut buf = BytesMut::from(random.as_slice());
        // Should return Err, never panic
        let _ = TransportFrame::decode(&mut buf);
    }
}
