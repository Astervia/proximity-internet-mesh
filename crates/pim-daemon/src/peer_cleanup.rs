//! Centralized peer-cleanup driver.
//!
//! Each peer kind that maintains a TTL-eligible store implements
//! [`PeerCleanupTracker`]. The [`spawn`] function takes a tracker
//! plus a [`PeerCleanupConfig`] and runs a periodic sweep loop with
//! the policy floors applied.
//!
//! See `plans/rfcomm-reconnect/plan.md` for the original motivation
//! and `pim-core::config::PeerCleanupConfig` for the shared schema.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pim_core::PeerCleanupConfig;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

#[cfg(target_os = "linux")]
#[path = "peer_cleanup/bluetooth_pan.rs"]
pub(crate) mod bluetooth_pan;
#[path = "peer_cleanup/broadcast.rs"]
pub(crate) mod broadcast;
#[path = "peer_cleanup/mesh_identity.rs"]
pub(crate) mod mesh_identity;
#[path = "peer_cleanup/reconnect_discovered.rs"]
pub(crate) mod reconnect_discovered;
#[path = "peer_cleanup/rfcomm.rs"]
pub(crate) mod rfcomm;
#[cfg(target_os = "linux")]
#[path = "peer_cleanup/wfd.rs"]
pub(crate) mod wfd;

/// 1 hour. Smallest sane unreachable-lifetime — anything below this
/// is almost certainly a typoed minutes-vs-seconds confusion that
/// would silently delete persistent state within minutes of boot.
pub(crate) const MIN_LIFETIME_S: u64 = 3_600;
/// 60 s. Smallest sane sweep cadence — labs may want shorter, but
/// the production minimum protects against busy-looping the
/// destructive action under a misconfigured interval.
pub(crate) const MIN_INTERVAL_S: u64 = 60;

/// Implemented by each peer kind that owns a store with TTL-based
/// pruning. The [`spawn`] driver calls [`sweep`] on every tick.
#[async_trait]
pub(crate) trait PeerCleanupTracker: Send + Sync + 'static {
    /// Short identifier surfaced in logs (e.g. `"rfcomm"`,
    /// `"mesh-identity"`). Must be a stable string — operators
    /// search for it in journals.
    fn name(&self) -> &'static str;

    /// Run a single sweep with the supplied lifetime threshold (in
    /// unix seconds). Per-peer failures should be logged and the
    /// sweep should keep going; only return `Err` for infrastructure
    /// failures (e.g. shell-out spawn errors) so the driver can log
    /// the tick as failed without panicking.
    async fn sweep(&self, lifetime_s: i64) -> anyhow::Result<()>;
}

/// Spawn a periodic cleanup task for `tracker`. No-ops when
/// `cfg.enabled == false`.
///
/// Both `cleanup_interval_s` and `max_unreachable_lifetime_s` are
/// clamped to their respective floors at spawn time so the task can
/// never run faster than [`MIN_INTERVAL_S`] or with a horizon shorter
/// than [`MIN_LIFETIME_S`] regardless of TOML mistakes.
pub(crate) fn spawn(
    cfg: PeerCleanupConfig,
    tracker: Arc<dyn PeerCleanupTracker>,
    cancel: CancellationToken,
) {
    if !cfg.enabled {
        debug!(
            tracker = %tracker.name(),
            "peer cleanup disabled by config"
        );
        return;
    }
    let interval = Duration::from_secs(cfg.cleanup_interval_s.max(MIN_INTERVAL_S));
    let lifetime_s = cfg.max_unreachable_lifetime_s.max(MIN_LIFETIME_S) as i64;
    let name = tracker.name();
    tokio::spawn(async move {
        info!(
            tracker = %name,
            interval_s = interval.as_secs(),
            lifetime_s = lifetime_s,
            "peer cleanup task started"
        );
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!(tracker = %name, "peer cleanup shutdown");
                    return;
                }
                _ = tokio::time::sleep(interval) => {}
            }
            if let Err(e) = tracker.sweep(lifetime_s).await {
                warn!(tracker = %name, "peer cleanup tick failed: {e}");
            }
        }
    });
}

/// Wall-clock seconds, used by trackers to compute peer ages.
pub(crate) fn unix_seconds_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod driver_tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct CountingTracker {
        ticks: Arc<AtomicU32>,
    }

    #[async_trait]
    impl PeerCleanupTracker for CountingTracker {
        fn name(&self) -> &'static str {
            "counting"
        }
        async fn sweep(&self, _lifetime_s: i64) -> anyhow::Result<()> {
            self.ticks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn spawn_returns_immediately_when_disabled() {
        let cfg = PeerCleanupConfig {
            enabled: false,
            max_unreachable_lifetime_s: 60,
            cleanup_interval_s: 1,
        };
        let ticks = Arc::new(AtomicU32::new(0));
        let tracker: Arc<dyn PeerCleanupTracker> = Arc::new(CountingTracker {
            ticks: ticks.clone(),
        });
        let cancel = CancellationToken::new();
        // No tokio runtime needed — spawn() returns without
        // creating a task when enabled = false.
        spawn(cfg, tracker, cancel);
        assert_eq!(ticks.load(Ordering::SeqCst), 0);
    }
}
