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
        // PERFORMANCE: Pre-validate that all chunks are present before
        // allocating the target vector. If we allocate unconditionally,
        // every fragment arrival costs a redundant `Vec` allocation
        // equal to `total_length` that gets immediately discarded.
        let mut covered_up_to = 0usize;
        let total = self.total_length as usize;

        for (&offset, chunk) in &self.chunks {
            let start = offset as usize;
            if start > covered_up_to {
                // There is a gap — not complete yet.
                return None;
            }
            let end = start + chunk.len();
            if end > total {
                // Chunk extends past declared total — malformed, bail out.
                return None;
            }
            if end > covered_up_to {
                covered_up_to = end;
            }
        }

        if covered_up_to != total {
            return None;
        }

        // All bytes present. Perform the final allocation and copy.
        let mut buf = vec![0u8; total];
        for (&offset, chunk) in &self.chunks {
            let start = offset as usize;
            let end = start + chunk.len();
            buf[start..end].copy_from_slice(chunk);
        }

        Some(buf)
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
mod tests;
