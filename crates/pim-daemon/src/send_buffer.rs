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
mod tests {
    use super::*;

    fn node(b: u8) -> NodeId {
        NodeId::from_bytes([b; 16])
    }

    fn mk_frame(ft: FrameType) -> TransportFrame {
        TransportFrame {
            frame_type: ft,
            nonce: [0; 12],
            payload: vec![],
            tag: [0; 16],
        }
    }

    fn ctrl() -> TransportFrame {
        mk_frame(FrameType::Control)
    }
    fn route() -> TransportFrame {
        mk_frame(FrameType::RouteUpdate)
    }
    fn data() -> TransportFrame {
        mk_frame(FrameType::Data)
    }

    // ── Priority ──────────────────────────────────────────────────────────────

    #[test]
    fn priority_ordering_correct() {
        assert!(Priority::Control < Priority::Route);
        assert!(Priority::Route < Priority::Data);
        assert!(Priority::Control < Priority::Data);
    }

    #[test]
    fn priority_classified_by_frame_type() {
        assert_eq!(Priority::of(FrameType::Control), Priority::Control);
        assert_eq!(Priority::of(FrameType::Handshake), Priority::Control);
        assert_eq!(Priority::of(FrameType::RouteUpdate), Priority::Route);
        assert_eq!(Priority::of(FrameType::Data), Priority::Data);
        assert_eq!(Priority::of(FrameType::Heartbeat), Priority::Data);
    }

    // ── PeerBuffer ────────────────────────────────────────────────────────────

    #[test]
    fn peer_buffer_drain_returns_highest_priority_first() {
        let mut buf = PeerBuffer::new(16, Duration::from_secs(60));
        buf.push(Priority::Data, data());
        buf.push(Priority::Route, route());
        buf.push(Priority::Control, ctrl());
        let frames = buf.drain();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].frame_type, FrameType::Control);
        assert_eq!(frames[1].frame_type, FrameType::RouteUpdate);
        assert_eq!(frames[2].frame_type, FrameType::Data);
    }

    #[test]
    fn peer_buffer_same_priority_is_fifo() {
        let mut buf = PeerBuffer::new(16, Duration::from_secs(60));
        // Use payload to distinguish frames
        let mut f1 = data();
        f1.payload = vec![1];
        let mut f2 = data();
        f2.payload = vec![2];
        buf.push(Priority::Data, f1);
        buf.push(Priority::Data, f2);
        let frames = buf.drain();
        assert_eq!(frames[0].payload, vec![1]);
        assert_eq!(frames[1].payload, vec![2]);
    }

    #[test]
    fn peer_buffer_overflow_drops_incoming_lower_priority() {
        let mut buf = PeerBuffer::new(2, Duration::from_secs(60));
        buf.push(Priority::Control, ctrl());
        buf.push(Priority::Route, route());
        // At capacity; incoming Data is lower priority than both → dropped
        buf.push(Priority::Data, data());
        assert_eq!(buf.len(), 2, "buffer should stay at capacity");
        let frames = buf.drain();
        assert!(frames.iter().all(|f| f.frame_type != FrameType::Data));
    }

    #[test]
    fn peer_buffer_overflow_evicts_worst_for_higher_priority_incoming() {
        let mut buf = PeerBuffer::new(2, Duration::from_secs(60));
        buf.push(Priority::Route, route());
        buf.push(Priority::Data, data());
        // At capacity; incoming Control evicts Data (lowest priority)
        buf.push(Priority::Control, ctrl());
        assert_eq!(buf.len(), 2);
        let frames = buf.drain();
        assert!(frames.iter().any(|f| f.frame_type == FrameType::Control));
        assert!(frames
            .iter()
            .any(|f| f.frame_type == FrameType::RouteUpdate));
        assert!(!frames.iter().any(|f| f.frame_type == FrameType::Data));
    }

    #[test]
    fn peer_buffer_expire_removes_stale_frames() {
        let mut buf = PeerBuffer::new(16, Duration::from_millis(1));
        buf.push(Priority::Data, data());
        std::thread::sleep(Duration::from_millis(10));
        let removed = buf.expire();
        assert_eq!(removed, 1);
        assert!(buf.is_empty());
    }

    #[test]
    fn peer_buffer_drain_filters_expired() {
        let mut buf = PeerBuffer::new(16, Duration::from_millis(1));
        buf.push(Priority::Control, ctrl());
        std::thread::sleep(Duration::from_millis(10));
        let frames = buf.drain();
        assert!(frames.is_empty(), "expired frame must not be delivered");
    }

    // ── SendBuffer ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn send_buffer_push_and_drain_priority_order() {
        let sb = SendBuffer::new(16, Duration::from_secs(60));
        let peer = node(1);
        sb.push(peer, Priority::Data, data()).await;
        sb.push(peer, Priority::Control, ctrl()).await;
        sb.push(peer, Priority::Route, route()).await;
        let frames = sb.drain(&peer).await;
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].frame_type, FrameType::Control);
        assert_eq!(frames[1].frame_type, FrameType::RouteUpdate);
        assert_eq!(frames[2].frame_type, FrameType::Data);
    }

    #[tokio::test]
    async fn send_buffer_drain_removes_peer_entry() {
        let sb = SendBuffer::new(16, Duration::from_secs(60));
        let peer = node(1);
        sb.push(peer, Priority::Data, data()).await;
        sb.drain(&peer).await;
        assert_eq!(
            sb.peer_count().await,
            0,
            "peer entry should be removed after drain"
        );
    }

    #[tokio::test]
    async fn send_buffer_drain_unknown_peer_returns_empty() {
        let sb = SendBuffer::new(16, Duration::from_secs(60));
        let frames = sb.drain(&node(99)).await;
        assert!(frames.is_empty());
    }

    #[tokio::test]
    async fn send_buffer_expire_all_clears_stale() {
        let sb = SendBuffer::new(16, Duration::from_millis(1));
        sb.push(node(1), Priority::Data, data()).await;
        sb.push(node(2), Priority::Control, ctrl()).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        let removed = sb.expire_all().await;
        assert_eq!(removed, 2);
        assert_eq!(sb.peer_count().await, 0);
    }

    #[tokio::test]
    async fn send_buffer_multiple_peers_independent() {
        let sb = SendBuffer::new(16, Duration::from_secs(60));
        sb.push(node(1), Priority::Data, data()).await;
        sb.push(node(2), Priority::Control, ctrl()).await;
        assert_eq!(sb.frame_count(&node(1)).await, 1);
        assert_eq!(sb.frame_count(&node(2)).await, 1);
        sb.drain(&node(1)).await;
        assert_eq!(sb.frame_count(&node(1)).await, 0);
        assert_eq!(sb.frame_count(&node(2)).await, 1);
    }

    #[tokio::test]
    async fn send_buffer_respects_capacity_per_peer() {
        let sb = SendBuffer::new(2, Duration::from_secs(60));
        let peer = node(1);
        sb.push(peer, Priority::Control, ctrl()).await;
        sb.push(peer, Priority::Route, route()).await;
        sb.push(peer, Priority::Data, data()).await; // should be dropped (lowest)
        assert_eq!(sb.frame_count(&peer).await, 2);
    }
}
