//! Diesel record for the `messages` table.
//!
//! Mirrors the marc27-core server schema in `20260510000045_conversations.sql`
//! (UUID -> TEXT on SQLite, JSONB -> TEXT-storing-JSON, TIMESTAMPTZ -> BIGINT
//! unix epoch ms). This is append-only per-message storage that runs
//! alongside the legacy `conversations.context` blob — both are written
//! during the v2.7.2 transition and the blob remains the canonical
//! hydration source until the per-message read path is wired up.
//!
//! The record only models the storage shape. Conversion to/from
//! `forge_domain::ContextMessage` lives at the call site — fan-out from a
//! single `Context` into a row stream needs ordinal allocation that this
//! type doesn't own.
//!
//! Currently dormant — wiring the read/write paths to these types lands in
//! a follow-up PR. `dead_code` is allowed module-wide so the build stays
//! warning-free while the schema settles.

#![allow(dead_code)]

use crate::database::schema::messages;

/// Allowed values for `messages.role`. The CHECK constraint in DDL is the
/// source of truth; this enum exists so call sites don't fat-finger the
/// string. Keep the `as_str` mapping in sync with the migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

impl MessageRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

/// Database model for the `messages` table.
///
/// Field-by-field mirror of the server schema:
/// - `id` — UUID rendered as TEXT (Diesel SQLite convention).
/// - `conversation_id` — soft FK; SQLite enforcement is loose, server
///   enforces the real cascade.
/// - `ordinal` — monotonic per conversation, client-assigned.
///   `(conversation_id, ordinal)` is the idempotent upsert key.
/// - `role` — one of `system|user|assistant|tool` (CHECK in DDL).
/// - `content` / `tool_calls_json` / `tool_results_json` / `usage_json` —
///   nullable; payloads are JSON-encoded TEXT on SQLite (JSONB on Postgres).
///   `tool_results_json` should reference artifact IDs once the artifact
///   table lands; never inline raw bytes.
/// - `created_at` — unix epoch ms. BIGINT instead of `Timestamp` because
///   we need a stable monotonic value for outbox ordering and clock-skew
///   debugging across the local/server pair. The legacy `conversations`
///   table uses `Timestamp` for historical reasons; not propagating here.
#[derive(
    Debug, Clone, diesel::Queryable, diesel::Selectable, diesel::Insertable, diesel::AsChangeset,
)]
#[diesel(table_name = messages)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct MessageRecord {
    pub id: String,
    pub conversation_id: String,
    pub ordinal: i64,
    pub role: String,
    pub content: Option<String>,
    pub tool_calls_json: Option<String>,
    pub tool_results_json: Option<String>,
    pub usage_json: Option<String>,
    pub created_at: i64,
}
