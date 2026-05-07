use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use pim_core::NodeId;
use rand::Rng as _;
use tokio::sync::Mutex;

/// Tracks configured peer addresses and drives exponential-backoff reconnects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ConnectTarget {
    Tcp(SocketAddr),
    BluetoothPan(SocketAddr),
}

impl ConnectTarget {
    pub(crate) fn addr(self) -> SocketAddr {
        match self {
            Self::Tcp(addr) | Self::BluetoothPan(addr) => addr,
        }
    }

    pub(crate) fn mechanism_name(self) -> &'static str {
        match self {
            Self::Tcp(_) => "tcp",
            Self::BluetoothPan(_) => "bluetooth_pan",
        }
    }
}

pub(crate) struct ReconnectManager {
    /// Configured peer targets from `[[peers]]` config: always reconnect if lost.
    configured_targets: HashSet<ConnectTarget>,
    /// Targets learned from dynamic discovery: also reconnect if lost.
    /// Each entry tracks when it was last (re-)observed so the
    /// `[transport.peer_cleanup]` sweep can drop entries that haven't
    /// been refreshed in a long time. Without this the set grows
    /// monotonically over the lifetime of the daemon.
    discovered_targets: Mutex<HashMap<ConnectTarget, Instant>>,
    /// Maps real peer NodeId to connection target, learned after handshake.
    target_by_peer: Mutex<HashMap<NodeId, ConnectTarget>>,
    /// Addresses that currently have an active reconnect task.
    reconnecting: Mutex<HashSet<ConnectTarget>>,
}

impl ReconnectManager {
    pub(crate) fn new(targets: impl IntoIterator<Item = ConnectTarget>) -> Self {
        Self {
            configured_targets: targets.into_iter().collect(),
            discovered_targets: Mutex::new(HashMap::new()),
            target_by_peer: Mutex::new(HashMap::new()),
            reconnecting: Mutex::new(HashSet::new()),
        }
    }

    /// Record `peer_id -> target` after a successful handshake.
    pub(crate) async fn register(&self, peer_id: NodeId, target: ConnectTarget) {
        self.target_by_peer.lock().await.insert(peer_id, target);
    }

    /// Return the configured target for `peer_id`, if it is a configured peer.
    #[cfg(test)]
    pub(crate) async fn configured_target(&self, peer_id: &NodeId) -> Option<ConnectTarget> {
        let target = self.target_by_peer.lock().await.get(peer_id).copied()?;
        self.configured_targets.contains(&target).then_some(target)
    }

    /// Register a target that came from dynamic peer discovery.
    /// Refreshes the `last_seen` timestamp so a target that keeps
    /// being re-observed never ages out.
    pub(crate) async fn register_discovered(&self, target: ConnectTarget) {
        self.discovered_targets
            .lock()
            .await
            .insert(target, Instant::now());
    }

    /// Drop discovered targets last observed more than `lifetime`
    /// ago. Returns the number of evicted targets so the cleanup
    /// driver can log it.
    pub(crate) async fn expire_discovered_targets(&self, lifetime: Duration) -> usize {
        let mut map = self.discovered_targets.lock().await;
        let before = map.len();
        map.retain(|_, last_seen| last_seen.elapsed() <= lifetime);
        before.saturating_sub(map.len())
    }

    /// Return the target for `peer_id` if it is either a configured or
    /// discovered peer. Both should be reconnected on loss.
    pub(crate) async fn is_reconnectable_target(&self, peer_id: &NodeId) -> Option<ConnectTarget> {
        let target = self.target_by_peer.lock().await.get(peer_id).copied()?;
        let is_configured = self.configured_targets.contains(&target);
        let is_discovered = self.discovered_targets.lock().await.contains_key(&target);
        (is_configured || is_discovered).then_some(target)
    }

    /// Return snapshot-friendly metadata for a connected peer.
    pub(crate) async fn peer_info(&self, peer_id: &NodeId) -> Option<(ConnectTarget, bool, bool)> {
        let target = self.target_by_peer.lock().await.get(peer_id).copied()?;
        let configured = self.configured_targets.contains(&target);
        let discovered = self.discovered_targets.lock().await.contains_key(&target);
        Some((target, configured, discovered))
    }

    /// Claim the reconnect slot for `target`.
    ///
    /// Returns `true` if a new reconnect task should be spawned, meaning none
    /// was already running.
    pub(crate) async fn begin_reconnect(&self, target: ConnectTarget) -> bool {
        self.reconnecting.lock().await.insert(target)
    }

    /// Release the reconnect slot when the task finishes, successfully or not.
    pub(crate) async fn end_reconnect(&self, target: ConnectTarget) {
        self.reconnecting.lock().await.remove(&target);
    }

    /// Find the peer ID most recently associated with `target` via a successful
    /// handshake, if any. Used to detect whether a session is already live.
    pub(crate) async fn peer_id_for_target(&self, target: &ConnectTarget) -> Option<NodeId> {
        let by_peer = self.target_by_peer.lock().await;
        by_peer
            .iter()
            .find(|(_, t)| *t == target)
            .map(|(id, _)| *id)
    }
}

/// Base delay in ms for `attempt` without jitter: 1 s * 2^attempt, capped at 10 s.
pub(crate) fn backoff_base_ms(attempt: u32) -> u64 {
    const BASE_MS: u64 = 1_000;
    const MAX_MS: u64 = 10_000;
    let shift = attempt.min(15) as u64;
    BASE_MS.saturating_mul(1u64 << shift).min(MAX_MS)
}

/// Exponential backoff with +/-25% uniform jitter.
pub(crate) fn backoff_duration(attempt: u32) -> Duration {
    let base = backoff_base_ms(attempt);
    let jitter_range = (base / 4) as i64;
    let jitter = rand::thread_rng().gen_range(-jitter_range..=jitter_range);
    Duration::from_millis((base as i64 + jitter).max(1) as u64)
}
