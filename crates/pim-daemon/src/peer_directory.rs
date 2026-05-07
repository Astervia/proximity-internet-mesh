//! Daemon-owned peer keystore.
//!
//! Stores `(node_id, x25519_pub, last_known_name, first_seen_ms,
//! last_seen_ms)` for every peer the daemon has ever heard a `PeerInfo`
//! from (direct or routed) plus any out-of-band `peers.import_identity`
//! entries.
//!
//! This is mesh-essential — the daemon's identity-broadcast cycle and
//! the `peers.*` JSON-RPC surface depend on it. It lives in
//! `pim-daemon` (not the messaging plugin) so the daemon can run
//! without messaging compiled in.
//!
//! Plugins read it through [`pim_plugin::PeerDirectory`]; mutations
//! emit [`pim_plugin::PeerDirectoryEvent`] on a broadcast channel
//! both plugins and the JSON-RPC layer subscribe to.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use async_trait::async_trait;
use pim_core::NodeId;
use pim_plugin::{PeerDirectory, PeerDirectoryEvent, PeerInfoSource};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS peers_seen (
    node_id          TEXT PRIMARY KEY,
    x25519_pub       BLOB NOT NULL,
    last_known_name  TEXT NOT NULL DEFAULT '',
    first_seen_ms    INTEGER NOT NULL,
    last_seen_ms     INTEGER NOT NULL
);

-- RFCOMM peer reachability log. Driven by the daemon's RFCOMM event
-- loop: every paired peer the daemon observes (via dial-attempt or
-- inbound session) gets a row; successful sessions update
-- `last_connected_at_s`. Phase 2 cleanup uses
-- `max(first_paired_at_s, last_connected_at_s)` as the freshness
-- horizon. Timestamps are unix seconds, not milliseconds — cleanup
-- thresholds are days, not minutes, so second-precision is plenty
-- and keeps the rows easy to read on `sqlite3 peers.db`.
CREATE TABLE IF NOT EXISTS rfcomm_peer_lifecycle (
    bd_addr               TEXT PRIMARY KEY,
    name                  TEXT NOT NULL,
    first_paired_at_s     INTEGER NOT NULL,
    last_connected_at_s   INTEGER
);
"#;

/// Outcome of an out-of-band identity import via
/// [`PeerDirectoryService::import_identity_if_compatible`].
///
/// Returned in lieu of a plain `bool` so the RPC layer can distinguish
/// "first-time insert", "redundant idempotent import", and "user must
/// resolve the conflict before we silently rewrite the keystore".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ImportOutcome {
    /// `node_id` was not previously cached — a fresh row was created.
    Inserted,
    /// `node_id` was already cached with the same `x25519_pub`. The
    /// row's `last_seen_ms` (and optionally `last_known_name`) was
    /// refreshed; no key material changed.
    Refreshed,
    /// `node_id` is already cached with a *different* `x25519_pub`. The
    /// stored row was left untouched. The hex-encoded existing key is
    /// returned so callers can surface a meaningful diagnostic.
    KeyMismatch {
        /// Hex of the existing cached key.
        existing_x25519_hex: String,
    },
}

/// Outcome of [`PeerDirectoryService::forget`]. Reported back through
/// `peers.forget` so the UI can surface exact counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForgetOutcome {
    /// True when a row existed and was removed.
    pub forgot_identity: bool,
}

/// Daemon-side peer keystore.
pub(crate) struct PeerDirectoryService {
    conn: Mutex<Connection>,
    events_tx: broadcast::Sender<PeerDirectoryEvent>,
}

