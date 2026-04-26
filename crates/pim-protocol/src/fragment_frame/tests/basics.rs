use super::super::*;

#[test]
fn single_fragment_for_small_packet() {
    let data = vec![0xAB; 100];
    let frags = fragment_packet(&data, 1);
    assert_eq!(frags.len(), 1);
    assert_eq!(frags[0].fragment_offset, 0);
    assert_eq!(frags[0].total_length, 100);
    assert!(frags[0].is_last());
}

#[test]
fn large_packet_produces_multiple_fragments() {
    let data = vec![0x55; 4000];
    let frags = fragment_packet(&data, 42);

    // 4000 / 1200 = 3 full + 1 partial → 4 fragments
    let expected = 4000_usize.div_ceil(MAX_FRAGMENT_PAYLOAD);
    assert_eq!(frags.len(), expected);

    // offsets must be contiguous
    let mut expected_offset = 0u16;
    for f in &frags {
        assert_eq!(f.fragment_id, 42);
        assert_eq!(f.total_length, 4000);
        assert_eq!(f.fragment_offset, expected_offset);
        expected_offset += f.payload.len() as u16;
    }

    assert!(frags.last().unwrap().is_last());
}

#[test]
fn serialize_deserialize_round_trip() {
    let frame = FragmentFrame {
        fragment_id: 0xDEAD_BEEF,
        fragment_offset: 1200,
        total_length: 2400,
        payload: bytes::Bytes::from(vec![1, 2, 3, 4]),
    };
    let bytes = frame.serialize();
    let decoded = FragmentFrame::deserialize(&bytes).unwrap();
    assert_eq!(frame, decoded);
}

#[test]
fn deserialize_too_short_returns_none() {
    assert!(FragmentFrame::deserialize(&[0u8; 7]).is_none());
}

#[test]
fn deserialize_offset_past_total_returns_none() {
    // Craft a frame where offset + payload.len() > total_length
    let mut bad = FragmentFrame {
        fragment_id: 1,
        fragment_offset: 1000,
        total_length: 500, // total < offset
        payload: bytes::Bytes::from(vec![0u8; 100]),
    };
    // Serialize then hand-patch so deserializer sees inconsistent values.
    let bytes = bad.serialize();
    // total_length bytes are at [6..8]; set to something smaller than offset+payload
    let bytes2 = bytes.clone();
    // offset = 1000, payload = 100, so end = 1100; total must be >= 1100.
    // We set total_length = 500, which is < 1100. This should fail.
    bad.total_length = 500;
    let bytes3 = bad.serialize();
    assert!(FragmentFrame::deserialize(&bytes3).is_none());
    let _ = bytes2; // suppress warning
}

#[test]
fn empty_data_produces_no_fragments() {
    assert!(fragment_packet(&[], 0).is_empty());
}
