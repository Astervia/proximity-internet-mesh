//! User-to-user encrypted messaging.
//!
//! Wire layer:
//! - [`pim_protocol::ControlType::PeerInfo`] is sent once per direction
//!   right after a session is established. Carries the sender's static
//!   X25519 public key plus its friendly node name, so the receiver can
//!   ECIES-encrypt future messages and display a stable label even when
//!   the peer's mesh IP or hostname changes.
//! - [`pim_protocol::ControlType::Message`] carries the ECIES-encrypted
//!   payload as produced by [`pim_crypto::e2e_encrypt`].
//! - [`pim_protocol::ControlType::MessageAck`] confirms receipt
//!   (delivered) and read state.
//!
//! Storage layer:
//! - SQLite database at `data_dir/messages.db` (mode 0600 on Unix).
//! - Two tables (`messages`, `peers_seen`) plus a denormalized
//!   `conversations_meta` table for cheap conversation-list queries.
//!
//! Eventing:
//! - [`MessageEvent`] is broadcast via a Tokio broadcast channel and
//!   forwarded to JSON-RPC `messages.event` subscribers by `rpc.rs`.

#[path = "messaging/storage.rs"]
mod storage;

#[path = "messaging/dispatch.rs"]
pub(crate) mod dispatch;

pub(crate) use storage::{
    AckKind, ConversationSummary, ForgetPeerOutcome, ImportOutcome, MessageDirection,
    MessageRecord, MessageStatus, MessagingStorage,
};

use std::path::PathBuf;
use std::sync::Arc;

use pim_core::NodeId;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Maximum plaintext body size accepted by [`MessagingState::send_local`].
pub const MAX_BODY_BYTES: usize = 8 * 1024;

/// Source of an incoming `PeerInfo` frame — direct (handshake) vs.
/// routed (multi-hop broadcast). Lets `handle_incoming_peer_info`
/// apply broadcast gates only to the routed path and lets the
/// `peer_seen` event payload surface `via` so the UI can section
/// direct-paired vs. broadcast-discovered peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerInfoSource {
    /// Direct neighbour over an existing session.
    Direct,
    /// Routed via the multi-hop control plane (mesh broadcast).
    Routed,
}

/// Event emitted by the messaging subsystem and forwarded over JSON-RPC.
///
/// `MessageReceived` carries `MessageRecord` + `ConversationSummary` boxed
/// because it dwarfs the other variants (≈ 264 B vs 32-57 B). Boxing
/// keeps the enum small and silences `clippy::large-enum-variant`.
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
    /// A peer's identity metadata was just learned (or refreshed).
    PeerSeen {
        /// Stable cryptographic identifier (32-char hex).
        peer_node_id: String,
        /// Latest friendly name advertised by the peer.
        name: String,
        /// Whether we now have a usable X25519 public key for them.
        x25519_known: bool,
        /// How the identity arrived — direct handshake or routed
        /// broadcast. The UI can use this to section direct-paired
        /// vs. broadcast-discovered peers.
        via: PeerInfoSource,
    },
    /// Message history was wiped — `scope: "peer"` carries
    /// `peer_node_id`; `scope: "all"` clears everything. Lets live
    /// UIs flush their per-peer message buffers + conversation rows
    /// without an extra refetch.
    HistoryCleared {
        peer_node_id: Option<String>,
        scope: HistoryScope,
        /// Number of message rows the daemon actually deleted.
        deleted_messages: i64,
    },
}

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

/// Daemon-side messaging facade exposed via [`crate::app::DaemonState`].
pub struct MessagingState {
    storage: Arc<MessagingStorage>,
    events_tx: broadcast::Sender<MessageEvent>,
}

