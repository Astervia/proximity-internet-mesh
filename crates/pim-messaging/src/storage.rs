//! SQLite-backed persistence for the messaging plugin.
//!
//! All blocking calls are funneled through `tokio::task::spawn_blocking`
//! by [`crate::service::MessagingService`], so handlers never call into
//! rusqlite from an async context directly.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use pim_core::NodeId;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::hex_node_id;

const SCHEMA: &str = include_str!("schema.sql");

/// Direction of a stored message relative to the local node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageDirection {
    /// Outbound message originated locally.
    Sent,
    /// Inbound message received from a peer.
    Received,
}

impl MessageDirection {
    fn as_str(&self) -> &'static str {
        match self {
            MessageDirection::Sent => "sent",
            MessageDirection::Received => "received",
        }
    }

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "sent" => Ok(Self::Sent),
            "received" => Ok(Self::Received),
            other => Err(anyhow!("unknown direction: {other}")),
        }
    }
}

/// Lifecycle of an outbound message, from local persistence to peer
/// acknowledgement. Inbound messages start at `Delivered`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageStatus {
    /// Persisted locally; not yet handed to the transport layer.
    Pending,
    /// Sent on the wire; awaiting recipient ack.
    Sent,
    /// Recipient confirmed receipt.
    Delivered,
    /// Recipient marked the message as read.
    Read,
    /// Send / delivery failed permanently.
    Failed,
}

impl MessageStatus {
    fn as_str(&self) -> &'static str {
        match self {
            MessageStatus::Pending => "pending",
            MessageStatus::Sent => "sent",
            MessageStatus::Delivered => "delivered",
            MessageStatus::Read => "read",
            MessageStatus::Failed => "failed",
        }
    }

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "pending" => Ok(Self::Pending),
            "sent" => Ok(Self::Sent),
            "delivered" => Ok(Self::Delivered),
            "read" => Ok(Self::Read),
            "failed" => Ok(Self::Failed),
            other => Err(anyhow!("unknown status: {other}")),
        }
    }
}

/// Acknowledgement category transmitted on the wire (`messaging.ack` payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckKind {
    /// Recipient persisted the message — equivalent to `MessageStatus::Delivered`.
    Delivered = 1,
    /// Recipient marked the message read in their UI.
    Read = 2,
}

impl AckKind {
    /// Decode the wire-level ack tag. Returns `None` for unknown values.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Delivered),
            2 => Some(Self::Read),
            _ => None,
        }
    }
}

/// One stored message — sent or received. Field ordering matches the JSON
/// shape exposed via JSON-RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    /// 32-char lowercase hex (UUIDv4 bytes).
    pub id: String,
    /// Peer node id (32-char lowercase hex).
    pub peer_node_id: String,
    /// `sent` | `received`.
    pub direction: MessageDirection,
    /// Plaintext UTF-8 body.
    pub body: String,
    /// Wall-clock at sender (`timestamp_ms` from the wire frame for
    /// `received`, local clock for `sent`).
    pub timestamp_ms: i64,
    /// Lifecycle status.
    pub status: MessageStatus,
    /// Last failure reason (only set when `status = Failed`).
    pub failure_reason: Option<String>,
    /// Wall-clock when delivery ack was applied.
    pub delivered_at_ms: Option<i64>,
    /// Wall-clock when read ack was applied.
    pub read_at_ms: Option<i64>,
}

/// Denormalized "conversation" row used to avoid full-table aggregations
/// on the conversations list query. The `name` and `x25519_pubkey` fields
/// are populated by the service layer from the daemon's peer directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSummary {
    /// Peer node id (32-char lowercase hex).
    pub peer_node_id: String,
    /// 8-char prefix for compact UI surfacing (e.g. sidebar lines).
    pub peer_node_id_short: String,
    /// Latest friendly name we have for the peer; falls back to short id.
    pub name: String,
    /// First few characters of the most recent message body (≤ 80 chars).
    pub last_message_preview: Option<String>,
    /// Timestamp of the most recent message (sent or received).
    pub last_message_ts_ms: Option<i64>,
    /// Number of received messages since `mark_read`.
    pub unread_count: i64,
    /// Cached X25519 static public key (64-char lowercase hex), if known.
    pub x25519_pubkey: Option<String>,
}

