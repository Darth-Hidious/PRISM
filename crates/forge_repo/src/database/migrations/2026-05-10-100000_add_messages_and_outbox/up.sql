-- Per-message storage (mirror of marc27-core 20260510000045_conversations.sql).
-- ADDITIVE: existing conversations.context blob is untouched and remains the
-- belt-and-braces hydration source. These tables introduce per-message
-- storage so we can stop O(n^2) full-blob rewrites and sync per-turn
-- write-throughs to the server.

CREATE TABLE IF NOT EXISTS messages (
    id                TEXT PRIMARY KEY NOT NULL,
    conversation_id   TEXT NOT NULL,
    ordinal           BIGINT NOT NULL,
    role              TEXT NOT NULL CHECK (role IN ('system','user','assistant','tool')),
    content           TEXT,
    tool_calls_json   TEXT,
    tool_results_json TEXT,
    usage_json        TEXT,
    created_at        BIGINT NOT NULL,
    UNIQUE (conversation_id, ordinal)
);

CREATE INDEX IF NOT EXISTS idx_messages_conversation_ordinal
    ON messages (conversation_id, ordinal DESC);

-- Write-through retry queue. Drain worker pops the lowest id, fetches the
-- [low_ordinal, high_ordinal] slice from `messages`, POSTs to
-- /v1/conversations/{id}/messages, and deletes on success. Failed attempts
-- bump `attempts` + `last_error` + `last_attempt_at`.
CREATE TABLE IF NOT EXISTS sync_outbox (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL,
    low_ordinal     BIGINT NOT NULL,
    high_ordinal    BIGINT NOT NULL,
    attempts        INTEGER NOT NULL DEFAULT 0,
    last_attempt_at BIGINT,
    last_error      TEXT,
    created_at      BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_sync_outbox_created_at
    ON sync_outbox (created_at);
