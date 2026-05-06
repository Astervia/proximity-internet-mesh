//! Plugin-internal facade combining storage, events, and ports.
//!
//! The daemon's RPC layer (when the `messaging` feature is enabled)
//! holds an `Arc<MessagingService>` and calls into it directly for the
//! action API (`send`, `mark_*`, `delete_*`, history queries).
//!
//! The plugin's [`crate::plugin::MessagingPlugin`] also holds an
//! `Arc<MessagingService>`; its `handle_payload` thunk dispatches into
//! [`Self::handle_incoming_message`] /
//! [`Self::handle_incoming_message_ack`] when a matching
//! `ControlFrame::PluginPayload` arrives.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use pim_core::NodeId;
use pim_crypto::{e2e_decrypt_in_place, e2e_encrypt};
use pim_plugin::{ControlSender, IdentitySecrets, PeerDirectory};
use pim_protocol::ControlFrame;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::storage::{
    AckKind, ConversationSummary, MessageDirection, MessageRecord, MessageStatus, MessagingStorage,
};
use crate::wire::{decode_ack, decode_message, encode_ack, encode_message, KIND_ACK, KIND_MESSAGE};
use crate::{hex16, hex_node_id};

/// Maximum plaintext body size accepted by [`MessagingService::send`].
pub const MAX_BODY_BYTES: usize = 8 * 1024;

/// Discriminator for `HistoryCleared` — `peer` (one conversation)
/// vs. `all` (everything).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryScope {
    /// One conversation cleared; `peer_node_id` is set.
    Peer,
    /// All conversations cleared; `peer_node_id` is `None`.
    All,
}

/// Event emitted by the messaging subsystem and forwarded over JSON-RPC
/// as `messages.event` notifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageEvent {
    /// New message arrived from a peer.
    MessageReceived {
        /// The persisted message.
        message: Box<MessageRecord>,
        /// Updated conversation summary (denormalized).
        conversation: Box<ConversationSummary>,
    },
    /// Status of a previously-sent message changed (sent → delivered → read,
    /// or any → failed).
    MessageStatus {
        /// Affected message id (UUIDv4 hex without dashes).
        message_id: String,
        /// Peer the original message was addressed to / received from.
        peer_node_id: String,
        /// New status.
        new_status: MessageStatus,
        /// Wall-clock when the transition happened.
        at_ms: i64,
    },
    /// Message history was wiped — `scope: "peer"` carries
    /// `peer_node_id`; `scope: "all"` clears everything.
    HistoryCleared {
        /// Affected peer (set only for `scope: "peer"`).
        peer_node_id: Option<String>,
        /// Whether one conversation or everything was wiped.
        scope: HistoryScope,
        /// Number of message rows the daemon actually deleted.
        deleted_messages: i64,
    },
}

/// Plugin-internal service handle.
pub struct MessagingService {
    storage: Arc<MessagingStorage>,
    events_tx: broadcast::Sender<MessageEvent>,
    peers: Arc<dyn PeerDirectory>,
    control: Arc<dyn ControlSender>,
    identity: Arc<dyn IdentitySecrets>,
}

impl MessagingService {
    /// Open the messaging database and bind the daemon-supplied ports.
    pub fn open(
        db_path: PathBuf,
        peers: Arc<dyn PeerDirectory>,
        control: Arc<dyn ControlSender>,
        identity: Arc<dyn IdentitySecrets>,
    ) -> Result<Self> {
        let storage = Arc::new(MessagingStorage::open(db_path)?);
        let (events_tx, _rx) = broadcast::channel(256);
        Ok(Self {
            storage,
            events_tx,
            peers,
            control,
            identity,
        })
    }

