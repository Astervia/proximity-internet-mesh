use super::super::*;
use crate::fragment_frame::fragment_packet;

fn big_packet(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i & 0xFF) as u8).collect()
}

#[test]
fn small_packet_single_fragment_reassembled() {
    let data = big_packet(100);
    let frags = fragment_packet(bytes::Bytes::from(data.clone()), 1);
    assert_eq!(frags.len(), 1);

    let mut r = Reassembler::new();
    let result = r.insert(frags.into_iter().next().unwrap());
    assert_eq!(result.as_deref(), Some(data.as_slice()));
}

#[test]
fn large_packet_in_order_reassembled() {
    let data = big_packet(4000);
    let frags = fragment_packet(bytes::Bytes::from(data.clone()), 42);
    assert!(frags.len() > 1);

    let mut r = Reassembler::new();
    let mut result = None;
    for f in frags {
        result = r.insert(f);
    }
    assert_eq!(result.as_deref(), Some(data.as_slice()));
    assert_eq!(r.buffer_count(), 0);
}

#[test]
fn out_of_order_fragments_reassembled() {
    let data = big_packet(4000);
    let mut frags = fragment_packet(bytes::Bytes::from(data.clone()), 7);
    // Reverse the order
    frags.reverse();

    let mut r = Reassembler::new();
    let mut result = None;
    for f in frags {
        result = r.insert(f);
    }
    assert_eq!(result.as_deref(), Some(data.as_slice()));
}

#[test]
fn duplicate_fragment_is_ignored() {
    let data = big_packet(2500);
    let frags = fragment_packet(bytes::Bytes::from(data.clone()), 99);
    let mut r = Reassembler::new();

    // Send the first fragment twice.
    r.insert(frags[0].clone());
    r.insert(frags[0].clone()); // duplicate — should be silently ignored

    let mut result = None;
    for f in frags {
        result = r.insert(f);
    }
    assert_eq!(result.as_deref(), Some(data.as_slice()));
}

#[test]
fn missing_fragment_prevents_reassembly() {
    let data = big_packet(4000);
    let frags = fragment_packet(bytes::Bytes::from(data.clone()), 5);

    let mut r = Reassembler::new();
    // Skip the second fragment.
    for (i, f) in frags.into_iter().enumerate() {
        if i == 1 {
            continue;
        }
        let res = r.insert(f);
        assert!(res.is_none(), "must not reassemble with a missing fragment");
    }
    assert_eq!(r.buffer_count(), 1, "incomplete buffer must remain");
}

#[test]
fn timeout_discards_incomplete_buffer() {
    let data = big_packet(2500);
    let frags = fragment_packet(bytes::Bytes::from(data.clone()), 3);

    // Use a zero-duration timeout so it expires immediately.
    let mut r = Reassembler::with_timeout(Duration::ZERO);
    r.insert(frags[0].clone());
    assert_eq!(r.buffer_count(), 1);

    r.expire_stale();
    assert_eq!(r.buffer_count(), 0, "expired buffer must be removed");
}

#[test]
fn multiple_concurrent_streams() {
    let data_a = big_packet(2500);
    let data_b = big_packet(3600);
    let frags_a = fragment_packet(bytes::Bytes::from(data_a.clone()), 10);
    let frags_b = fragment_packet(bytes::Bytes::from(data_b.clone()), 20);

    let mut r = Reassembler::new();

    // Interleave fragments from two different streams.
    let max = frags_a.len().max(frags_b.len());
    let mut result_a = None;
    let mut result_b = None;
    for i in 0..max {
        if i < frags_a.len() {
            let res = r.insert(frags_a[i].clone());
            if res.is_some() {
                result_a = res;
            }
        }
        if i < frags_b.len() {
            let res = r.insert(frags_b[i].clone());
            if res.is_some() {
                result_b = res;
            }
        }
    }
    assert_eq!(result_a.as_deref(), Some(data_a.as_slice()));
    assert_eq!(result_b.as_deref(), Some(data_b.as_slice()));
    assert_eq!(r.buffer_count(), 0);
}
