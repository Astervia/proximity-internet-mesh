//! Peer reputation and automatic blacklisting.
//!
//! Each peer starts with a score of 0.  Failures (heartbeat timeout, invalid
//! frames) increment the score; successful interactions (Pong response) decrement
//! it.  When the score reaches `BLACKLIST_THRESHOLD` the peer is considered
//! malicious and is blacklisted in the routing table.

use std::collections::HashMap;

use pim_core::NodeId;

/// Score at which a peer is considered malicious and blacklisted.
pub const BLACKLIST_THRESHOLD: u32 = 10;

pub struct ReputationTracker {
    scores: HashMap<NodeId, u32>,
}

impl ReputationTracker {
    pub fn new() -> Self {
        Self { scores: HashMap::new() }
    }

    /// Record a failure for `peer_id`.
    ///
    /// Returns `true` the first time the peer's score reaches
    /// `BLACKLIST_THRESHOLD` (i.e., the peer is *newly* blacklisted).
    pub fn record_failure(&mut self, peer_id: NodeId) -> bool {
        let score = self.scores.entry(peer_id).or_insert(0);
        *score = score.saturating_add(1);
        *score == BLACKLIST_THRESHOLD
    }

    /// Record a successful interaction, decrementing the score (floor 0).
    pub fn record_success(&mut self, peer_id: NodeId) {
        if let Some(score) = self.scores.get_mut(&peer_id) {
            *score = score.saturating_sub(1);
        }
    }

    /// Returns `true` if `peer_id` is at or above the blacklist threshold.
    #[allow(dead_code)]
    pub fn is_blacklisted(&self, peer_id: &NodeId) -> bool {
        self.scores.get(peer_id).copied().unwrap_or(0) >= BLACKLIST_THRESHOLD
    }

    /// Pardon a peer by removing all accumulated score.
    #[allow(dead_code)]
    pub fn pardon(&mut self, peer_id: &NodeId) {
        self.scores.remove(peer_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pim_core::NodeId;

    fn peer(b: u8) -> NodeId {
        NodeId::from_bytes([b; 16])
    }

    #[test]
    fn failure_accumulation_reaches_threshold() {
        let mut rt = ReputationTracker::new();
        let p = peer(1);
        for i in 1..BLACKLIST_THRESHOLD {
            assert!(!rt.record_failure(p), "failure {i} should not newly blacklist");
        }
        assert!(rt.record_failure(p), "final failure should newly blacklist");
    }

    #[test]
    fn blacklist_threshold_detected() {
        let mut rt = ReputationTracker::new();
        let p = peer(2);
        for _ in 0..BLACKLIST_THRESHOLD {
            rt.record_failure(p);
        }
        assert!(rt.is_blacklisted(&p));
    }

    #[test]
    fn success_decrements_score() {
        let mut rt = ReputationTracker::new();
        let p = peer(3);
        for _ in 0..5 {
            rt.record_failure(p);
        }
        rt.record_success(p);
        assert!(!rt.is_blacklisted(&p));
        // Score should be 4 — not yet at threshold
        for _ in 0..(BLACKLIST_THRESHOLD - 4 - 1) {
            rt.record_failure(p);
        }
        assert!(!rt.is_blacklisted(&p));
    }

    #[test]
    fn success_floor_is_zero() {
        let mut rt = ReputationTracker::new();
        let p = peer(4);
        // record_success on unknown peer should not panic or underflow
        rt.record_success(p);
        rt.record_success(p);
        assert!(!rt.is_blacklisted(&p));
    }

    #[test]
    fn pardon_resets_score() {
        let mut rt = ReputationTracker::new();
        let p = peer(5);
        for _ in 0..BLACKLIST_THRESHOLD {
            rt.record_failure(p);
        }
        assert!(rt.is_blacklisted(&p));
        rt.pardon(&p);
        assert!(!rt.is_blacklisted(&p));
    }
}