impl MessagingState {
    /// Open or create the messages database and prepare the event channel.
    pub fn open(db_path: PathBuf) -> anyhow::Result<Self> {
        let storage = Arc::new(MessagingStorage::open(db_path)?);
        let (events_tx, _rx) = broadcast::channel(256);
        Ok(Self { storage, events_tx })
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

    /// Import a peer's identity from out-of-band material (the
    /// `peers.import_identity` RPC). Refuses to silently overwrite an
    /// existing key with a different one — see [`ImportOutcome`] for
    /// the three possible result states. Emits the same `peer_seen`
    /// event as a wire-learned `PeerInfo` on Inserted/Refreshed so the
    /// UI can react identically.
    pub async fn import_peer_identity(
        &self,
        peer: NodeId,
        x25519_pub: [u8; 32],
        name_if_set: Option<String>,
        now_ms: i64,
    ) -> anyhow::Result<ImportOutcome> {
        let storage = self.storage.clone();
        let name_for_storage = name_if_set.clone();
        let outcome = tokio::task::spawn_blocking(move || -> anyhow::Result<ImportOutcome> {
            storage.import_peer_identity_if_compatible(
                &peer,
                &x25519_pub,
                name_for_storage.as_deref(),
                now_ms,
            )
        })
        .await??;

        if matches!(outcome, ImportOutcome::Inserted | ImportOutcome::Refreshed) {
            // Mirror the event the on-wire PeerInfo path emits so live
            // UI subscribers refresh their conversation list / identity
            // cards without polling.
            let storage_for_lookup = self.storage.clone();
            let name = match name_if_set.filter(|s| !s.is_empty()) {
                Some(n) => n,
                None => {
                    tokio::task::spawn_blocking(move || storage_for_lookup.lookup_peer_name(&peer))
                        .await
                        .ok()
                        .and_then(|r| r.ok())
                        .flatten()
                        .unwrap_or_default()
                }
            };
            let _ = self.events_tx.send(MessageEvent::PeerSeen {
                peer_node_id: hex_node_id(&peer),
                name,
                x25519_known: true,
                // An import_identity is an explicit user action — surface
                // it as a "direct" identity so the UI doesn't filter it
                // through the broadcast-watch toggle.
                via: PeerInfoSource::Direct,
            });
        }

        Ok(outcome)
    }

    /// Atomic per-peer wipe — see [`MessagingStorage::delete_conversation`].
    /// Emits `HistoryCleared { scope: Peer }` on success so live UIs
    /// drop the buffer + sidebar row without polling.
    pub async fn delete_conversation(&self, peer: NodeId) -> anyhow::Result<(usize, bool)> {
        let storage = self.storage.clone();
        let peer_hex = hex_node_id(&peer);
        let peer_hex_for_storage = peer_hex.clone();
        let outcome = tokio::task::spawn_blocking(move || -> anyhow::Result<(usize, bool)> {
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
    /// Emits `HistoryCleared { scope: All }`.
    pub async fn delete_all_messages(&self) -> anyhow::Result<(usize, usize)> {
        let storage = self.storage.clone();
        let outcome = tokio::task::spawn_blocking(move || -> anyhow::Result<(usize, usize)> {
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

    /// Drop the cached identity for a peer (and optionally its
    /// message history). Emits `peer_seen { x25519_known: false }`
    /// so the discovered/known sidebar drops the row immediately,
    /// plus `HistoryCleared { scope: Peer }` when messages were
    /// also wiped.
    pub async fn forget_peer(
        &self,
        peer: NodeId,
        also_delete_messages: bool,
    ) -> anyhow::Result<ForgetPeerOutcome> {
        let storage = self.storage.clone();
        let peer_hex = hex_node_id(&peer);
        let peer_hex_for_storage = peer_hex.clone();
        let outcome = tokio::task::spawn_blocking(move || -> anyhow::Result<ForgetPeerOutcome> {
            storage.forget_peer(&peer_hex_for_storage, also_delete_messages)
        })
        .await??;

        if outcome.forgot_identity {
            let _ = self.events_tx.send(MessageEvent::PeerSeen {
                peer_node_id: peer_hex.clone(),
                name: String::new(),
                x25519_known: false,
                via: PeerInfoSource::Direct,
            });
        }
        if also_delete_messages && outcome.deleted_messages > 0 {
            let _ = self.events_tx.send(MessageEvent::HistoryCleared {
                peer_node_id: Some(peer_hex),
                scope: HistoryScope::Peer,
                deleted_messages: outcome.deleted_messages as i64,
            });
        }
        Ok(outcome)
    }

    /// Persist a peer's advertised identity. Returns `true` if a new
    /// peer was inserted (vs. an existing entry refreshed).
    ///
    /// `source` discriminates direct vs. routed PeerInfo on the
    /// emitted `peer_seen` event so subscribers can section paired
    /// peers from broadcast-discovered ones.
    ///
    /// `emit_event` is the "watch incoming broadcasts" gate plumbed
    /// through from `handle_incoming_peer_info` — when `false`, the
    /// keystore upsert still runs (replies need the X25519 key) but
    /// no `peer_seen` event is broadcast.
    pub async fn record_peer_seen(
        &self,
        peer: NodeId,
        x25519_pub: [u8; 32],
        name: String,
        now_ms: i64,
        source: PeerInfoSource,
        emit_event: bool,
    ) -> anyhow::Result<bool> {
        let storage = self.storage.clone();
        let storage_name = name.clone();
        let inserted = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            storage.upsert_peer_seen(&peer, &x25519_pub, &storage_name, now_ms)
        })
        .await??;

        if emit_event {
            let _ = self.events_tx.send(MessageEvent::PeerSeen {
                peer_node_id: hex_node_id(&peer),
                name,
                x25519_known: true,
                via: source,
            });
        }

        Ok(inserted)
    }

    /// Persist a freshly-sent local message in `pending` status. The dispatch
    /// task owned by `app/event_loop` is responsible for actually putting it
    /// on the wire and calling [`Self::mark_sent`] / [`Self::mark_delivered`]
    /// when the corresponding ack arrives.
    pub async fn record_local_send(
        &self,
        peer: NodeId,
        message_id: [u8; 16],
        body: String,
        timestamp_ms: i64,
    ) -> anyhow::Result<MessageRecord> {
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
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
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
    pub async fn mark_sent(
        &self,
        peer: NodeId,
        message_id: [u8; 16],
        at_ms: i64,
    ) -> anyhow::Result<()> {
        let storage = self.storage.clone();
        let id_hex = hex16(&message_id);
        let peer_hex = hex_node_id(&peer);
        let storage_id = id_hex.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            storage.set_message_status(&storage_id, MessageStatus::Sent, None, None)
        })
        .await??;

        let _ = self.events_tx.send(MessageEvent::MessageStatus {
            message_id: id_hex,
            peer_node_id: peer_hex,
            new_status: MessageStatus::Sent,
            at_ms,
        });
        Ok(())
    }

    /// Apply a `delivered` ack from the peer.
    pub async fn mark_delivered(
        &self,
        peer: NodeId,
        message_id: [u8; 16],
        at_ms: i64,
    ) -> anyhow::Result<()> {
        let storage = self.storage.clone();
        let id_hex = hex16(&message_id);
        let peer_hex = hex_node_id(&peer);
        let storage_id = id_hex.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            storage.set_message_status(&storage_id, MessageStatus::Delivered, Some(at_ms), None)
        })
        .await??;

        let _ = self.events_tx.send(MessageEvent::MessageStatus {
            message_id: id_hex,
            peer_node_id: peer_hex,
            new_status: MessageStatus::Delivered,
            at_ms,
        });
        Ok(())
    }

    /// Apply a `read` ack from the peer (if/when supported by the UI).
    pub async fn mark_read(
        &self,
        peer: NodeId,
        message_id: [u8; 16],
        at_ms: i64,
    ) -> anyhow::Result<()> {
        let storage = self.storage.clone();
        let id_hex = hex16(&message_id);
        let peer_hex = hex_node_id(&peer);
        let storage_id = id_hex.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            storage.set_message_status(&storage_id, MessageStatus::Read, None, Some(at_ms))
        })
        .await??;

        let _ = self.events_tx.send(MessageEvent::MessageStatus {
            message_id: id_hex,
            peer_node_id: peer_hex,
            new_status: MessageStatus::Read,
            at_ms,
        });
        Ok(())
    }

