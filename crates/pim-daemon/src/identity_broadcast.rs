//! Daemon-essential identity broadcast.
//!
//! - [`send_peer_info`] — direct (per-session) PeerInfo emit.
//! - [`send_peer_info_routed`] — multi-hop PeerInfo emit.
//! - [`run_broadcast_cycle`] — one-shot pulse that fans out our
//!   identity to every node in the routing table.
//! - [`run_broadcast_task`] — long-running task driving the cycle on
//!   the configured interval; honours `messaging.broadcast.*`
//!   configuration even though messaging itself is now an optional
//!   plugin (the toggle is mesh-wide identity hygiene, not chat).
//! - [`handle_incoming_peer_info`] — folds inbound PeerInfo into the
//!   daemon's peer directory, applying the routed broadcast rate-limit
//!   gate.

use std::sync::Arc;
use std::time::SystemTime;

use pim_core::NodeId;
use pim_plugin::PeerInfoSource;
use pim_protocol::ControlFrame;
use tracing::{debug, info, warn};

use crate::app::ip_control::send_routed_control;
use crate::app::peer_tasks::send_control;
use crate::app::DaemonState;

/// Wall-clock now in milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Send our local PeerInfo to a freshly-handshaken peer. Best-effort.
pub(crate) async fn send_peer_info(state: &Arc<DaemonState>, peer: NodeId) {
    let frame = ControlFrame::PeerInfo {
        x25519_pub: state.own_x25519_pub,
        friendly_name: state.node_name.clone(),
    };
    send_control(state, &peer, frame).await;
    debug!(%peer, "sent PeerInfo");
}

/// Routed counterpart to [`send_peer_info`]. Best-effort: a failed
/// send is logged but does not abort surrounding work.
pub(crate) async fn send_peer_info_routed(state: &Arc<DaemonState>, peer: NodeId) {
    let frame = ControlFrame::PeerInfo {
        x25519_pub: state.own_x25519_pub,
        friendly_name: state.node_name.clone(),
    };
    let sent = send_routed_control(state, peer, frame).await;
    if !sent {
        debug!(%peer, "routed PeerInfo: no route");
    }
}

/// Outcome of one identity-broadcast cycle.
pub(crate) struct BroadcastCycleOutcome {
    /// Number of distinct destination NodeIds the cycle attempted to
    /// reach (excluding ourselves).
    pub recipients: usize,
}

/// Send our identity (`PeerInfo`) to every node currently in the
/// routing table. Idempotent on the recipient side.
pub(crate) async fn run_broadcast_cycle(state: &Arc<DaemonState>) -> BroadcastCycleOutcome {
    let snapshot: Vec<NodeId> = {
        let routing = state.routing.lock().await;
        routing
            .routes_snapshot()
            .into_iter()
            .map(|(id, _entry)| id)
            .filter(|id| *id != state.self_id)
            .collect()
    };
    let recipients = snapshot.len();
    for peer in snapshot {
        send_peer_info_routed(state, peer).await;
    }
    let now_ms = now_ms();
    state
        .last_broadcast_ms
        .store(now_ms, std::sync::atomic::Ordering::Relaxed);
    state
        .last_broadcast_recipients
        .store(recipients as u64, std::sync::atomic::Ordering::Relaxed);
    debug!(recipients, "broadcast cycle complete");
    BroadcastCycleOutcome { recipients }
}

/// Long-running background task driving [`run_broadcast_cycle`] on
/// the configured `outgoing_interval_s`. Wakes early when
/// `state.broadcast_notify` is poked (e.g. on a config change).
pub(crate) async fn run_broadcast_task(state: Arc<DaemonState>) {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tokio::time::sleep;

    /// How long to wait before re-checking the config when broadcasts
    /// are disabled.
    const DISABLED_RECHECK_S: u64 = 30;

    loop {
        if state.cancel.is_cancelled() {
            return;
        }
        let interval = state.broadcast_config.read().await.outgoing_interval_s;

        let sleep_dur = match interval {
            Some(secs) => Duration::from_secs(secs.max(1)),
            None => Duration::from_secs(DISABLED_RECHECK_S),
        };

        tokio::select! {
            _ = sleep(sleep_dur) => {}
            _ = state.broadcast_notify.notified() => {
                continue;
            }
            _ = state.cancel.cancelled() => return,
        }

        let still_enabled = state
            .broadcast_config
            .read()
            .await
            .outgoing_interval_s
            .is_some();
        if !still_enabled {
            continue;
        }
        let _ = run_broadcast_cycle(&state).await;
        let _ = Ordering::Relaxed;
    }
}

/// Persist a freshly-learned PeerInfo into the daemon's peer directory
/// and (conditionally) emit the `peer_seen` event.
///
/// Routed PeerInfo arrivals are subject to two configurable gates:
/// 1. `messaging.broadcast.min_peer_interval_s` — drops broadcasts
///    arriving sooner than the configured per-peer minimum so a single
///    misbehaving peer cannot flood our keystore.
/// 2. `messaging.broadcast.watch_incoming` — when false, we still
///    upsert the X25519 key (replies need it) but do NOT surface the
///    `peer_seen` event so subscribers stay quiet.
pub(crate) async fn handle_incoming_peer_info(
    state: &Arc<DaemonState>,
    src: NodeId,
    x25519_pub: [u8; 32],
    friendly_name: String,
    source: PeerInfoSource,
) {
    if source == PeerInfoSource::Routed {
        let min_interval_s = state.broadcast_config.read().await.min_peer_interval_s;
        if min_interval_s > 0 {
            let now = std::time::Instant::now();
            let mut last_seen = state.broadcast_peer_last_seen.lock().await;
            if let Some(prev) = last_seen.get(&src) {
                if now.duration_since(*prev).as_secs() < min_interval_s {
                    debug!(
                        %src,
                        elapsed_s = now.duration_since(*prev).as_secs(),
                        min_interval_s,
                        "broadcast rate-limit drop"
                    );
                    return;
                }
            }
            last_seen.insert(src, now);
        }
    }

    let inserted = match state
        .peer_directory
        .record_peer_seen(
            src,
            x25519_pub,
            friendly_name.clone(),
            now_ms(),
            source,
            broadcast_should_emit_event(state, source).await,
        )
        .await
    {
        Ok(v) => v,
        Err(e) => {
            warn!(%src, "record_peer_seen failed: {e}");
            return;
        }
    };
    if inserted {
        info!(%src, name = %friendly_name, ?source, "first contact: peer identity cached");
    }
}

async fn broadcast_should_emit_event(state: &Arc<DaemonState>, source: PeerInfoSource) -> bool {
    match source {
        PeerInfoSource::Direct => true,
        PeerInfoSource::Routed => state.broadcast_config.read().await.watch_incoming,
    }
}
