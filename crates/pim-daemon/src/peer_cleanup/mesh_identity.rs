//! Mesh-identity (`peers_seen`) cleanup. Drops the row + emits a
//! `peer_forgotten` broadcast event, which the messaging plugin
//! reacts to by wiping per-peer message history.
//!
//! Skips peers currently in `state.sessions` — losing the X25519 key
//! mid-session would cause the next route advertisement signature
//! verification to fail.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use pim_core::NodeId;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use super::{unix_seconds_now, PeerCleanupTracker};
use crate::app::peer_directory::PeerDirectoryService;
use crate::app::session::Session;

type SessionMap = Arc<RwLock<HashMap<NodeId, Arc<Session>>>>;

pub(crate) struct MeshIdentityTracker {
    directory: Arc<PeerDirectoryService>,
    sessions: SessionMap,
}

impl MeshIdentityTracker {
    pub(crate) fn new(directory: Arc<PeerDirectoryService>, sessions: SessionMap) -> Self {
        Self {
            directory,
            sessions,
        }
    }
}

#[async_trait]
impl PeerCleanupTracker for MeshIdentityTracker {
    fn name(&self) -> &'static str {
        "mesh-identity"
    }

    async fn sweep(&self, lifetime_s: i64) -> anyhow::Result<()> {
        let threshold_ms = unix_seconds_now()
            .saturating_sub(lifetime_s)
            .saturating_mul(1_000);

        let directory_for_list = self.directory.clone();
        let stale = tokio::task::spawn_blocking(move || {
            directory_for_list.list_peers_older_than(threshold_ms)
        })
        .await??;

        if stale.is_empty() {
            debug!("mesh-identity cleanup: no stale rows");
            return Ok(());
        }

        // Snapshot the active session set once per sweep. The map
        // can change underneath us mid-iteration but a peer in an
        // active session should be skipped — at worst we'll forget
        // a peer whose session opened in the same tick (reconnect
        // is cheap).
        let active: std::collections::HashSet<NodeId> =
            self.sessions.read().await.keys().copied().collect();

        for (peer, last_seen_ms, name) in stale {
            if active.contains(&peer) {
                continue;
            }
            match self.directory.forget(peer).await {
                Ok(outcome) if outcome.forgot_identity => {
                    info!(
                        peer_node_id = %peer,
                        name = %name,
                        last_seen_ms = last_seen_ms,
                        age_days = (unix_seconds_now() - last_seen_ms / 1_000) / 86_400,
                        "mesh-identity cleanup: forgot stale peer"
                    );
                }
                Ok(_) => {
                    // Row vanished between list + forget — race with
                    // a concurrent `peers.forget` RPC. Benign.
                }
                Err(e) => {
                    warn!(
                        peer_node_id = %peer,
                        "mesh-identity cleanup: forget failed: {e}"
                    );
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pim_plugin::PeerInfoSource;
    use std::sync::Arc;

    fn open_temp_directory() -> Arc<PeerDirectoryService> {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("peers.db");
        std::mem::forget(dir);
        Arc::new(PeerDirectoryService::open(path).expect("open peers directory"))
    }

    fn empty_sessions() -> SessionMap {
        Arc::new(RwLock::new(HashMap::new()))
    }

    fn n(byte: u8) -> NodeId {
        NodeId::from_bytes([byte; 16])
    }

    async fn seed_peer(
        directory: &Arc<PeerDirectoryService>,
        peer: NodeId,
        x25519_pub: [u8; 32],
        name: &str,
        last_seen_ms: i64,
    ) {
        directory
            .record_peer_seen(
                peer,
                x25519_pub,
                name.to_string(),
                last_seen_ms,
                PeerInfoSource::Direct,
                false,
            )
            .await
            .expect("seed peer");
    }

    fn directory_contains(directory: &PeerDirectoryService, peer: NodeId) -> bool {
        directory
            .list_peers_older_than(i64::MAX)
            .unwrap()
            .into_iter()
            .any(|(id, _, _)| id == peer)
    }

    #[tokio::test]
    async fn sweep_keeps_recently_seen_peer() {
        let directory = open_temp_directory();
        let now_ms = unix_seconds_now() * 1_000;
        // Recent: well within the 1-hour lifetime floor.
        seed_peer(&directory, n(0x11), [1u8; 32], "alice", now_ms - 60_000).await;
        let tracker = MeshIdentityTracker::new(directory.clone(), empty_sessions());
        tracker.sweep(3_600).await.unwrap();
        assert!(directory_contains(&directory, n(0x11)));
    }

    #[tokio::test]
    async fn sweep_forgets_stale_peer() {
        let directory = open_temp_directory();
        let now_ms = unix_seconds_now() * 1_000;
        // Stale: last seen 2 hours ago, lifetime 1 hour.
        seed_peer(&directory, n(0x22), [2u8; 32], "bob", now_ms - 7_200_000).await;
        let tracker = MeshIdentityTracker::new(directory.clone(), empty_sessions());
        tracker.sweep(3_600).await.unwrap();
        assert!(!directory_contains(&directory, n(0x22)));
    }

    #[tokio::test]
    async fn sweep_emits_forgotten_event() {
        let directory = open_temp_directory();
        let mut events = directory.subscribe();
        let now_ms = unix_seconds_now() * 1_000;
        seed_peer(&directory, n(0x44), [4u8; 32], "dave", now_ms - 7_200_000).await;
        let tracker = MeshIdentityTracker::new(directory.clone(), empty_sessions());
        tracker.sweep(3_600).await.unwrap();
        // First poll should yield the Forgotten event the cleanup
        // emitted; messaging plugin subscribers depend on this for
        // history wipe.
        let evt = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
            .await
            .expect("event arrives within 1 s")
            .expect("recv");
        match evt {
            pim_plugin::PeerDirectoryEvent::Forgotten { node_id } => assert_eq!(node_id, n(0x44)),
            other => panic!("expected Forgotten, got {other:?}"),
        }
    }
}
