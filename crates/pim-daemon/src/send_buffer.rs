//! Store-and-forward send buffer (Phase 4.2).
//!
//! When a peer is temporarily unreachable, outbound frames are held in a
//! bounded priority queue instead of being dropped.  When the peer reconnects
//! the buffer is drained and frames are delivered in priority order:
//!
//! 1. **Control** (`FrameType::Control`, `FrameType::Handshake`) — highest
//! 2. **Route** (`FrameType::RouteUpdate`)
//! 3. **Data** (everything else) — lowest
//!
//! On overflow the lowest-priority frame is evicted first.  Frames that have
//! been waiting longer than `timeout` are silently dropped on access.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use pim_core::NodeId;
use pim_protocol::{FrameType, TransportFrame};
use tokio::sync::Mutex;

// ── Priority ──────────────────────────────────────────────────────────────────

/// Priority level for buffered frames (lower value = delivered first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Control = 0,
    Route = 1,
    Data = 2,
}

impl Priority {
    /// Classify a frame by its `FrameType`.
    pub fn of(frame_type: FrameType) -> Self {
        match frame_type {
            FrameType::Control | FrameType::Handshake => Priority::Control,
            FrameType::RouteUpdate => Priority::Route,
            _ => Priority::Data,
        }
    }
}

// ── Constants ─────────────────────────────────────────────────────────────────

/// Default per-peer buffer capacity in frames.
pub const DEFAULT_CAPACITY: usize = 256;
/// Default frame timeout before a buffered frame is discarded.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

// ── PeerBuffer ────────────────────────────────────────────────────────────────

struct BufferedFrame {
    enqueued_at: Instant,
    priority: Priority,
    frame: TransportFrame,
}

/// Bounded per-peer send buffer, sorted by ascending priority value (highest
/// importance first in the storage order for efficient drain).
struct PeerBuffer {
    /// Frames sorted ascending by priority then by enqueue time (FIFO within
    /// the same priority level).  Index 0 = highest priority.
    frames: Vec<BufferedFrame>,
    capacity: usize,
    timeout: Duration,
}

impl PeerBuffer {
    fn new(capacity: usize, timeout: Duration) -> Self {
        Self {
            frames: Vec::with_capacity(capacity.min(64)),
            capacity,
            timeout,
        }
    }

    fn push(&mut self, priority: Priority, frame: TransportFrame) {
        // Evict already-expired frames before touching capacity.
        let now = Instant::now();
        self.frames
            .retain(|f| now.duration_since(f.enqueued_at) < self.timeout);

        // At capacity: attempt to evict the lowest-priority (last) frame.
        if self.frames.len() >= self.capacity {
            let worst = self.frames.last().map(|f| f.priority);
            match worst {
                Some(w) if w >= priority => {
                    self.frames.pop();
                }
                _ => return, // incoming is lower priority than everything buffered; drop it
            }
        }

        // Insert so the Vec remains sorted ascending by (priority, enqueued_at).
        let bf = BufferedFrame {
            enqueued_at: Instant::now(),
            priority,
            frame,
        };
        let pos = self.frames.partition_point(|f| {
            f.priority < bf.priority
                || (f.priority == bf.priority && f.enqueued_at <= bf.enqueued_at)
        });
        self.frames.insert(pos, bf);
    }

    /// Drain all non-expired frames, highest priority first.
    fn drain(&mut self) -> Vec<TransportFrame> {
        let now = Instant::now();
        let timeout = self.timeout;
        self.frames
            .drain(..)
            .filter(|f| now.duration_since(f.enqueued_at) < timeout)
            .map(|f| f.frame)
            .collect()
    }

    /// Remove expired frames.  Returns the count removed.
    fn expire(&mut self) -> usize {
        let before = self.frames.len();
        let now = Instant::now();
        let timeout = self.timeout;
        self.frames
            .retain(|f| now.duration_since(f.enqueued_at) < timeout);
        before - self.frames.len()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn len(&self) -> usize {
        self.frames.len()
    }

    fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

// ── SendBuffer ────────────────────────────────────────────────────────────────

/// Global store-and-forward buffer: one [`PeerBuffer`] per destination peer.
pub struct SendBuffer {
    peers: Mutex<HashMap<NodeId, PeerBuffer>>,
    capacity_per_peer: usize,
    /// Maximum age before a buffered frame is discarded.
    pub timeout: Duration,
}

impl SendBuffer {
    pub fn new(capacity_per_peer: usize, timeout: Duration) -> Self {
        Self {
            peers: Mutex::new(HashMap::new()),
            capacity_per_peer,
            timeout,
        }
    }

    /// Buffer `frame` for `peer_id` with the given `priority`.
    pub async fn push(&self, peer_id: NodeId, priority: Priority, frame: TransportFrame) {
        self.peers
            .lock()
            .await
            .entry(peer_id)
            .or_insert_with(|| PeerBuffer::new(self.capacity_per_peer, self.timeout))
            .push(priority, frame);
    }

    /// Drain all non-expired frames for `peer_id`, highest priority first.
    /// Removes the peer's buffer entry when empty.
    pub async fn drain(&self, peer_id: &NodeId) -> Vec<TransportFrame> {
        let mut peers = self.peers.lock().await;
        let Some(buf) = peers.get_mut(peer_id) else {
            return vec![];
        };
        let frames = buf.drain();
        if buf.is_empty() {
            peers.remove(peer_id);
        }
        frames
    }

    /// Expire stale frames across all peer buffers.  Returns total removed.
    pub async fn expire_all(&self) -> usize {
        let mut peers = self.peers.lock().await;
        let total: usize = peers.values_mut().map(|b| b.expire()).sum();
        peers.retain(|_, b| !b.is_empty());
        total
    }

    /// Number of peers that currently have buffered frames.
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn peer_count(&self) -> usize {
        self.peers.lock().await.len()
    }

    /// Number of buffered frames for a specific peer.
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn frame_count(&self, peer_id: &NodeId) -> usize {
        self.peers
            .lock()
            .await
            .get(peer_id)
            .map(|b| b.len())
            .unwrap_or(0)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "send_buffer/tests.rs"]
mod tests;