/// Wraps a single SQLite connection guarded by a `std::sync::Mutex`. The
/// daemon serializes writes through the mutex, which is acceptable for the
/// expected message volume (interactive chat).
pub struct MessagingStorage {
    conn: Mutex<Connection>,
}

impl MessagingStorage {
    /// Open or create the database file and apply the canonical schema.
    pub fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create messages dir {}", parent.display()))?;
        }

        let conn = Connection::open(&path)
            .with_context(|| format!("open messages db at {}", path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(&path, perms);
            }
        }

        conn.execute_batch(SCHEMA)
            .context("apply messages schema")?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Persist a brand-new message row. Caller is responsible for choosing
    /// the appropriate initial `status` (`Pending` for outbound, `Delivered`
    /// for inbound).
    pub fn insert_message(&self, m: &MessageRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        /* PERF: Leverage rusqlite statement cache */
        conn.prepare_cached("INSERT OR REPLACE INTO messages \
             (id, peer_node_id, direction, body, timestamp_ms, status, failure_reason, delivered_at_ms, read_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)")?.execute(params![
                m.id,
                m.peer_node_id,
                m.direction.as_str(),
                m.body,
                m.timestamp_ms,
                m.status.as_str(),
                m.failure_reason,
                m.delivered_at_ms,
                m.read_at_ms,
            ])?;
        Ok(())
    }

    /// Refresh the `conversations_meta` row after a local outbound send.
    pub fn bump_conversation_after_local_send(
        &self,
        peer_id_hex: &str,
        message_id_hex: &str,
        ts_ms: i64,
        body: &str,
    ) -> Result<()> {
        let preview = preview_of(body);
        let conn = self.conn.lock().unwrap();
        /* PERF: Leverage rusqlite statement cache */
        conn.prepare_cached(
            "INSERT INTO conversations_meta (peer_node_id, unread_count, last_read_message_id, \
                last_message_id, last_message_preview, last_message_ts_ms) \
             VALUES (?1, 0, NULL, ?2, ?3, ?4) \
             ON CONFLICT(peer_node_id) DO UPDATE SET \
                last_message_id = excluded.last_message_id, \
                last_message_preview = excluded.last_message_preview, \
                last_message_ts_ms = excluded.last_message_ts_ms",
        )?
        .execute(params![peer_id_hex, message_id_hex, preview, ts_ms])?;
        Ok(())
    }

    /// Refresh the `conversations_meta` row after an inbound message and
    /// return the resulting partial summary (the service layer fills in
    /// `name` and `x25519_pubkey` from the peer directory).
    pub fn bump_conversation_after_remote_receive(
        &self,
        peer_id_hex: &str,
        message_id_hex: &str,
        ts_ms: i64,
        body: &str,
    ) -> Result<ConversationBump> {
        let preview = preview_of(body);
        let conn = self.conn.lock().unwrap();

        /* PERF: Leverage rusqlite statement cache */
        conn.prepare_cached(
            "INSERT INTO conversations_meta (peer_node_id, unread_count, last_message_id, \
                last_message_preview, last_message_ts_ms) \
             VALUES (?1, 1, ?2, ?3, ?4) \
             ON CONFLICT(peer_node_id) DO UPDATE SET \
                unread_count = unread_count + 1, \
                last_message_id = excluded.last_message_id, \
                last_message_preview = excluded.last_message_preview, \
                last_message_ts_ms = excluded.last_message_ts_ms",
        )?
        .execute(params![peer_id_hex, message_id_hex, preview, ts_ms])?;

        let unread_count: i64 = /* PERF: Leverage rusqlite statement cache */
conn.prepare_cached("SELECT unread_count FROM conversations_meta WHERE peer_node_id = ?1")?.query_row(params![peer_id_hex], |row| row.get::<_,i64>(0),)?;

        Ok(ConversationBump {
            preview,
            unread_count,
        })
    }

    /// Update the lifecycle status of an existing outbound message and the
    /// optional delivered/read timestamps.
    pub fn set_message_status(
        &self,
        message_id_hex: &str,
        status: MessageStatus,
        delivered_at_ms: Option<i64>,
        read_at_ms: Option<i64>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        match (delivered_at_ms, read_at_ms) {
            (Some(d), Some(r)) => {
                /* PERF: Leverage rusqlite statement cache */
                conn.prepare_cached("UPDATE messages SET status = ?1, delivered_at_ms = ?2, read_at_ms = ?3 WHERE id = ?4")?.execute(params![status.as_str(), d, r, message_id_hex])?;
            }
            (Some(d), None) => {
                /* PERF: Leverage rusqlite statement cache */
                conn.prepare_cached("UPDATE messages SET status = ?1, delivered_at_ms = COALESCE(delivered_at_ms, ?2) WHERE id = ?3")?.execute(params![status.as_str(), d, message_id_hex])?;
            }
            (None, Some(r)) => {
                /* PERF: Leverage rusqlite statement cache */
                conn.prepare_cached("UPDATE messages SET status = ?1, read_at_ms = COALESCE(read_at_ms, ?2) WHERE id = ?3")?.execute(params![status.as_str(), r, message_id_hex])?;
            }
            (None, None) => {
                /* PERF: Leverage rusqlite statement cache */
                conn.prepare_cached("UPDATE messages SET status = ?1 WHERE id = ?2")?
                    .execute(params![status.as_str(), message_id_hex])?;
            }
        }
        Ok(())
    }

    /// Set `status = failed` and persist a human-readable reason.
    pub fn set_message_failed(&self, message_id_hex: &str, reason: &str, at_ms: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        /* PERF: Leverage rusqlite statement cache */
        conn.prepare_cached("UPDATE messages SET status = 'failed', failure_reason = ?1, delivered_at_ms = COALESCE(delivered_at_ms, ?2) WHERE id = ?3")?.execute(params![reason, at_ms, message_id_hex])?;
        Ok(())
    }

    /// Page through a peer's history newest-first.
    pub fn history(
        &self,
        peer_id_hex: &str,
        before_ts_ms: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<MessageRecord>, bool)> {
        let conn = self.conn.lock().unwrap();
        let limit_plus = limit.saturating_add(1).max(2);

        let mut stmt = match before_ts_ms {
            Some(_) => /* PERF: Leverage rusqlite statement cache */ conn.prepare_cached(
                "SELECT id, peer_node_id, direction, body, timestamp_ms, status, failure_reason, delivered_at_ms, read_at_ms \
                 FROM messages \
                 WHERE peer_node_id = ?1 AND timestamp_ms < ?2 \
                 ORDER BY timestamp_ms DESC, id DESC \
                 LIMIT ?3",
            )?,
            None => /* PERF: Leverage rusqlite statement cache */ conn.prepare_cached(
                "SELECT id, peer_node_id, direction, body, timestamp_ms, status, failure_reason, delivered_at_ms, read_at_ms \
                 FROM messages \
                 WHERE peer_node_id = ?1 \
                 ORDER BY timestamp_ms DESC, id DESC \
                 LIMIT ?2",
            )?,
        };

        let rows = match before_ts_ms {
            Some(ts) => stmt.query_map(params![peer_id_hex, ts, limit_plus], parse_row)?,
            None => stmt.query_map(params![peer_id_hex, limit_plus], parse_row)?,
        };

        let mut out: Vec<MessageRecord> = rows.collect::<Result<Vec<_>, _>>()?;
        let has_more = out.len() as i64 > limit;
        if has_more {
            out.truncate(limit as usize);
        }
        Ok((out, has_more))
    }

    /// Snapshot of all conversations, sorted by most-recent activity.
    /// `name` and `x25519_pubkey` are left blank/None — the service
    /// layer fills them in from the daemon's peer directory.
    pub fn list_conversations_raw(&self) -> Result<Vec<ConversationSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = /* PERF: Leverage rusqlite statement cache */ conn.prepare_cached(
            "SELECT peer_node_id, last_message_preview, last_message_ts_ms, unread_count \
             FROM conversations_meta \
             ORDER BY last_message_ts_ms DESC NULLS LAST",
        )?;

        let rows = stmt
            .query_map([], |row| {
                let peer_node_id: String = row.get(0)?;
                let preview: Option<String> = row.get(1)?;
                let ts: Option<i64> = row.get(2)?;
                let unread: i64 = row.get(3)?;
                let short = short_id(&peer_node_id);
                Ok(ConversationSummary {
                    peer_node_id_short: short.clone(),
                    name: short,
                    peer_node_id,
                    last_message_preview: preview,
                    last_message_ts_ms: ts,
                    unread_count: unread,
                    x25519_pubkey: None,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    /// Atomic per-peer wipe of `messages` and `conversations_meta`.
    /// Returns `(deleted_messages, deleted_conversation)` so the caller
    /// can report exact counts to the RPC client.
    pub fn delete_conversation(&self, peer_id_hex: &str) -> Result<(usize, bool)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let deleted_messages = /* PERF: Leverage rusqlite statement cache */
tx.prepare_cached("DELETE FROM messages WHERE peer_node_id = ?1")?.execute(params![peer_id_hex])?;
        let deleted_meta = /* PERF: Leverage rusqlite statement cache */
tx.prepare_cached("DELETE FROM conversations_meta WHERE peer_node_id = ?1")?.execute(params![peer_id_hex])?;
        tx.commit()?;
        Ok((deleted_messages, deleted_meta > 0))
    }

    /// Atomic global wipe of every message + conversation row.
    pub fn delete_all_messages(&self) -> Result<(usize, usize)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let deleted_messages = /* PERF: Leverage rusqlite statement cache */
tx.prepare_cached("DELETE FROM messages")?.execute([])?;
        let deleted_meta = /* PERF: Leverage rusqlite statement cache */
tx.prepare_cached("DELETE FROM conversations_meta")?.execute([])?;
        tx.commit()?;
        Ok((deleted_messages, deleted_meta))
    }

    /// Mark every message at-or-before `up_to_ts_ms` as read for the peer.
    /// Returns the new `unread_count` (always 0).
    pub fn mark_read_up_to(&self, peer_id_hex: &str, up_to_ts_ms: i64) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        /* PERF: Leverage rusqlite statement cache */
        conn.prepare_cached("UPDATE messages SET status = 'read', read_at_ms = COALESCE(read_at_ms, ?2) \
             WHERE peer_node_id = ?1 AND direction = 'received' AND timestamp_ms <= ?2 AND status != 'read'")?.execute(params![peer_id_hex, up_to_ts_ms])?;
        /* PERF: Leverage rusqlite statement cache */
        conn.prepare_cached(
            "UPDATE conversations_meta SET unread_count = 0 WHERE peer_node_id = ?1",
        )?
        .execute(params![peer_id_hex])?;
        Ok(0)
    }

    /// Convenience wrapper used by [`crate::service::MessagingService`]
    /// when handling an `on_peer_forgotten` lifecycle event. Equivalent
    /// to [`Self::delete_conversation`] but keyed by `NodeId` instead of
    /// the hex projection.
    pub fn delete_conversation_for_peer(&self, peer: &NodeId) -> Result<(usize, bool)> {
        let hex = hex_node_id(peer);
        self.delete_conversation(&hex)
    }
}

