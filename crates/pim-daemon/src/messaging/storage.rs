//! SQLite-backed persistence for the messaging subsystem.
//!
//! All blocking calls are funneled through `tokio::task::spawn_blocking`
//! by the surrounding [`super::MessagingState`] facade, so handlers
//! never call into rusqlite from an async context directly.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use pim_core::NodeId;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use super::hex_node_id;

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

/// Acknowledgement category transmitted on the wire (`MessageAck` frame).
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
    /// Wall-clock at sender (`timestamp_ms` from the `Message` frame
    /// for `received`, local clock for `sent`).
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
/// on the conversations list query.
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
    /// Populated from `peers_seen` so the UI's identity card can show the
    /// key for offline / known-only peers without an extra round-trip.
    pub x25519_pubkey: Option<String>,
}

/// Outcome of [`MessagingStorage::forget_peer`]. Reported back through
/// `peers.forget` so the UI can surface exact counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetPeerOutcome {
    /// True when a `peers_seen` row existed and was removed.
    pub forgot_identity: bool,
    /// Number of `messages` rows deleted (only non-zero when the
    /// caller asked to also wipe message history).
    pub deleted_messages: usize,
    /// True when a `conversations_meta` row existed and was removed.
    pub deleted_conversation: bool,
}

/// Outcome of an out-of-band identity import
/// ([`MessagingStorage::import_peer_identity_if_compatible`]).
///
/// Returned in lieu of a plain `bool` so the RPC layer can distinguish
/// "first-time insert", "redundant idempotent import", and "user must
/// resolve the conflict before we silently rewrite the keystore".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    /// `node_id` was not previously cached — a fresh row was created.
    Inserted,
    /// `node_id` was already cached with the same `x25519_pub`. The
    /// row's `last_seen_ms` (and optionally `last_known_name`) was
    /// refreshed; no key material changed.
    Refreshed,
    /// `node_id` is already cached with a *different* `x25519_pub`. The
    /// stored row was left untouched. The hex-encoded existing key is
    /// returned so callers can surface a meaningful diagnostic.
    KeyMismatch { existing_x25519_hex: String },
}