    /// Mark a previously-pending outbound message as failed.
    pub async fn mark_failed(
        &self,
        peer: NodeId,
        message_id: [u8; 16],
        reason: String,
        at_ms: i64,
    ) -> anyhow::Result<()> {
        let storage = self.storage.clone();
        let id_hex = hex16(&message_id);
        let peer_hex = hex_node_id(&peer);
        let storage_id = id_hex.clone();
        let storage_reason = reason.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
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

    /// Persist an incoming peer message and emit a `MessageReceived` event.
    /// The caller is responsible for ECIES-decrypting the ciphertext into
    /// the plaintext body before calling this.
    pub async fn record_remote_receive(
        &self,
        peer: NodeId,
        message_id: [u8; 16],
        body: String,
        sender_timestamp_ms: i64,
        received_at_ms: i64,
        cached_peer_name: Option<String>,
    ) -> anyhow::Result<MessageRecord> {
        let storage = self.storage.clone();
        let peer_id_hex = hex_node_id(&peer);
        let message_id_hex = hex16(&message_id);
        let preview_source = body.clone();

        let record = MessageRecord {
            id: message_id_hex.clone(),
            peer_node_id: peer_id_hex.clone(),
            direction: MessageDirection::Received,
            body,
            timestamp_ms: sender_timestamp_ms,
            status: MessageStatus::Delivered,
            failure_reason: None,
            delivered_at_ms: Some(received_at_ms),
            read_at_ms: None,
        };
        let record_clone = record.clone();
        let conv = tokio::task::spawn_blocking(move || -> anyhow::Result<ConversationSummary> {
            storage.insert_message(&record_clone)?;
            storage.bump_conversation_after_remote_receive(
                &peer_id_hex,
                &message_id_hex,
                sender_timestamp_ms,
                &preview_source,
                cached_peer_name.as_deref(),
            )
        })
        .await??;

        let _ = self.events_tx.send(MessageEvent::MessageReceived {
            message: Box::new(record.clone()),
            conversation: Box::new(conv),
        });
        Ok(record)
    }
}

/// Helper to format a [`NodeId`] as a 32-char lowercase hex string (matches
/// the on-wire NodeId hex used elsewhere in `pim-core`).
pub fn hex_node_id(id: &NodeId) -> String {
    let mut out = String::with_capacity(32);
    for b in id.as_bytes() {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

fn hex16(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(32);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}