/// Side-effect summary returned by
/// [`MessagingStorage::bump_conversation_after_remote_receive`]. The
/// service layer combines this with the peer-directory lookup to build
/// the full [`ConversationSummary`] published on the event channel.
#[derive(Debug, Clone)]
pub struct ConversationBump {
    /// Truncated preview as stored in `conversations_meta.last_message_preview`.
    pub preview: String,
    /// Updated `unread_count` for the conversation.
    pub unread_count: i64,
}

fn parse_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageRecord> {
    let direction_str: String = row.get(2)?;
    let status_str: String = row.get(5)?;

    Ok(MessageRecord {
        id: row.get(0)?,
        peer_node_id: row.get(1)?,
        direction: MessageDirection::from_str(&direction_str)
            .map_err(|e| rusqlite::Error::InvalidColumnName(e.to_string()))?,
        body: row.get(3)?,
        timestamp_ms: row.get(4)?,
        status: MessageStatus::from_str(&status_str)
            .map_err(|e| rusqlite::Error::InvalidColumnName(e.to_string()))?,
        failure_reason: row.get(6)?,
        delivered_at_ms: row.get(7)?,
        read_at_ms: row.get(8)?,
    })
}

fn preview_of(body: &str) -> String {
    let truncated: String = body.chars().take(80).collect();
    truncated.replace('\n', " ")
}

pub(crate) fn short_id(hex: &str) -> String {
    hex.chars().take(8).collect()
}
