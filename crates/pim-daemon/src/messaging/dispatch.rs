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

use super::{hex_node_id, AckKind, MAX_BODY_BYTES};
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

/// Persist a freshly-learned PeerInfo and emit the `peer_seen` event.
pub(crate) async fn handle_incoming_peer_info(
    state: &Arc<DaemonState>,
    src: NodeId,
    x25519_pub: [u8; 32],
    friendly_name: String,
) {
    let inserted = match state
        .messaging
        .record_peer_seen(src, x25519_pub, friendly_name.clone(), now_ms())
        .await
    {
        Ok(v) => v,
        Err(e) => {
            warn!(%src, "record_peer_seen failed: {e}");
            return;
        }
    };
    if inserted {
        info!(%src, name = %friendly_name, "first contact: peer identity cached");
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
