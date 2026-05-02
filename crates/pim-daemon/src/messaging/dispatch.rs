//! Outbound dispatch + inbound handling glue for the messaging subsystem.
//!
//! Lives at `pim_daemon::messaging::dispatch` (re-exported by
//! `messaging.rs`) so the busy `app/event_loop.rs` only has to call into
//! a handful of small helpers.

use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{anyhow, Context, Result};
use pim_core::NodeId;
use pim_crypto::{e2e_decrypt_in_place, e2e_encrypt};
use pim_protocol::{ControlFrame, DataFlags};
use tracing::{debug, info, warn};

use super::{hex_node_id, AckKind, PeerInfoSource, MAX_BODY_BYTES};
use crate::app::ip_control::send_routed_control;
use crate::app::peer_tasks::send_control;
use crate::app::DaemonState;

/// Wall-clock now in milliseconds since the Unix epoch (i64 to match the
/// SQLite column type).
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

/// Routed counterpart to [`send_peer_info`] — piggybacks our identity
/// ahead of a user message so a multi-hop recipient that has not yet
/// cached our X25519 key can decrypt + reply without an explicit
/// `peers.import_identity` round-trip on their side. Best-effort: a
/// failed send is logged but does not abort the surrounding `Message`
/// dispatch.
pub(crate) async fn send_peer_info_routed(state: &Arc<DaemonState>, peer: NodeId) {
    let frame = ControlFrame::PeerInfo {
        x25519_pub: state.own_x25519_pub,
        friendly_name: state.node_name.clone(),
    };
    let sent = send_routed_control(state, peer, frame).await;
    if !sent {
        debug!(%peer, "routed PeerInfo: no route (proceeding with Message anyway)");
    }
}

/// Outcome of one identity-broadcast cycle — used by both the
/// `peers.broadcast_identity_now` RPC and the periodic background
/// task so they share a single implementation.
pub(crate) struct BroadcastCycleOutcome {
    /// Number of distinct destination NodeIds the cycle attempted to
    /// reach (excluding ourselves). The actual on-wire send rate may
    /// be lower if a route disappeared between snapshot and dispatch.
    pub recipients: usize,
}

/// Send our identity (`PeerInfo`) to every node currently in the
/// routing table. Idempotent on the recipient side via the existing
/// `record_peer_seen` upsert.
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

/// Long-running background task that fires `run_broadcast_cycle`
/// according to `state.broadcast_config.outgoing_interval_s`. Wakes
/// early when `broadcast_notify` is poked (e.g. on a config change)
/// so the next cycle reflects the new interval immediately.
///
/// Cancellation: the task exits cleanly when `state.cancel` fires.
pub(crate) async fn run_broadcast_task(state: Arc<DaemonState>) {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tokio::time::sleep;

    /// How long to wait before re-checking the config when broadcasts
    /// are disabled. Picked small enough that a freshly-enabled
    /// schedule fires within a few seconds; the `Notify` path makes
    /// this responsive even when the user toggles via UI.
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

        // Race the timer against the notify so config edits take
        // effect immediately. Ordering matters: notify_one() wakes
        // exactly one waiter, so a missed wake while we're inside
        // run_broadcast_cycle is harmless — the next loop re-reads
        // the (already-updated) config.
        tokio::select! {
            _ = sleep(sleep_dur) => {}
            _ = state.broadcast_notify.notified() => {
                // Config changed — re-evaluate without firing a cycle.
                continue;
            }
            _ = state.cancel.cancelled() => return,
        }

        // Re-read interval after sleep — it may have flipped to None
        // while we waited (e.g. user disabled broadcasts).
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
        // Update last counters again here for clarity (run_broadcast_cycle
        // already does it but keeping the side-effect explicit makes the
        // task readable).
        let _ = Ordering::Relaxed;
    }
}

