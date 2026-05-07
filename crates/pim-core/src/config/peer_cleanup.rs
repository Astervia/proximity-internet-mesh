//! Centralized peer-lifetime / cleanup configuration shared by every
//! subsystem that maintains a peer-keyed store needing TTL-based
//! pruning.
//!
//! See `plans/rfcomm-reconnect/plan.md` for the original RFCOMM-focused
//! motivation and the decision to centralize.
//!
//! Each owning section embeds a `PeerCleanupConfig` as a TOML
//! subsection (`[<owner>.peer_cleanup]`). The schema is identical
//! across owners — only the defaults differ, because the right horizon
//! for "Bluetooth peer unreachable" (hours) is very different from
//! "mesh identity unseen" (months).
//!
//! ```toml
//! [bluetooth_rfcomm.peer_cleanup]
//! enabled = true
//! max_unreachable_lifetime_s = 7200
//! cleanup_interval_s = 3600
//! ```

use serde::{Deserialize, Serialize};

/// Shared shape of every peer-cleanup subsection. Parent configs
/// embed an instance and provide their own [`Default`] so the
/// horizon and cadence are tuned to the kind of peer being tracked.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerCleanupConfig {
    /// Enable the periodic cleanup task for this owner. When `false`
    /// no destructive action is taken regardless of the other
    /// fields.
    pub enabled: bool,
    /// Threshold past which a peer is considered unreachable and
    /// eligible for the destructive cleanup action. Daemon clamps
    /// values below `MIN_LIFETIME_S` (1 hour) at runtime.
    pub max_unreachable_lifetime_s: u64,
    /// Interval between cleanup sweeps. Daemon clamps values below
    /// `MIN_INTERVAL_S` (60 seconds) at runtime.
    pub cleanup_interval_s: u64,
}

impl PeerCleanupConfig {
    /// Bluetooth-flavoured default: cleanup ON with a 2 h horizon and
    /// 1 h sweep. Tuned for paired BT devices that come and go on
    /// daily timescales.
    pub fn bluetooth_default() -> Self {
        Self {
            enabled: true,
            max_unreachable_lifetime_s: 7_200,
            cleanup_interval_s: 3_600,
        }
    }

    /// Mesh-identity default: cleanup ON with a 90 day horizon and
    /// daily sweep. Identity rows are cheap to recreate on next
    /// contact (one extra handshake), so ageing them out after a
    /// season is fine and prevents `peers_seen` from growing
    /// indefinitely on long-running nodes.
    pub fn mesh_identity_default() -> Self {
        Self {
            enabled: true,
            max_unreachable_lifetime_s: 7_776_000,
            cleanup_interval_s: 86_400,
        }
    }

    /// In-memory rate-limit / discovery state default: cleanup ON
    /// with a 7 day horizon and 1 h sweep. The map keys cost
    /// effectively nothing per entry, but a long-running broadcaster
    /// shouldn't accumulate entries for peers seen weeks ago.
    pub fn ephemeral_default() -> Self {
        Self {
            enabled: true,
            max_unreachable_lifetime_s: 604_800,
            cleanup_interval_s: 3_600,
        }
    }
}
