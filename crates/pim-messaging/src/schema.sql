-- pim-messaging on-disk schema (messages.db).
--
-- Identity cache (x25519 + names) lives in the daemon-owned peer
-- directory, NOT here, so the daemon can run without the messaging
-- plugin compiled in.

CREATE TABLE IF NOT EXISTS messages (
    id                TEXT PRIMARY KEY,
    peer_node_id      TEXT NOT NULL,
    direction         TEXT NOT NULL CHECK (direction IN ('sent', 'received')),
    body              TEXT NOT NULL,
    timestamp_ms      INTEGER NOT NULL,
    status            TEXT NOT NULL CHECK (status IN ('pending','sent','delivered','read','failed')),
    failure_reason    TEXT,
    delivered_at_ms   INTEGER,
    read_at_ms        INTEGER
);

CREATE INDEX IF NOT EXISTS idx_messages_peer_ts
    ON messages (peer_node_id, timestamp_ms DESC);

-- Denormalized per-conversation state used by `messages.list_conversations`
-- so the conversations view doesn't aggregate the full messages table on
-- every query.
CREATE TABLE IF NOT EXISTS conversations_meta (
    peer_node_id          TEXT PRIMARY KEY,
    unread_count          INTEGER NOT NULL DEFAULT 0,
    last_read_message_id  TEXT,
    last_message_id       TEXT,
    last_message_preview  TEXT,
    last_message_ts_ms    INTEGER
);