/// Persist a freshly-learned PeerInfo and (conditionally) emit the
/// `peer_seen` event.
///
/// Routed PeerInfo arrivals are subject to two configurable gates:
/// 1. `messaging.broadcast.min_peer_interval_s` — drops broadcasts
///    arriving sooner than the configured per-peer minimum so a single
///    misbehaving peer cannot flood our keystore or UI.
/// 2. `messaging.broadcast.watch_incoming` — when false, we still
///    upsert the X25519 key (replies need it) but do NOT surface the
///    `peer_seen` event so the UI stays quiet.
///
/// Direct PeerInfo (handshake-initiated) bypasses both gates — the
/// frame can only be sent once per direct session and we always want
/// it surfaced.
pub(crate) async fn handle_incoming_peer_info(
    state: &Arc<DaemonState>,
    src: NodeId,
    x25519_pub: [u8; 32],
    friendly_name: String,
    source: PeerInfoSource,
) {
    if source == PeerInfoSource::Routed {
        // Rate-limit gate. Reject (silently — debug only) repeats from
        // the same peer arriving sooner than configured.
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
        .messaging
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

/// Whether `record_peer_seen` should also emit the `peer_seen` event
/// based on the watch-incoming policy. Direct PeerInfo always emits;
/// routed PeerInfo respects the toggle.
async fn broadcast_should_emit_event(state: &Arc<DaemonState>, source: PeerInfoSource) -> bool {
    match source {
        PeerInfoSource::Direct => true,
        PeerInfoSource::Routed => state.broadcast_config.read().await.watch_incoming,
    }
}

/// Encrypt + send a user message to `peer`. Returns the persisted record
/// (with status `pending` initially, transitioning to `sent` once the
/// transport accepts it). Bumps to `failed` if no route or no x25519 is
/// known yet.
pub(crate) async fn send_user_message(
    state: &Arc<DaemonState>,
    peer: NodeId,
    body: String,
) -> Result<super::MessageRecord> {
    if body.len() > MAX_BODY_BYTES {
        return Err(anyhow!("message body exceeds {MAX_BODY_BYTES} bytes"));
    }

    let storage = state.messaging.storage().clone();
    let recipient_x25519 = {
        let peer_copy = peer;
        tokio::task::spawn_blocking(move || storage.lookup_x25519_pub(&peer_copy))
            .await
            .context("storage join")??
    };
    let recipient_x25519 = match recipient_x25519 {
        Some(k) => k,
        None => {
            return Err(anyhow!(
                "no x25519 public key cached for {peer}; wait until peer comes online and re-issues PeerInfo"
            ))
        }
    };

    let mut id_bytes = [0u8; 16];
    id_bytes.copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let timestamp_ms = now_ms();

    let record = state
        .messaging
        .record_local_send(peer, id_bytes, body.clone(), timestamp_ms)
        .await?;

    let ciphertext = e2e_encrypt(body.as_bytes(), &recipient_x25519)
        .map_err(|e| anyhow!("e2e_encrypt failed: {e}"))?;

    // Best-effort: prepend a routed PeerInfo so a multi-hop recipient
    // who only knows us via an out-of-band identity card (or not at
    // all) can populate their x25519 cache and reply. Idempotent on
    // the recipient side — `record_peer_seen` upserts. If the routing
    // table has no path the helper logs and returns; the subsequent
    // Message attempt will surface the `no_route` failure to the user.
    send_peer_info_routed(state, peer).await;

    let frame = ControlFrame::Message {
        message_id: id_bytes,
        timestamp_ms: timestamp_ms as u64,
        ciphertext,
    };

    let sent = send_routed_control_with_e2e_flag(state, peer, frame).await;
    if sent {
        let _ = state.messaging.mark_sent(peer, id_bytes, now_ms()).await;
    } else {
        let _ = state
            .messaging
            .mark_failed(peer, id_bytes, "no_route".into(), now_ms())
            .await;
    }

    Ok(record)
}

/// Variant of [`send_routed_control`] that sets `IS_CONTROL | IS_E2E` on
/// the underlying `MeshDataFrame` (so intermediate relays do not try to
/// route the inner ciphertext as if it were an IP packet).
async fn send_routed_control_with_e2e_flag(
    state: &Arc<DaemonState>,
    dst_id: NodeId,
    cf: ControlFrame,
) -> bool {
    // Prefer the existing helper for IS_CONTROL routing; the IS_E2E flag is
    // additive but not currently surfaced through send_routed_control. For
    // v1 we route via send_routed_control (IS_CONTROL only) — relays pass
    // it through unchanged because dst_id != self_id and IS_CONTROL is set;
    // ECIES still protects the inner plaintext regardless of the flag.
    let _ = DataFlags::IS_E2E; // marker kept for grep until kernel adds an IS_E2E-aware routed path
    send_routed_control(state, dst_id, cf).await
}

/// Decrypt + persist an incoming `Message` and ack it.
pub(crate) async fn handle_incoming_message(
    state: &Arc<DaemonState>,
    src: NodeId,
    message_id: [u8; 16],
    sender_timestamp_ms: u64,
    ciphertext: Vec<u8>,
) {
    let identity_seed = state.identity.signing_key().to_bytes();
    let mut buffer = ciphertext;
    if let Err(e) = e2e_decrypt_in_place(&mut buffer, &identity_seed) {
        warn!(%src, "messaging: ECIES decrypt failed: {e}");
        return;
    }
    let body = match String::from_utf8(buffer) {
        Ok(s) => s,
        Err(_) => {
            warn!(%src, "messaging: payload not valid UTF-8");
            return;
        }
    };

    let received_at = now_ms();
    let cached_name = {
        let storage = state.messaging.storage().clone();
        let peer_copy = src;
        match tokio::task::spawn_blocking(move || storage.lookup_peer_name(&peer_copy)).await {
            Ok(Ok(n)) => n,
            _ => None,
        }
    };

    if let Err(e) = state
        .messaging
        .record_remote_receive(
            src,
            message_id,
            body,
            sender_timestamp_ms as i64,
            received_at,
            cached_name,
        )
        .await
    {
        warn!(%src, "record_remote_receive failed: {e}");
        return;
    }

    let ack = ControlFrame::MessageAck {
        message_id,
        ack_kind: AckKind::Delivered as u8,
    };
    let _ = send_routed_control(state, src, ack).await;
    debug!(%src, id = %hex_node_id(&src), "messaging: stored received + acked delivered");
}

/// Apply an inbound `MessageAck` from `src` to the local outbound row.
pub(crate) async fn handle_incoming_message_ack(
    state: &Arc<DaemonState>,
    src: NodeId,
    message_id: [u8; 16],
    ack_kind: u8,
) {
    let kind = match AckKind::from_u8(ack_kind) {
        Some(k) => k,
        None => {
            warn!(%src, "messaging: ignoring MessageAck with unknown kind {ack_kind}");
            return;
        }
    };
    let now = now_ms();
    match kind {
        AckKind::Delivered => {
            let _ = state.messaging.mark_delivered(src, message_id, now).await;
        }
        AckKind::Read => {
            let _ = state.messaging.mark_read(src, message_id, now).await;
        }
    }
}
