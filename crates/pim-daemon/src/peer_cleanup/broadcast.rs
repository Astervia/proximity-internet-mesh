//! `state.broadcast_peer_last_seen` cleanup.
//!
//! The daemon's identity-broadcast subsystem keeps a per-peer
//! `Instant` of the last broadcast it accepted, used to enforce
//! `[messaging.broadcast].min_peer_interval_s` rate-limiting. The
//! map is never trimmed elsewhere, so over a long uptime it grows
//! monotonically — every unique peer the node has ever seen
//! broadcast contributes ~32 bytes plus map overhead.
//!
//! This tracker drops entries last touched longer ago than the
//! lifetime threshold. Re-observing a peer after eviction simply
//! reinserts the entry, so dropping is purely a memory-bounding
//! operation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use pim_core::NodeId;
use tokio::sync::Mutex;
use tracing::{debug, info};

use super::PeerCleanupTracker;

type BroadcastMap = Arc<Mutex<HashMap<NodeId, Instant>>>;

pub(crate) struct BroadcastTracker {
    map: BroadcastMap,
}

impl BroadcastTracker {
    pub(crate) fn new(map: BroadcastMap) -> Self {
        Self { map }
    }
}

#[async_trait]
impl PeerCleanupTracker for BroadcastTracker {
    fn name(&self) -> &'static str {
        "broadcast"
    }

    async fn sweep(&self, lifetime_s: i64) -> anyhow::Result<()> {
        let lifetime = Duration::from_secs(lifetime_s.max(0) as u64);
        let mut map = self.map.lock().await;
        let before = map.len();
        map.retain(|_, last_seen| last_seen.elapsed() <= lifetime);
        let evicted = before.saturating_sub(map.len());
        if evicted > 0 {
            info!(
                evicted,
                remaining = map.len(),
                "broadcast cleanup: dropped stale rate-limit entries"
            );
        } else {
            debug!(remaining = map.len(), "broadcast cleanup: nothing to drop");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(byte: u8) -> NodeId {
        NodeId::from_bytes([byte; 16])
    }

    #[tokio::test]
    async fn sweep_keeps_recent_entries_drops_stale() {
        let map: BroadcastMap = Arc::new(Mutex::new(HashMap::new()));
        let now = Instant::now();
        // "Recent" entries (Instant::now()).
        map.lock().await.insert(n(0x11), now);
        // "Stale" entry — pretend it was inserted a long time ago.
        // Instant doesn't let us go backwards directly; subtract a
        // duration via `checked_sub` and tolerate the rare clock
        // monotonicity case where it returns None.
        if let Some(stale_at) = now.checked_sub(Duration::from_secs(7_200)) {
            map.lock().await.insert(n(0x22), stale_at);
        } else {
            // Test environment doesn't have enough monotonic past
            // to cleanly synthesize a stale entry; skip the assert.
            return;
        }

        let tracker = BroadcastTracker::new(map.clone());
        // Lifetime 1 hour: 0x22 should be evicted, 0x11 kept.
        tracker.sweep(3_600).await.unwrap();

        let map = map.lock().await;
        assert!(map.contains_key(&n(0x11)));
        assert!(!map.contains_key(&n(0x22)));
    }
}
