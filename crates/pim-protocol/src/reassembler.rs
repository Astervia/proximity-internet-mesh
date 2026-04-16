//! Reassembly of fragmented mesh data payloads.
//!
//! [`Reassembler`] collects [`FragmentFrame`]s by their `fragment_id`,
//! reassembles them in order once all bytes have arrived, and discards
//! incomplete buffers after a configurable timeout.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use crate::fragment_frame::FragmentFrame;

/// Default timeout before an incomplete reassembly buffer is discarded.
pub const DEFAULT_REASSEMBLY_TIMEOUT: Duration = Duration::from_secs(10);

// ── Internal state ─────────────────────────────────────────────────────────

struct ReassemblyBuffer {
    /// Total byte length of the original packet.
    total_length: u16,
    /// Received chunks keyed by their starting byte offset within the original
    /// packet.  Stored in a `BTreeMap` so we can iterate offsets in order.
    chunks: BTreeMap<u16, bytes::Bytes>,
    /// Monotonic timestamp of the first fragment received.
    created_at: Instant,
}

impl ReassemblyBuffer {
    fn new(total_length: u16) -> Self {
        Self {
            total_length,
            chunks: BTreeMap::new(),
            created_at: Instant::now(),
        }
    }

    /// Insert a chunk.  Returns `true` if this chunk was new (not a duplicate).
    fn insert(&mut self, offset: u16, payload: bytes::Bytes) -> bool {
        use std::collections::btree_map::Entry;
        match self.chunks.entry(offset) {
            Entry::Vacant(e) => {
                e.insert(payload);
                true
            }
            Entry::Occupied(_) => false,
        }
    }

    /// Returns `Some(packet)` when all bytes `[0, total_length)` have been
    /// received, otherwise `None`.
    fn try_reassemble(&self) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; self.total_length as usize];
        let mut covered_up_to = 0usize;

        for (&offset, chunk) in &self.chunks {
            let start = offset as usize;
            if start > covered_up_to {
                // There is a gap — not complete yet.
                return None;
            }
            let end = start + chunk.len();
            if end > buf.len() {
                // Chunk extends past declared total — malformed, bail out.
                return None;
            }
            buf[start..end].copy_from_slice(chunk);
            if end > covered_up_to {
                covered_up_to = end;
            }
        }

        if covered_up_to == self.total_length as usize {
            Some(buf)
        } else {
            None
        }
    }

    fn is_expired(&self, timeout: Duration) -> bool {
        self.created_at.elapsed() >= timeout
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Reassembles fragmented mesh payloads.
///
/// Call [`Reassembler::insert`] for each received [`FragmentFrame`].  When the
/// last required fragment arrives the complete packet is returned.  Call
/// [`Reassembler::expire_stale`] periodically (e.g. once per second) to free
/// memory for timed-out incomplete buffers.
pub struct Reassembler {
    timeout: Duration,
    buffers: HashMap<u32, ReassemblyBuffer>,
}

impl Reassembler {
    /// Create a reassembler with the default 10-second timeout.
    pub fn new() -> Self {
        Self {
            timeout: DEFAULT_REASSEMBLY_TIMEOUT,
            buffers: HashMap::new(),
        }
    }

    /// Create a reassembler with a custom timeout (useful in tests).
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            buffers: HashMap::new(),
        }
    }

    /// Feed a [`FragmentFrame`] to the reassembler.
    ///
    /// Returns `Some(packet)` when the complete packet is ready, otherwise
    /// `None`.  Duplicate fragments are silently ignored.
    pub fn insert(&mut self, frame: FragmentFrame) -> Option<Vec<u8>> {
        let buf = self
            .buffers
            .entry(frame.fragment_id)
            .or_insert_with(|| ReassemblyBuffer::new(frame.total_length));

        // If total_length mismatches a previously-seen fragment, ignore.
        if buf.total_length != frame.total_length {
            return None;
        }

        buf.insert(frame.fragment_offset, frame.payload);

        if let Some(packet) = buf.try_reassemble() {
            self.buffers.remove(&frame.fragment_id);
            return Some(packet);
        }

        None
    }

    /// Discard all reassembly buffers that have been waiting longer than the
    /// configured timeout.
    pub fn expire_stale(&mut self) {
        let timeout = self.timeout;
        self.buffers.retain(|_, buf| !buf.is_expired(timeout));
    }

    /// Number of in-progress reassembly buffers currently held.
    pub fn buffer_count(&self) -> usize {
        self.buffers.len()
    }
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fragment_frame::fragment_packet;

    fn big_packet(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i & 0xFF) as u8).collect()
    }

    #[test]
    fn small_packet_single_fragment_reassembled() {
        let data = big_packet(100);
        let frags = fragment_packet(&data, 1);
        assert_eq!(frags.len(), 1);

        let mut r = Reassembler::new();
        let result = r.insert(frags.into_iter().next().unwrap());
        assert_eq!(result.as_deref(), Some(data.as_slice()));
    }

    #[test]
    fn large_packet_in_order_reassembled() {
        let data = big_packet(4000);
        let frags = fragment_packet(&data, 42);
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
        let mut frags = fragment_packet(&data, 7);
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
        let frags = fragment_packet(&data, 99);
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
        let frags = fragment_packet(&data, 5);

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
        let frags = fragment_packet(&data, 3);

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
        let frags_a = fragment_packet(&data_a, 10);
        let frags_b = fragment_packet(&data_b, 20);

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
}
