use super::super::*;

#[test]
fn encode_decode_round_trip() {
    let message = b"hello mesh";
    let mut buf = BytesMut::new();
    LengthDelimitedCodec::encode(message, &mut buf);

    let decoded = LengthDelimitedCodec::decode(&mut buf).unwrap().unwrap();
    assert_eq!(&decoded[..], message);
    assert!(buf.is_empty()); // consumed
}

#[test]
fn partial_length_returns_none() {
    let mut buf = BytesMut::from(&[0x00, 0x00][..]); // only 2 of 4 length bytes
    assert!(LengthDelimitedCodec::decode(&mut buf).unwrap().is_none());
}

#[test]
fn partial_payload_returns_none() {
    let mut buf = BytesMut::new();
    buf.put_u32(10); // says 10 bytes
    buf.put_slice(&[0u8; 5]); // only 5 bytes of payload
    assert!(LengthDelimitedCodec::decode(&mut buf).unwrap().is_none());
}

#[test]
fn multiple_messages() {
    let mut buf = BytesMut::new();
    LengthDelimitedCodec::encode(b"first", &mut buf);
    LengthDelimitedCodec::encode(b"second", &mut buf);

    let m1 = LengthDelimitedCodec::decode(&mut buf).unwrap().unwrap();
    assert_eq!(&m1[..], b"first");

    let m2 = LengthDelimitedCodec::decode(&mut buf).unwrap().unwrap();
    assert_eq!(&m2[..], b"second");

    assert!(buf.is_empty());
}

#[test]
fn empty_message() {
    let mut buf = BytesMut::new();
    LengthDelimitedCodec::encode(b"", &mut buf);
    let decoded = LengthDelimitedCodec::decode(&mut buf).unwrap().unwrap();
    assert!(decoded.is_empty());
}

#[test]
fn reject_oversized_frame() {
    let mut buf = BytesMut::new();
    buf.put_u32(MAX_FRAME_SIZE + 1);
    assert!(LengthDelimitedCodec::decode(&mut buf).is_err());
}

#[test]
fn empty_buffer_returns_none() {
    let mut buf = BytesMut::new();
    assert!(LengthDelimitedCodec::decode(&mut buf).unwrap().is_none());
}