/// Wraps a single SQLite connection guarded by a `std::sync::Mutex`. The
/// daemon serializes writes through the mutex, which is acceptable for the
/// expected message volume (interactive chat). All work happens inside
/// `spawn_blocking` from the caller side.
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

    /// Insert a peer's identity if missing, otherwise refresh its `name`
    /// and `last_seen_ms`. Returns `true` when the row was newly inserted.
    pub fn upsert_peer_seen(
        &self,
        peer: &NodeId,
        x25519_pub: &[u8; 32],
        name: &str,
        now_ms: i64,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let peer_hex = hex_node_id(peer);
        let existing: Option<(String, i64)> = conn
            .query_row(
                "SELECT last_known_name, first_seen_ms FROM peers_seen WHERE node_id = ?1",
                params![peer_hex],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .ok();

        match existing {
            None => {
                conn.execute(
                    "INSERT INTO peers_seen (node_id, x25519_pub, last_known_name, first_seen_ms, last_seen_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?4)",
                    params![peer_hex, x25519_pub.as_slice(), name, now_ms],
                )?;
                Ok(true)
            }
            Some(_) => {
                conn.execute(
                    "UPDATE peers_seen SET x25519_pub = ?2, last_known_name = ?3, last_seen_ms = ?4 \
                     WHERE node_id = ?1",
                    params![peer_hex, x25519_pub.as_slice(), name, now_ms],
                )?;
                Ok(false)
            }
        }
    }

    /// Insert or refresh a peer's identity ONLY if the supplied
    /// `x25519_pub` matches any existing cached value. Returns
    /// `ImportOutcome::Inserted` when the row is new,
    /// `ImportOutcome::Refreshed` when an identical row already
    /// existed (timestamps/name updated), or
    /// `ImportOutcome::KeyMismatch` when `node_id` is already cached
    /// with a different x25519 key. The mismatch case yields the
    /// existing key as hex so the caller can surface it to the user.
    ///
    /// `name_if_set` is only applied when `Some(_)` AND non-empty —
    /// callers that wish to preserve an existing label should pass
    /// `None`.
    pub fn import_peer_identity_if_compatible(
        &self,
        peer: &NodeId,
        x25519_pub: &[u8; 32],
        name_if_set: Option<&str>,
        now_ms: i64,
    ) -> Result<ImportOutcome> {
        let conn = self.conn.lock().unwrap();
        let peer_hex = hex_node_id(peer);
        let existing: Option<Vec<u8>> = conn
            .query_row(
                "SELECT x25519_pub FROM peers_seen WHERE node_id = ?1",
                params![peer_hex],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .ok();

        match existing {
            None => {
                let name = name_if_set.unwrap_or("");
                conn.execute(
                    "INSERT INTO peers_seen (node_id, x25519_pub, last_known_name, first_seen_ms, last_seen_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?4)",
                    params![peer_hex, x25519_pub.as_slice(), name, now_ms],
                )?;
                Ok(ImportOutcome::Inserted)
            }
            Some(bytes) if bytes.as_slice() == x25519_pub.as_slice() => {
                // Same key. Refresh last_seen_ms; only overwrite name if
                // the caller supplied a non-empty replacement.
                if let Some(name) = name_if_set.filter(|s| !s.is_empty()) {
                    conn.execute(
                        "UPDATE peers_seen SET last_known_name = ?2, last_seen_ms = ?3 WHERE node_id = ?1",
                        params![peer_hex, name, now_ms],
                    )?;
                } else {
                    conn.execute(
                        "UPDATE peers_seen SET last_seen_ms = ?2 WHERE node_id = ?1",
                        params![peer_hex, now_ms],
                    )?;
                }
                Ok(ImportOutcome::Refreshed)
            }
            Some(bytes) => Ok(ImportOutcome::KeyMismatch {
                existing_x25519_hex: hex32(&bytes),
            }),
        }
    }

    /// Snapshot every known peer's X25519 public key as hex. Used by the
    /// RPC peer-summary builder to attach `x25519_pubkey` without a
    /// per-peer SQLite round-trip. Keyed by 32-char node_id hex.
    pub fn list_known_x25519_pubs(&self) -> Result<std::collections::HashMap<String, String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT node_id, x25519_pub FROM peers_seen")?;
        let rows = stmt
            .query_map([], |row| {
                let node_id: String = row.get(0)?;
                let bytes: Vec<u8> = row.get(1)?;
                Ok((node_id, bytes))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = std::collections::HashMap::with_capacity(rows.len());
        for (node_id, bytes) in rows {
            if bytes.len() == 32 {
                out.insert(node_id, hex32(&bytes));
            }
        }
        Ok(out)
    }

    /// Look up a peer's cached X25519 static public key, if known.
    pub fn lookup_x25519_pub(&self, peer: &NodeId) -> Result<Option<[u8; 32]>> {
        let conn = self.conn.lock().unwrap();
        let peer_hex = hex_node_id(peer);
        let row: Option<Vec<u8>> = conn
            .query_row(
                "SELECT x25519_pub FROM peers_seen WHERE node_id = ?1",
                params![peer_hex],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .ok();
        match row {
            Some(bytes) if bytes.len() == 32 => {
                let mut out = [0u8; 32];
                out.copy_from_slice(&bytes);
                Ok(Some(out))
            }
            _ => Ok(None),
        }
    }

    /// Most recent friendly name observed for the peer, if any.
    pub fn lookup_peer_name(&self, peer: &NodeId) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let peer_hex = hex_node_id(peer);
        Ok(conn
            .query_row(
                "SELECT last_known_name FROM peers_seen WHERE node_id = ?1",
                params![peer_hex],
                |row| row.get::<_, String>(0),
            )
            .ok())
    }

    /// Persist a brand-new message row. Caller is responsible for choosing
    /// the appropriate initial `status` (`Pending` for outbound, `Delivered`
    /// for inbound).
    pub fn insert_message(&self, m: &MessageRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO messages \
             (id, peer_node_id, direction, body, timestamp_ms, status, failure_reason, delivered_at_ms, read_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                m.id,
                m.peer_node_id,
                m.direction.as_str(),
                m.body,
                m.timestamp_ms,
                m.status.as_str(),
                m.failure_reason,
                m.delivered_at_ms,
                m.read_at_ms,
            ],
        )?;
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
        conn.execute(
            "INSERT INTO conversations_meta (peer_node_id, unread_count, last_read_message_id, \
                last_message_id, last_message_preview, last_message_ts_ms) \
             VALUES (?1, 0, NULL, ?2, ?3, ?4) \
             ON CONFLICT(peer_node_id) DO UPDATE SET \
                last_message_id = excluded.last_message_id, \
                last_message_preview = excluded.last_message_preview, \
                last_message_ts_ms = excluded.last_message_ts_ms",
            params![peer_id_hex, message_id_hex, preview, ts_ms],
        )?;
        Ok(())
    }

    /// Refresh the `conversations_meta` row after an inbound message and
    /// return the resulting summary (so the caller can broadcast it).
    pub fn bump_conversation_after_remote_receive(
        &self,
        peer_id_hex: &str,
        message_id_hex: &str,
        ts_ms: i64,
        body: &str,
        cached_peer_name: Option<&str>,
    ) -> Result<ConversationSummary> {
        let preview = preview_of(body);
        let conn = self.conn.lock().unwrap();

        conn.execute(
            "INSERT INTO conversations_meta (peer_node_id, unread_count, last_message_id, \
                last_message_preview, last_message_ts_ms) \
             VALUES (?1, 1, ?2, ?3, ?4) \
             ON CONFLICT(peer_node_id) DO UPDATE SET \
                unread_count = unread_count + 1, \
                last_message_id = excluded.last_message_id, \
                last_message_preview = excluded.last_message_preview, \
                last_message_ts_ms = excluded.last_message_ts_ms",
            params![peer_id_hex, message_id_hex, preview, ts_ms],
        )?;

        let unread_count: i64 = conn.query_row(
            "SELECT unread_count FROM conversations_meta WHERE peer_node_id = ?1",
            params![peer_id_hex],
            |row| row.get::<_, i64>(0),
        )?;

        let name = cached_peer_name
            .map(|s| s.to_owned())
            .or_else(|| {
                conn.query_row(
                    "SELECT last_known_name FROM peers_seen WHERE node_id = ?1",
                    params![peer_id_hex],
                    |row| row.get::<_, String>(0),
                )
                .ok()
            })
            .unwrap_or_else(|| short_id(peer_id_hex));

        let x25519_pubkey: Option<String> = conn
            .query_row(
                "SELECT x25519_pub FROM peers_seen WHERE node_id = ?1",
                params![peer_id_hex],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .ok()
            .and_then(|bytes| {
                if bytes.len() == 32 {
                    Some(hex32(&bytes))
                } else {
                    None
                }
            });

        Ok(ConversationSummary {
            peer_node_id: peer_id_hex.to_owned(),
            peer_node_id_short: short_id(peer_id_hex),
            name,
            last_message_preview: Some(preview),
            last_message_ts_ms: Some(ts_ms),
            unread_count,
            x25519_pubkey,
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
                conn.execute(
                    "UPDATE messages SET status = ?1, delivered_at_ms = ?2, read_at_ms = ?3 WHERE id = ?4",
                    params![status.as_str(), d, r, message_id_hex],
                )?;
            }
            (Some(d), None) => {
                conn.execute(
                    "UPDATE messages SET status = ?1, delivered_at_ms = COALESCE(delivered_at_ms, ?2) WHERE id = ?3",
                    params![status.as_str(), d, message_id_hex],
                )?;
            }
            (None, Some(r)) => {
                conn.execute(
                    "UPDATE messages SET status = ?1, read_at_ms = COALESCE(read_at_ms, ?2) WHERE id = ?3",
                    params![status.as_str(), r, message_id_hex],
                )?;
            }
            (None, None) => {
                conn.execute(
                    "UPDATE messages SET status = ?1 WHERE id = ?2",
                    params![status.as_str(), message_id_hex],
                )?;
            }
        }
        Ok(())
    }

    /// Set `status = failed` and persist a human-readable reason.
    pub fn set_message_failed(&self, message_id_hex: &str, reason: &str, at_ms: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE messages SET status = 'failed', failure_reason = ?1, delivered_at_ms = COALESCE(delivered_at_ms, ?2) WHERE id = ?3",
            params![reason, at_ms, message_id_hex],
        )?;
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
            Some(_) => conn.prepare(
                "SELECT id, peer_node_id, direction, body, timestamp_ms, status, failure_reason, delivered_at_ms, read_at_ms \
                 FROM messages \
                 WHERE peer_node_id = ?1 AND timestamp_ms < ?2 \
                 ORDER BY timestamp_ms DESC, id DESC \
                 LIMIT ?3",
            )?,
            None => conn.prepare(
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
    pub fn list_conversations(&self) -> Result<Vec<ConversationSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT cm.peer_node_id, ps.last_known_name, cm.last_message_preview, cm.last_message_ts_ms, cm.unread_count, ps.x25519_pub \
             FROM conversations_meta cm \
             LEFT JOIN peers_seen ps ON ps.node_id = cm.peer_node_id \
             ORDER BY cm.last_message_ts_ms DESC NULLS LAST",
        )?;

        let rows = stmt
            .query_map([], |row| {
                let peer_node_id: String = row.get(0)?;
                let name: Option<String> = row.get(1)?;
                let preview: Option<String> = row.get(2)?;
                let ts: Option<i64> = row.get(3)?;
                let unread: i64 = row.get(4)?;
                let x25519_bytes: Option<Vec<u8>> = row.get(5)?;
                let x25519_pubkey = x25519_bytes.and_then(|bytes| {
                    if bytes.len() == 32 {
                        Some(hex32(&bytes))
                    } else {
                        None
                    }
                });
                let short = short_id(&peer_node_id);
                Ok(ConversationSummary {
                    peer_node_id_short: short.clone(),
                    name: name.unwrap_or(short),
                    peer_node_id,
                    last_message_preview: preview,
                    last_message_ts_ms: ts,
                    unread_count: unread,
                    x25519_pubkey,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    /// Atomic per-peer wipe of `messages` and `conversations_meta`.
    /// Returns `(deleted_messages, deleted_conversation)` so the
    /// caller can report exact counts to the RPC client.
    /// Does NOT touch `peers_seen` — the cached x25519 stays so the
    /// peer remains messageable.
    pub fn delete_conversation(&self, peer_id_hex: &str) -> Result<(usize, bool)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let deleted_messages = tx.execute(
            "DELETE FROM messages WHERE peer_node_id = ?1",
            params![peer_id_hex],
        )?;
        let deleted_meta = tx.execute(
            "DELETE FROM conversations_meta WHERE peer_node_id = ?1",
            params![peer_id_hex],
        )?;
        tx.commit()?;
        Ok((deleted_messages, deleted_meta > 0))
    }

    /// Atomic global wipe of every message + conversation row.
    /// Identities in `peers_seen` are preserved — that's a separate
    /// "factory reset" we don't expose yet. Returns
    /// `(deleted_messages, deleted_conversations)`.
    pub fn delete_all_messages(&self) -> Result<(usize, usize)> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let deleted_messages = tx.execute("DELETE FROM messages", [])?;
        let deleted_meta = tx.execute("DELETE FROM conversations_meta", [])?;
        tx.commit()?;
        Ok((deleted_messages, deleted_meta))
    }

    /// Drop a peer's identity row from `peers_seen`. Returns whether
    /// a row was actually removed (false ⇒ peer was already unknown).
    /// `also_delete_messages` extends the same transaction with a
    /// per-peer `delete_conversation` so the wipe is atomic across
    /// all three tables.
    pub fn forget_peer(
        &self,
        peer_id_hex: &str,
        also_delete_messages: bool,
    ) -> Result<ForgetPeerOutcome> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let (deleted_messages, deleted_conversation) = if also_delete_messages {
            let m = tx.execute(
                "DELETE FROM messages WHERE peer_node_id = ?1",
                params![peer_id_hex],
            )?;
            let c = tx.execute(
                "DELETE FROM conversations_meta WHERE peer_node_id = ?1",
                params![peer_id_hex],
            )?;
            (m, c > 0)
        } else {
            (0, false)
        };
        let forgot_identity = tx.execute(
            "DELETE FROM peers_seen WHERE node_id = ?1",
            params![peer_id_hex],
        )? > 0;
        tx.commit()?;
        Ok(ForgetPeerOutcome {
            forgot_identity,
            deleted_messages,
            deleted_conversation,
        })
    }

    /// Mark every message at-or-before `up_to_ts_ms` as read for the peer.
    /// Returns the new `unread_count` (always 0).
    pub fn mark_read_up_to(&self, peer_id_hex: &str, up_to_ts_ms: i64) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE messages SET status = 'read', read_at_ms = COALESCE(read_at_ms, ?2) \
             WHERE peer_node_id = ?1 AND direction = 'received' AND timestamp_ms <= ?2 AND status != 'read'",
            params![peer_id_hex, up_to_ts_ms],
        )?;
        conn.execute(
            "UPDATE conversations_meta SET unread_count = 0 WHERE peer_node_id = ?1",
            params![peer_id_hex],
        )?;
        Ok(0)
    }

    /// Iterate every message currently in `pending` status, yielding
    /// `(peer_node_id_hex, message_id_hex, body, timestamp_ms)`. Designed
    /// for the future retry-on-restart path; currently unused.
    #[allow(dead_code)]
    pub fn pending_outbound(&self) -> Result<Vec<(String, String, String, i64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT peer_node_id, id, body, timestamp_ms FROM messages \
             WHERE direction = 'sent' AND status IN ('pending') \
             ORDER BY timestamp_ms ASC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
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

fn short_id(hex: &str) -> String {
    hex.chars().take(8).collect()
}

fn hex32(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open_temp_storage() -> (TempDir, MessagingStorage) {
        let dir = TempDir::new().expect("create tempdir");
        let path = dir.path().join("messages.db");
        let storage = MessagingStorage::open(path).expect("open storage");
        (dir, storage)
    }

    fn node_a() -> NodeId {
        NodeId::from_bytes([0xa1; 16])
    }

    fn node_b() -> NodeId {
        NodeId::from_bytes([0xb2; 16])
    }

    #[test]
    fn import_inserts_when_unknown() {
        let (_d, storage) = open_temp_storage();
        let key = [0x11u8; 32];
        let outcome = storage
            .import_peer_identity_if_compatible(&node_a(), &key, Some("alice"), 100)
            .expect("import");
        assert_eq!(outcome, ImportOutcome::Inserted);
        let cached = storage
            .lookup_x25519_pub(&node_a())
            .expect("lookup")
            .expect("present");
        assert_eq!(cached, key);
        let name = storage
            .lookup_peer_name(&node_a())
            .expect("name")
            .expect("present");
        assert_eq!(name, "alice");
    }

    #[test]
    fn import_refreshes_when_same_key() {
        let (_d, storage) = open_temp_storage();
        let key = [0x22u8; 32];
        storage
            .import_peer_identity_if_compatible(&node_a(), &key, Some("alice"), 100)
            .unwrap();
        let outcome = storage
            .import_peer_identity_if_compatible(&node_a(), &key, Some("alice-renamed"), 200)
            .expect("import");
        assert_eq!(outcome, ImportOutcome::Refreshed);
        let name = storage
            .lookup_peer_name(&node_a())
            .expect("name")
            .expect("present");
        assert_eq!(name, "alice-renamed");
    }

    #[test]
    fn import_preserves_name_when_caller_passes_none() {
        let (_d, storage) = open_temp_storage();
        let key = [0x33u8; 32];
        storage
            .import_peer_identity_if_compatible(&node_a(), &key, Some("alice"), 100)
            .unwrap();
        let outcome = storage
            .import_peer_identity_if_compatible(&node_a(), &key, None, 200)
            .expect("import");
        assert_eq!(outcome, ImportOutcome::Refreshed);
        let name = storage
            .lookup_peer_name(&node_a())
            .expect("name")
            .expect("present");
        assert_eq!(name, "alice");
    }

    #[test]
    fn import_refuses_mismatched_key() {
        let (_d, storage) = open_temp_storage();
        let key1 = [0x44u8; 32];
        let key2 = [0x55u8; 32];
        storage
            .import_peer_identity_if_compatible(&node_a(), &key1, Some("alice"), 100)
            .unwrap();
        let outcome = storage
            .import_peer_identity_if_compatible(&node_a(), &key2, Some("alice"), 200)
            .expect("import");
        match outcome {
            ImportOutcome::KeyMismatch {
                existing_x25519_hex,
            } => {
                assert_eq!(existing_x25519_hex, hex32(&key1));
            }
            other => panic!("expected KeyMismatch, got {other:?}"),
        }
        // Stored key was NOT overwritten.
        let cached = storage
            .lookup_x25519_pub(&node_a())
            .expect("lookup")
            .expect("present");
        assert_eq!(cached, key1);
    }

    #[test]
    fn list_known_x25519_pubs_snapshot() {
        let (_d, storage) = open_temp_storage();
        let key_a = [0x66u8; 32];
        let key_b = [0x77u8; 32];
        storage
            .import_peer_identity_if_compatible(&node_a(), &key_a, Some("alice"), 100)
            .unwrap();
        storage
            .import_peer_identity_if_compatible(&node_b(), &key_b, Some("bob"), 100)
            .unwrap();
        let snapshot = storage.list_known_x25519_pubs().expect("snapshot");
        assert_eq!(snapshot.len(), 2);
        assert_eq!(
            snapshot.get(&hex_node_id(&node_a())).cloned(),
            Some(hex32(&key_a))
        );
        assert_eq!(
            snapshot.get(&hex_node_id(&node_b())).cloned(),
            Some(hex32(&key_b))
        );
    }

    #[test]
    fn list_conversations_includes_x25519_when_cached() {
        let (_d, storage) = open_temp_storage();
        let key = [0x88u8; 32];
        // First, cache the identity.
        storage
            .import_peer_identity_if_compatible(&node_a(), &key, Some("alice"), 100)
            .unwrap();
        // Synthesize an inbound message so a conversations_meta row exists.
        let peer_hex = hex_node_id(&node_a());
        let _ = storage
            .bump_conversation_after_remote_receive(&peer_hex, "deadbeef", 200, "hi", None)
            .unwrap();
        let convos = storage.list_conversations().expect("list");
        let convo = convos
            .iter()
            .find(|c| c.peer_node_id == peer_hex)
            .expect("have one");
        assert_eq!(convo.x25519_pubkey.as_deref(), Some(hex32(&key).as_str()));
    }

    /// Helper: insert one inbound row + bump the conversations_meta
    /// counter for the given peer so the delete tests have something
    /// concrete to wipe.
    fn seed_one_inbound(storage: &MessagingStorage, peer: &NodeId, body: &str, ts: i64, id: &str) {
        let peer_hex = hex_node_id(peer);
        let _ = storage
            .bump_conversation_after_remote_receive(&peer_hex, id, ts, body, None)
            .unwrap();
        // bump_conversation_after_remote_receive doesn't insert into the
        // `messages` table itself; do it explicitly so deletion has a row.
        let record = MessageRecord {
            id: id.to_string(),
            peer_node_id: peer_hex,
            direction: MessageDirection::Received,
            body: body.to_string(),
            timestamp_ms: ts,
            status: MessageStatus::Delivered,
            failure_reason: None,
            delivered_at_ms: Some(ts),
            read_at_ms: None,
        };
        storage.insert_message(&record).expect("insert");
    }

    #[test]
    fn delete_conversation_wipes_messages_and_meta_for_peer() {
        let (_d, storage) = open_temp_storage();
        let key = [0xaau8; 32];
        storage
            .import_peer_identity_if_compatible(&node_a(), &key, Some("alice"), 100)
            .unwrap();
        seed_one_inbound(&storage, &node_a(), "hello", 200, "aaaa1111aaaa1111");
        seed_one_inbound(&storage, &node_a(), "world", 300, "bbbb2222bbbb2222");

        let (deleted_messages, deleted_conversation) = storage
            .delete_conversation(&hex_node_id(&node_a()))
            .unwrap();
        assert_eq!(deleted_messages, 2);
        assert!(deleted_conversation);

        // Cached x25519 identity must remain so the peer stays messageable.
        let cached = storage
            .lookup_x25519_pub(&node_a())
            .expect("lookup")
            .expect("present");
        assert_eq!(cached, key);
        // Conversation list no longer contains the peer.
        let convos = storage.list_conversations().expect("list");
        assert!(convos
            .iter()
            .all(|c| c.peer_node_id != hex_node_id(&node_a())));
        // History page is empty.
        let (rows, _) = storage.history(&hex_node_id(&node_a()), None, 100).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn delete_conversation_on_unknown_peer_is_noop() {
        let (_d, storage) = open_temp_storage();
        let (deleted_messages, deleted_conversation) = storage
            .delete_conversation(&hex_node_id(&node_a()))
            .unwrap();
        assert_eq!(deleted_messages, 0);
        assert!(!deleted_conversation);
    }

    #[test]
    fn delete_all_messages_wipes_every_conversation_but_keeps_identities() {
        let (_d, storage) = open_temp_storage();
        let key_a = [0xbbu8; 32];
        let key_b = [0xccu8; 32];
        storage
            .import_peer_identity_if_compatible(&node_a(), &key_a, Some("alice"), 100)
            .unwrap();
        storage
            .import_peer_identity_if_compatible(&node_b(), &key_b, Some("bob"), 100)
            .unwrap();
        seed_one_inbound(
            &storage,
            &node_a(),
            "hi from alice",
            200,
            "aaaa1111aaaa1111",
        );
        seed_one_inbound(&storage, &node_b(), "hi from bob", 200, "bbbb2222bbbb2222");
        seed_one_inbound(&storage, &node_b(), "again", 250, "cccc3333cccc3333");

        let (deleted_messages, deleted_conversations) = storage.delete_all_messages().unwrap();
        assert_eq!(deleted_messages, 3);
        assert_eq!(deleted_conversations, 2);

        // Identities preserved.
        assert_eq!(storage.lookup_x25519_pub(&node_a()).unwrap(), Some(key_a));
        assert_eq!(storage.lookup_x25519_pub(&node_b()).unwrap(), Some(key_b));
        // Conversation list empty.
        assert!(storage.list_conversations().unwrap().is_empty());
    }

    #[test]
    fn forget_peer_default_keeps_messages() {
        let (_d, storage) = open_temp_storage();
        let key = [0xddu8; 32];
        storage
            .import_peer_identity_if_compatible(&node_a(), &key, Some("alice"), 100)
            .unwrap();
        seed_one_inbound(&storage, &node_a(), "hi", 200, "aaaa1111aaaa1111");

        let outcome = storage.forget_peer(&hex_node_id(&node_a()), false).unwrap();
        assert!(outcome.forgot_identity);
        assert_eq!(outcome.deleted_messages, 0);
        assert!(!outcome.deleted_conversation);

        // Identity is gone …
        assert!(storage.lookup_x25519_pub(&node_a()).unwrap().is_none());
        // … but messages stay (the conversation row falls back to short_id
        // for `name` because the peers_seen JOIN now misses).
        let (rows, _) = storage.history(&hex_node_id(&node_a()), None, 100).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn forget_peer_with_messages_flag_wipes_everything() {
        let (_d, storage) = open_temp_storage();
        let key = [0xeeu8; 32];
        storage
            .import_peer_identity_if_compatible(&node_a(), &key, Some("alice"), 100)
            .unwrap();
        seed_one_inbound(&storage, &node_a(), "hi", 200, "aaaa1111aaaa1111");
        seed_one_inbound(&storage, &node_a(), "again", 300, "bbbb2222bbbb2222");

        let outcome = storage.forget_peer(&hex_node_id(&node_a()), true).unwrap();
        assert!(outcome.forgot_identity);
        assert_eq!(outcome.deleted_messages, 2);
        assert!(outcome.deleted_conversation);

        assert!(storage.lookup_x25519_pub(&node_a()).unwrap().is_none());
        let (rows, _) = storage.history(&hex_node_id(&node_a()), None, 100).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn forget_peer_unknown_returns_false_flags() {
        let (_d, storage) = open_temp_storage();
        let outcome = storage.forget_peer(&hex_node_id(&node_a()), true).unwrap();
        assert!(!outcome.forgot_identity);
        assert_eq!(outcome.deleted_messages, 0);
        assert!(!outcome.deleted_conversation);
    }
}