    /// Subscribe to the broadcast event stream. Each subscriber gets every
    /// future event until they drop the receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<MessageEvent> {
        self.events_tx.subscribe()
    }

    /// Borrow the underlying storage handle (read-mostly use cases like
    /// `messages.history`).
    pub fn storage(&self) -> &Arc<MessagingStorage> {
        &self.storage
    }

    /// Snapshot of all conversations, sorted by most-recent activity.
    /// Each summary's `name` and `x25519_pubkey` are filled in from the
    /// peer directory.
    pub async fn list_conversations(&self) -> Result<Vec<ConversationSummary>> {
        let storage = self.storage.clone();
        let mut rows = tokio::task::spawn_blocking(move || storage.list_conversations_raw())
            .await
            .context("storage join")??;
        for row in rows.iter_mut() {
            let peer = match parse_node_id(&row.peer_node_id) {
                Some(p) => p,
                None => continue,
            };
            if let Some(name) = self.peers.lookup_name(&peer).await {
                if !name.is_empty() {
                    row.name = name;
                }
            }
            if let Some(x25519) = self.peers.lookup_x25519(&peer).await {
                row.x25519_pubkey = Some(hex32(&x25519));
            }
        }
        Ok(rows)
    }

    /// Persist a freshly-sent local message in `pending` status.
    pub async fn record_local_send(
        &self,
        peer: NodeId,
        message_id: [u8; 16],
        body: String,
        timestamp_ms: i64,
    ) -> Result<MessageRecord> {
        let storage = self.storage.clone();
        let peer_id_hex = hex_node_id(&peer);
        let message_id_hex = hex16(&message_id);

        let record = MessageRecord {
            id: message_id_hex.clone(),
            peer_node_id: peer_id_hex.clone(),
            direction: MessageDirection::Sent,
            body,
            timestamp_ms,
            status: MessageStatus::Pending,
            failure_reason: None,
            delivered_at_ms: None,
            read_at_ms: None,
        };
        let record_clone = record.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            storage.insert_message(&record_clone)?;
            storage.bump_conversation_after_local_send(
                &peer_id_hex,
                &message_id_hex,
                timestamp_ms,
                &record_clone.body,
            )?;
            Ok(())
        })
        .await??;

        Ok(record)
    }

    /// Move an outbound message from `pending` to `sent`.
    pub async fn mark_sent(&self, peer: NodeId, message_id: [u8; 16], at_ms: i64) -> Result<()> {
        self.set_status(peer, message_id, MessageStatus::Sent, None, None, at_ms)
            .await
    }

    /// Apply a `delivered` ack from the peer.
    pub async fn mark_delivered(
        &self,
        peer: NodeId,
        message_id: [u8; 16],
        at_ms: i64,
    ) -> Result<()> {
        self.set_status(
            peer,
            message_id,
            MessageStatus::Delivered,
            Some(at_ms),
            None,
            at_ms,
        )
        .await
    }

    /// Apply a `read` ack from the peer (if/when supported by the UI).
    pub async fn mark_read(&self, peer: NodeId, message_id: [u8; 16], at_ms: i64) -> Result<()> {
        self.set_status(
            peer,
            message_id,
            MessageStatus::Read,
            None,
            Some(at_ms),
            at_ms,
        )
        .await
    }

    /// Mark a previously-pending outbound message as failed.
    pub async fn mark_failed(
        &self,
        peer: NodeId,
        message_id: [u8; 16],
        reason: String,
        at_ms: i64,
    ) -> Result<()> {
        let storage = self.storage.clone();
        let id_hex = hex16(&message_id);
        let peer_hex = hex_node_id(&peer);
        let storage_id = id_hex.clone();
        let storage_reason = reason.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            storage.set_message_failed(&storage_id, &storage_reason, at_ms)
        })
        .await??;

        let _ = self.events_tx.send(MessageEvent::MessageStatus {
            message_id: id_hex,
            peer_node_id: peer_hex,
            new_status: MessageStatus::Failed,
            at_ms,
        });
        Ok(())
    }

    async fn set_status(
        &self,
        peer: NodeId,
        message_id: [u8; 16],
        status: MessageStatus,
        delivered_at_ms: Option<i64>,
        read_at_ms: Option<i64>,
        at_ms: i64,
    ) -> Result<()> {
        let storage = self.storage.clone();
        let id_hex = hex16(&message_id);
        let peer_hex = hex_node_id(&peer);
        let storage_id = id_hex.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            storage.set_message_status(&storage_id, status, delivered_at_ms, read_at_ms)
        })
        .await??;

        let _ = self.events_tx.send(MessageEvent::MessageStatus {
            message_id: id_hex,
            peer_node_id: peer_hex,
            new_status: status,
            at_ms,
        });
        Ok(())
    }

    /// Atomic per-peer wipe — see [`MessagingStorage::delete_conversation`].
    pub async fn delete_conversation(&self, peer: NodeId) -> Result<(usize, bool)> {
        let storage = self.storage.clone();
        let peer_hex = hex_node_id(&peer);
        let peer_hex_for_storage = peer_hex.clone();
        let outcome = tokio::task::spawn_blocking(move || -> Result<(usize, bool)> {
            storage.delete_conversation(&peer_hex_for_storage)
        })
        .await??;
        let _ = self.events_tx.send(MessageEvent::HistoryCleared {
            peer_node_id: Some(peer_hex),
            scope: HistoryScope::Peer,
            deleted_messages: outcome.0 as i64,
        });
        Ok(outcome)
    }

    /// Atomic global wipe — see [`MessagingStorage::delete_all_messages`].
    pub async fn delete_all_messages(&self) -> Result<(usize, usize)> {
        let storage = self.storage.clone();
        let outcome = tokio::task::spawn_blocking(move || -> Result<(usize, usize)> {
            storage.delete_all_messages()
        })
        .await??;
        let _ = self.events_tx.send(MessageEvent::HistoryCleared {
            peer_node_id: None,
            scope: HistoryScope::All,
            deleted_messages: outcome.0 as i64,
        });
        Ok(outcome)
    }

    /// Encrypt + send a user message to `peer`. Returns the persisted record
    /// (with status `pending` initially, transitioning to `sent` once the
    /// transport accepts it). Bumps to `failed` if no route or no x25519 is
    /// known yet.
    pub async fn send(&self, peer: NodeId, body: String) -> Result<MessageRecord> {
        if body.len() > MAX_BODY_BYTES {
            return Err(anyhow!("message body exceeds {MAX_BODY_BYTES} bytes"));
        }

        let recipient_x25519 = match self.peers.lookup_x25519(&peer).await {
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

        let record = self
            .record_local_send(peer, id_bytes, body.clone(), timestamp_ms)
            .await?;

        let ciphertext = e2e_encrypt(body.as_bytes(), &recipient_x25519)
            .map_err(|e| anyhow!("e2e_encrypt failed: {e}"))?;

        let body_bytes = encode_message(id_bytes, timestamp_ms as u64, &ciphertext);
        let frame = ControlFrame::PluginPayload {
            kind: KIND_MESSAGE.into(),
            body: body_bytes,
        };

        let sent = self.control.send_routed(peer, frame).await;
        if sent {
            let _ = self.mark_sent(peer, id_bytes, now_ms()).await;
        } else {
            let _ = self
                .mark_failed(peer, id_bytes, "no_route".into(), now_ms())
                .await;
        }

        Ok(record)
    }

    /// Decrypt + persist an incoming `messaging.msg` and ack it.
    pub async fn handle_incoming_message(&self, src: NodeId, body: Bytes) {
        let decoded = match decode_message(&body) {
            Ok(d) => d,
            Err(e) => {
                warn!(%src, "messaging: malformed messaging.msg: {e}");
                return;
            }
        };

        let identity_seed = self.identity.signing_seed();
        let mut buffer = decoded.ciphertext;
        if let Err(e) = e2e_decrypt_in_place(&mut buffer, &identity_seed) {
            warn!(%src, "messaging: ECIES decrypt failed: {e}");
            return;
        }
        let plaintext = match String::from_utf8(buffer) {
            Ok(s) => s,
            Err(_) => {
                warn!(%src, "messaging: payload not valid UTF-8");
                return;
            }
        };

        let received_at = now_ms();
        let cached_name = self.peers.lookup_name(&src).await;
        let cached_x25519 = self.peers.lookup_x25519(&src).await;

        let storage = self.storage.clone();
        let peer_id_hex = hex_node_id(&src);
        let message_id_hex = hex16(&decoded.message_id);
        let timestamp_ms = decoded.timestamp_ms as i64;

        let record = MessageRecord {
            id: message_id_hex.clone(),
            peer_node_id: peer_id_hex.clone(),
            direction: MessageDirection::Received,
            body: plaintext.clone(),
            timestamp_ms,
            status: MessageStatus::Delivered,
            failure_reason: None,
            delivered_at_ms: Some(received_at),
            read_at_ms: None,
        };
        let record_clone = record.clone();
        let preview_source = plaintext;

        let bump_result =
            tokio::task::spawn_blocking(move || -> Result<crate::storage::ConversationBump> {
                storage.insert_message(&record_clone)?;
                storage.bump_conversation_after_remote_receive(
                    &peer_id_hex,
                    &message_id_hex,
                    timestamp_ms,
                    &preview_source,
                )
            })
            .await;

        let bump = match bump_result {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => {
                warn!(%src, "messaging: storage write failed: {e}");
                return;
            }
            Err(e) => {
                warn!(%src, "messaging: storage join failed: {e}");
                return;
            }
        };

        let peer_hex = hex_node_id(&src);
        let short = crate::storage::short_id(&peer_hex);
        let conversation = ConversationSummary {
            peer_node_id: peer_hex,
            peer_node_id_short: short.clone(),
            name: cached_name
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| short.clone()),
            last_message_preview: Some(bump.preview),
            last_message_ts_ms: Some(timestamp_ms),
            unread_count: bump.unread_count,
            x25519_pubkey: cached_x25519.map(|k| hex32(&k)),
        };

        let _ = self.events_tx.send(MessageEvent::MessageReceived {
            message: Box::new(record),
            conversation: Box::new(conversation),
        });

        // Best-effort delivery ack via PluginPayload.
        let ack = ControlFrame::PluginPayload {
            kind: KIND_ACK.into(),
            body: encode_ack(decoded.message_id, AckKind::Delivered as u8),
        };
        let _ = self.control.send_routed(src, ack).await;
        debug!(%src, "messaging: stored received + acked delivered");
    }

    /// Apply an inbound `messaging.ack` to the local outbound row.
    pub async fn handle_incoming_message_ack(&self, src: NodeId, body: Bytes) {
        let decoded = match decode_ack(&body) {
            Ok(d) => d,
            Err(e) => {
                warn!(%src, "messaging: malformed messaging.ack: {e}");
                return;
            }
        };
        let kind = match AckKind::from_u8(decoded.ack_kind) {
            Some(k) => k,
            None => {
                warn!(%src, "messaging: ignoring ack with unknown kind {}", decoded.ack_kind);
                return;
            }
        };
        let now = now_ms();
        match kind {
            AckKind::Delivered => {
                let _ = self.mark_delivered(src, decoded.message_id, now).await;
            }
            AckKind::Read => {
                let _ = self.mark_read(src, decoded.message_id, now).await;
            }
        }
    }

    /// Plugin lifecycle: daemon dropped this peer's identity. Wipe any
    /// per-peer state owned by messaging and emit `HistoryCleared` so
    /// live UIs flush their per-peer buffers.
    pub async fn on_peer_forgotten(&self, peer: NodeId) {
        let storage = self.storage.clone();
        let peer_hex = hex_node_id(&peer);
        let peer_hex_for_storage = peer_hex.clone();
        let outcome = tokio::task::spawn_blocking(move || -> Result<(usize, bool)> {
            storage.delete_conversation(&peer_hex_for_storage)
        })
        .await;

        let outcome = match outcome {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                warn!(%peer, "messaging: on_peer_forgotten storage failed: {e}");
                return;
            }
            Err(e) => {
                warn!(%peer, "messaging: on_peer_forgotten join failed: {e}");
                return;
            }
        };

        if outcome.0 > 0 || outcome.1 {
            let _ = self.events_tx.send(MessageEvent::HistoryCleared {
                peer_node_id: Some(peer_hex),
                scope: HistoryScope::Peer,
                deleted_messages: outcome.0 as i64,
            });
        }
    }
}

/// Wall-clock now in milliseconds since the Unix epoch (i64 to match the
/// SQLite column type).
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn hex32(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn parse_node_id(hex: &str) -> Option<NodeId> {
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for i in 0..16 {
        let pair = &hex[i * 2..i * 2 + 2];
        bytes[i] = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(NodeId::from_bytes(bytes))
}