impl PeerDirectoryService {
    /// Open or create the database file and apply the canonical schema.
    pub fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create peers dir {}", parent.display()))?;
        }

        let conn = Connection::open(&path)
            .with_context(|| format!("open peers db at {}", path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(&path, perms);
            }
        }

        conn.execute_batch(SCHEMA).context("apply peers schema")?;

        let (events_tx, _rx) = broadcast::channel(256);

        Ok(Self {
            conn: Mutex::new(conn),
            events_tx,
        })
    }

    /// Subscribe to identity-state events.
    pub fn subscribe(&self) -> broadcast::Receiver<PeerDirectoryEvent> {
        self.events_tx.subscribe()
    }

    /// Insert a peer's identity if missing, otherwise refresh its `name`
    /// and `last_seen_ms`. Emits `PeerDirectoryEvent::Seen` when
    /// `emit_event` is true.
    pub async fn record_peer_seen(
        self: &std::sync::Arc<Self>,
        peer: NodeId,
        x25519_pub: [u8; 32],
        name: String,
        now_ms: i64,
        source: PeerInfoSource,
        emit_event: bool,
    ) -> Result<bool> {
        let svc = self.clone();
        let storage_name = name.clone();
        let inserted = tokio::task::spawn_blocking(move || -> Result<bool> {
            svc.upsert_peer_seen_blocking(&peer, &x25519_pub, &storage_name, now_ms)
        })
        .await??;

        if emit_event {
            let _ = self.events_tx.send(PeerDirectoryEvent::Seen {
                node_id: peer,
                x25519_pub,
                name,
                via: source,
            });
        }

        Ok(inserted)
    }

    /// Out-of-band identity import. Refuses to silently overwrite an
    /// existing key with a different one — see [`ImportOutcome`] for
    /// the three possible result states.
    pub async fn import_identity_if_compatible(
        self: &std::sync::Arc<Self>,
        peer: NodeId,
        x25519_pub: [u8; 32],
        name_if_set: Option<String>,
        now_ms: i64,
    ) -> Result<ImportOutcome> {
        let svc = self.clone();
        let name_for_storage = name_if_set.clone();
        let outcome = tokio::task::spawn_blocking(move || -> Result<ImportOutcome> {
            svc.import_blocking(&peer, &x25519_pub, name_for_storage.as_deref(), now_ms)
        })
        .await??;

        if matches!(outcome, ImportOutcome::Inserted | ImportOutcome::Refreshed) {
            let svc_for_lookup = self.clone();
            let resolved_name = match name_if_set.filter(|s| !s.is_empty()) {
                Some(n) => n,
                None => {
                    tokio::task::spawn_blocking(move || svc_for_lookup.lookup_name_blocking(&peer))
                        .await
                        .ok()
                        .and_then(|r| r.ok())
                        .flatten()
                        .unwrap_or_default()
                }
            };
            let _ = self.events_tx.send(PeerDirectoryEvent::Seen {
                node_id: peer,
                x25519_pub,
                name: resolved_name,
                via: PeerInfoSource::Direct,
            });
        }

        Ok(outcome)
    }

    /// Drop a peer's identity row. Returns whether a row was actually
    /// removed (false ⇒ peer was already unknown). Emits
    /// `PeerDirectoryEvent::Forgotten` on success.
    pub async fn forget(self: &std::sync::Arc<Self>, peer: NodeId) -> Result<ForgetOutcome> {
        let svc = self.clone();
        let outcome = tokio::task::spawn_blocking(move || -> Result<ForgetOutcome> {
            svc.forget_blocking(&peer)
        })
        .await??;

        if outcome.forgot_identity {
            let _ = self
                .events_tx
                .send(PeerDirectoryEvent::Forgotten { node_id: peer });
        }
        Ok(outcome)
    }

    /// Snapshot every known peer's X25519 public key as hex. Used by
    /// the RPC peer-summary builder.
    pub async fn list_known_x25519_pubs(
        self: &std::sync::Arc<Self>,
    ) -> Result<std::collections::HashMap<String, String>> {
        let svc = self.clone();
        let map = tokio::task::spawn_blocking(move || svc.list_x25519_blocking())
            .await
            .context("storage join")??;
        Ok(map)
    }

    // ── Blocking impls ───────────────────────────────────────────────

    fn upsert_peer_seen_blocking(
        &self,
        peer: &NodeId,
        x25519_pub: &[u8; 32],
        name: &str,
        now_ms: i64,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let peer_hex = hex_node_id(peer);
        let existing: Option<i64> = conn
            .query_row(
                "SELECT first_seen_ms FROM peers_seen WHERE node_id = ?1",
                params![peer_hex],
                |row| row.get::<_, i64>(0),
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

    fn import_blocking(
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

    fn forget_blocking(&self, peer: &NodeId) -> Result<ForgetOutcome> {
        let conn = self.conn.lock().unwrap();
        let peer_hex = hex_node_id(peer);
        let removed = conn.execute(
            "DELETE FROM peers_seen WHERE node_id = ?1",
            params![peer_hex],
        )?;
        Ok(ForgetOutcome {
            forgot_identity: removed > 0,
        })
    }

    fn lookup_x25519_blocking(&self, peer: &NodeId) -> Result<Option<[u8; 32]>> {
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

    fn lookup_name_blocking(&self, peer: &NodeId) -> Result<Option<String>> {
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

    fn list_x25519_blocking(&self) -> Result<std::collections::HashMap<String, String>> {
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

    /// List peers whose `last_seen_ms` is older than `threshold_ms`.
    /// Returned tuples are `(NodeId, last_seen_ms, last_known_name)`
    /// — the name is included for log readability when the cleanup
    /// loop reports what it dropped.
    pub fn list_peers_older_than(&self, threshold_ms: i64) -> Result<Vec<(NodeId, i64, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT node_id, last_seen_ms, last_known_name \
             FROM peers_seen \
             WHERE last_seen_ms < ?1 \
             ORDER BY last_seen_ms ASC",
        )?;
        let rows = stmt
            .query_map(params![threshold_ms], |row| {
                let hex: String = row.get(0)?;
                let ts: i64 = row.get(1)?;
                let name: String = row.get(2)?;
                Ok((hex, ts, name))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut out = Vec::with_capacity(rows.len());
        for (hex, ts, name) in rows {
            match hex.parse::<NodeId>() {
                Ok(id) => out.push((id, ts, name)),
                Err(_) => continue,
            }
        }
        Ok(out)
    }

    // ── RFCOMM peer lifecycle ─────────────────────────────────────────
    //
    // Phase 1 of `plans/rfcomm-reconnect/plan.md`. The
    // `rfcomm_peer_lifecycle` table lives in `peers.db` because it is
    // daemon-essential (reachability, not messaging) — the rest of the
    // peer keystore is the natural neighbour. Connection and lock are
    // shared with `peers_seen` so the daemon never opens a second
    // connection to the same SQLite file.

    /// Record that the daemon observed `bd_addr` in the paired set
    /// with human-readable `name`. The first observation wins:
    /// subsequent calls leave `name` and `first_paired_at_s`
    /// untouched. Always idempotent.
    pub fn observe_rfcomm_paired(&self, bd_addr: &str, name: &str, now_s: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO rfcomm_peer_lifecycle (bd_addr, name, first_paired_at_s) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(bd_addr) DO NOTHING",
            params![bd_addr, name, now_s],
        )?;
        Ok(())
    }

    /// Record that the daemon just completed a successful RFCOMM
    /// handshake with `bd_addr`. Upserts so the row exists even if
    /// the peer has never been observed via the paired-list scan
    /// (e.g. an inbound session from a freshly-paired peer that
    /// hasn't been scanned yet). Updates
    /// `last_connected_at_s = max(existing, now)` so out-of-order
    /// events from concurrent inbound + outbound paths cannot rewind
    /// the timestamp.
    pub fn record_rfcomm_connected(&self, bd_addr: &str, name: &str, now_s: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO rfcomm_peer_lifecycle (bd_addr, name, first_paired_at_s, last_connected_at_s) \
             VALUES (?1, ?2, ?3, ?3) \
             ON CONFLICT(bd_addr) DO UPDATE SET \
                last_connected_at_s = MAX(COALESCE(last_connected_at_s, 0), excluded.last_connected_at_s)",
            params![bd_addr, name, now_s],
        )?;
        Ok(())
    }

    /// Most recent freshness signal for `bd_addr`:
    /// `max(first_paired_at_s, last_connected_at_s)`. `None` when
    /// the peer has never been observed.
    ///
    /// Phase 2 (cleanup loop) is the first production caller; covered
    /// by unit tests today.
    #[allow(dead_code)]
    pub fn rfcomm_last_seen(&self, bd_addr: &str) -> Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT MAX(first_paired_at_s, COALESCE(last_connected_at_s, first_paired_at_s)) \
                 FROM rfcomm_peer_lifecycle WHERE bd_addr = ?1",
                params![bd_addr],
                |row| row.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten())
    }

    /// Snapshot every tracked RFCOMM peer. Cleanup loop in Phase 2
    /// iterates this list; RPC introspection (later) returns it raw.
    #[allow(dead_code)]
    pub fn list_rfcomm_lifecycle(&self) -> Result<Vec<RfcommLifecycleRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT bd_addr, name, first_paired_at_s, last_connected_at_s \
             FROM rfcomm_peer_lifecycle \
             ORDER BY bd_addr ASC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(RfcommLifecycleRecord {
                    bd_addr: row.get(0)?,
                    name: row.get(1)?,
                    first_paired_at_s: row.get(2)?,
                    last_connected_at_s: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Drop a peer from the lifecycle log. Phase 2 cleanup calls
    /// this after a successful `bluetoothctl remove`. No-op when the
    /// row is absent.
    #[allow(dead_code)]
    pub fn forget_rfcomm_peer(&self, bd_addr: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM rfcomm_peer_lifecycle WHERE bd_addr = ?1",
            params![bd_addr],
        )?;
        Ok(n > 0)
    }
}

/// One row from `rfcomm_peer_lifecycle`. Returned by
/// [`PeerDirectoryService::list_rfcomm_lifecycle`] for the cleanup
/// loop and (later) for RPC introspection.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RfcommLifecycleRecord {
    /// `"AA:BB:CC:DD:EE:FF"` (uppercase, colon-separated). Matches
    /// the format produced by `bluetoothctl devices Paired`.
    pub bd_addr: String,
    /// Friendly name observed at first sighting. May be the BlueZ
    /// device name (from a paired-list scan) or the PIM node name
    /// (from a successful handshake) — whichever arrived first.
    /// Cosmetic; cleanup logs use this purely for human readability.
    pub name: String,
    /// Wall-clock seconds when the daemon first saw this paired
    /// peer.
    pub first_paired_at_s: i64,
    /// Wall-clock seconds when the daemon last completed a
    /// successful RFCOMM handshake with this peer. `None` until the
    /// first success.
    pub last_connected_at_s: Option<i64>,
}

/// Adapter implementing [`pim_plugin::PeerDirectory`] over
/// [`PeerDirectoryService`]. Plugins receive this as
/// `Arc<dyn PeerDirectory>` through `PluginContext`.
pub(crate) struct PeerDirectoryAdapter {
    inner: std::sync::Arc<PeerDirectoryService>,
}

impl PeerDirectoryAdapter {
    pub fn new(inner: std::sync::Arc<PeerDirectoryService>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl PeerDirectory for PeerDirectoryAdapter {
    async fn lookup_x25519(&self, peer: &NodeId) -> Option<[u8; 32]> {
        let svc = self.inner.clone();
        let peer_copy = *peer;
        tokio::task::spawn_blocking(move || svc.lookup_x25519_blocking(&peer_copy))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten()
    }

    async fn lookup_name(&self, peer: &NodeId) -> Option<String> {
        let svc = self.inner.clone();
        let peer_copy = *peer;
        tokio::task::spawn_blocking(move || svc.lookup_name_blocking(&peer_copy))
            .await
            .ok()
            .and_then(|r| r.ok())
            .flatten()
    }

    fn subscribe(&self) -> broadcast::Receiver<PeerDirectoryEvent> {
        self.inner.subscribe()
    }
}

/// Format a [`NodeId`] as a 32-char lowercase hex string.
pub(crate) fn hex_node_id(id: &NodeId) -> String {
    let mut out = String::with_capacity(32);
    for b in id.as_bytes() {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

fn hex32(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod rfcomm_lifecycle_tests {
    //! Phase 1 of `plans/rfcomm-reconnect/plan.md`. Covers the
    //! rfcomm-specific methods on `PeerDirectoryService`. The peer-
    //! identity surface is exercised by the broader `app::tests` /
    //! integration suite.

    use super::*;
    use tempfile::TempDir;

    fn open_temp_storage() -> (TempDir, PeerDirectoryService) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("peers.db");
        let storage = PeerDirectoryService::open(path).expect("open storage");
        (dir, storage)
    }

    #[test]
    fn rfcomm_observe_paired_inserts_then_preserves() {
        let (_d, storage) = open_temp_storage();
        storage
            .observe_rfcomm_paired("00:15:83:3D:0A:57", "PIM-foo", 1000)
            .unwrap();
        // Second observation with a different name MUST NOT overwrite.
        storage
            .observe_rfcomm_paired("00:15:83:3D:0A:57", "PIM-renamed", 2000)
            .unwrap();
        let rows = storage.list_rfcomm_lifecycle().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "PIM-foo");
        assert_eq!(rows[0].first_paired_at_s, 1000);
        assert_eq!(rows[0].last_connected_at_s, None);
    }

    #[test]
    fn rfcomm_record_connected_creates_then_updates() {
        let (_d, storage) = open_temp_storage();
        storage
            .record_rfcomm_connected("00:15:83:3D:0A:57", "PIM-foo", 5000)
            .unwrap();
        let last = storage
            .rfcomm_last_seen("00:15:83:3D:0A:57")
            .unwrap()
            .expect("present");
        assert_eq!(last, 5000);

        // A later success bumps last_connected_at.
        storage
            .record_rfcomm_connected("00:15:83:3D:0A:57", "PIM-foo", 6000)
            .unwrap();
        let last = storage
            .rfcomm_last_seen("00:15:83:3D:0A:57")
            .unwrap()
            .expect("present");
        assert_eq!(last, 6000);

        // An earlier success (out-of-order event) MUST NOT rewind.
        storage
            .record_rfcomm_connected("00:15:83:3D:0A:57", "PIM-foo", 5500)
            .unwrap();
        let last = storage
            .rfcomm_last_seen("00:15:83:3D:0A:57")
            .unwrap()
            .expect("present");
        assert_eq!(last, 6000);
    }

    #[test]
    fn rfcomm_last_seen_uses_max_of_paired_and_connected() {
        let (_d, storage) = open_temp_storage();
        storage
            .observe_rfcomm_paired("00:15:83:3D:0A:57", "PIM-foo", 9000)
            .unwrap();
        // Connected with an older timestamp (e.g. clock skew or an
        // out-of-order event from an inbound session that fired before
        // the paired-list scan).
        storage
            .record_rfcomm_connected("00:15:83:3D:0A:57", "PIM-foo", 8000)
            .unwrap();
        let last = storage
            .rfcomm_last_seen("00:15:83:3D:0A:57")
            .unwrap()
            .expect("present");
        assert_eq!(last, 9000);
    }

    #[test]
    fn rfcomm_last_seen_unknown_returns_none() {
        let (_d, storage) = open_temp_storage();
        assert_eq!(storage.rfcomm_last_seen("00:15:83:3D:0A:57").unwrap(), None);
    }

    #[test]
    fn rfcomm_lifecycle_persists_across_reopen() {
        let dir = TempDir::new().expect("create tempdir");
        let path = dir.path().join("peers.db");
        {
            let s = PeerDirectoryService::open(path.clone()).unwrap();
            s.observe_rfcomm_paired("00:15:83:3D:0A:57", "PIM-foo", 100)
                .unwrap();
            s.record_rfcomm_connected("00:15:83:3D:0A:57", "PIM-foo", 200)
                .unwrap();
        }
        // New connection on the same file — schema is `IF NOT EXISTS`,
        // existing rows must survive.
        let s = PeerDirectoryService::open(path).unwrap();
        let rows = s.list_rfcomm_lifecycle().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].first_paired_at_s, 100);
        assert_eq!(rows[0].last_connected_at_s, Some(200));
    }

    #[test]
    fn rfcomm_forget_removes_row() {
        let (_d, storage) = open_temp_storage();
        storage
            .observe_rfcomm_paired("00:15:83:3D:0A:57", "PIM-foo", 100)
            .unwrap();
        assert!(storage.forget_rfcomm_peer("00:15:83:3D:0A:57").unwrap());
        assert!(storage.list_rfcomm_lifecycle().unwrap().is_empty());
        // Subsequent forget on the same address is a no-op.
        assert!(!storage.forget_rfcomm_peer("00:15:83:3D:0A:57").unwrap());
    }
}
