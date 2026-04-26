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
    /// Stable identifier of the discovered peer.
    pub node_id: NodeId,
    /// Ed25519 public key.
    pub public_key: [u8; 32],
    /// Roles advertised by the peer.
    pub capabilities: NodeCapabilities,
    /// Reachable transport address (derived from UDP source IP + advertised port).
    pub listen_addr: SocketAddr,
    /// When this peer was last heard from.
    pub last_seen: Instant,
}

impl PeerRecord {
    /// Return `true` if the peer has not been refreshed within `max_age`.
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
    /// Create an empty peer table.
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

    /// Return `true` if no peers are currently tracked.
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
mod tests;
