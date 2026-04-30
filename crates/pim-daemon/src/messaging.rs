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
    AckKind, ConversationSummary, MessageDirection, MessageRecord, MessageStatus, MessagingStorage,
};

use std::path::PathBuf;
use std::sync::Arc;

use pim_core::NodeId;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Maximum plaintext body size accepted by [`MessagingState::send_local`].
pub const MAX_BODY_BYTES: usize = 8 * 1024;

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
    },
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

    /// Persist a peer's advertised identity. Returns `true` if a new
    /// peer was inserted (vs. an existing entry refreshed).
    pub async fn record_peer_seen(
        &self,
        peer: NodeId,
        x25519_pub: [u8; 32],
        name: String,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let storage = self.storage.clone();
        let storage_name = name.clone();
        let inserted = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            storage.upsert_peer_seen(&peer, &x25519_pub, &storage_name, now_ms)
        })
        .await??;

        let _ = self.events_tx.send(MessageEvent::PeerSeen {
            peer_node_id: hex_node_id(&peer),
            name,
            x25519_known: true,
        });

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

