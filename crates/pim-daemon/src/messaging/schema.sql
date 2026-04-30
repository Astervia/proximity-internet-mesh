PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS peers_seen (
    node_id            TEXT PRIMARY KEY,
    x25519_pub         BLOB NOT NULL,
    last_known_name    TEXT,
    first_seen_ms      INTEGER NOT NULL,
    last_seen_ms       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
    id              TEXT PRIMARY KEY,
    peer_node_id    TEXT NOT NULL,
    direction       TEXT NOT NULL CHECK(direction IN ('sent','received')),
    body            TEXT NOT NULL,
    timestamp_ms    INTEGER NOT NULL,
    status          TEXT NOT NULL CHECK(status IN ('pending','sent','delivered','read','failed')),
    failure_reason  TEXT,
    delivered_at_ms INTEGER,
    read_at_ms      INTEGER
);

CREATE INDEX IF NOT EXISTS idx_msg_peer_ts
    ON messages(peer_node_id, timestamp_ms DESC);

CREATE TABLE IF NOT EXISTS conversations_meta (
    peer_node_id          TEXT PRIMARY KEY,
    unread_count          INTEGER NOT NULL DEFAULT 0,
    last_read_message_id  TEXT,
    last_message_id       TEXT,
    last_message_preview  TEXT,
    last_message_ts_ms    INTEGER
);
