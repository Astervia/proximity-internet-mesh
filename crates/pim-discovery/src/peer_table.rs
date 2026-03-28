//! In-memory peer table: tracks known peers discovered on the mesh.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use pim_core::NodeId;

use crate::advertisement::NodeCapabilities;

// ── PeerRecord ────────────────────────────────────────────────────────────────

/// A single peer known to this node.
#[derive(Debug, Clone)]
pub struct PeerRecord {
    pub node_id: NodeId,
    /// Ed25519 public key.
    pub public_key: [u8; 32],
    pub capabilities: NodeCapabilities,
    /// Reachable transport address (derived from UDP source IP + advertised port).
    pub listen_addr: SocketAddr,
    /// When this peer was last heard from.
    pub last_seen: Instant,
}

impl PeerRecord {
    pub fn is_stale(&self, max_age: Duration) -> bool {
        self.last_seen.elapsed() > max_age
    }
}

// ── PeerTable ─────────────────────────────────────────────────────────────────

/// Tracks discovered peers.  **Not** thread-safe — callers must synchronise
/// (e.g., wrap in `tokio::sync::Mutex`).
#[derive(Debug, Default)]
pub struct PeerTable {
    peers: HashMap<NodeId, PeerRecord>,
}

impl PeerTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or refresh a peer.  Returns `true` if this is a newly discovered
    /// peer (not just a refresh).
    pub fn upsert(&mut self, record: PeerRecord) -> bool {
        let is_new = !self.peers.contains_key(&record.node_id);
        self.peers.insert(record.node_id, record);
        is_new
    }

    /// Remove a peer by NodeId.  Returns the record if it existed.
    pub fn remove(&mut self, node_id: &NodeId) -> Option<PeerRecord> {
        self.peers.remove(node_id)
    }

    /// Look up a peer.
    pub fn get(&self, node_id: &NodeId) -> Option<&PeerRecord> {
        self.peers.get(node_id)
    }

    /// Returns all known peers.
    pub fn all(&self) -> impl Iterator<Item = &PeerRecord> {
        self.peers.values()
    }

    /// Number of tracked peers.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Remove peers not heard from within `max_age`.
    /// Returns the NodeIds that were removed.
    pub fn expire_stale(&mut self, max_age: Duration) -> Vec<NodeId> {
        let stale: Vec<NodeId> = self
            .peers
            .values()
            .filter(|p| p.is_stale(max_age))
            .map(|p| p.node_id)
            .collect();
        for id in &stale {
            self.peers.remove(id);
        }
        stale
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn make_record(n: u8) -> PeerRecord {
        PeerRecord {
            node_id: NodeId::from_bytes([n; 16]),
            public_key: [n; 32],
            capabilities: NodeCapabilities::client(),
            listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, n)), 9100),
            last_seen: Instant::now(),
        }
    }

    #[test]
    fn upsert_returns_true_for_new_peer() {
        let mut table = PeerTable::new();
        assert!(table.upsert(make_record(1)));
    }

    #[test]
    fn upsert_returns_false_for_refresh() {
        let mut table = PeerTable::new();
        table.upsert(make_record(1));
        assert!(!table.upsert(make_record(1)));
    }

    #[test]
    fn remove_extracts_peer() {
        let mut table = PeerTable::new();
        table.upsert(make_record(2));
        let id = NodeId::from_bytes([2; 16]);
        assert!(table.remove(&id).is_some());
        assert!(table.get(&id).is_none());
    }

    #[test]
    fn len_tracks_correctly() {
        let mut table = PeerTable::new();
        assert_eq!(table.len(), 0);
        table.upsert(make_record(1));
        table.upsert(make_record(2));
        assert_eq!(table.len(), 2);
        table.upsert(make_record(1)); // refresh
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn expire_stale_removes_old_peers() {
        let mut table = PeerTable::new();
        // Insert a peer with an artificially old last_seen
        let mut old = make_record(5);
        old.last_seen = Instant::now() - Duration::from_secs(60);
        table.upsert(old);
        table.upsert(make_record(6)); // fresh

        let removed = table.expire_stale(Duration::from_secs(30));
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0], NodeId::from_bytes([5; 16]));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn expire_stale_keeps_fresh_peers() {
        let mut table = PeerTable::new();
        table.upsert(make_record(1));
        table.upsert(make_record(2));
        let removed = table.expire_stale(Duration::from_secs(60));
        assert!(removed.is_empty());
        assert_eq!(table.len(), 2);
    }
}
