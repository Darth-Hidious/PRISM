//! Diesel record for the `sync_outbox` table.
//!
//! Write-through retry queue for the conversation mirror. After each local
//! commit of one or more `messages` rows, the writer enqueues a row here
//! describing the `[low_ordinal, high_ordinal]` slice for a conversation.
//! A background worker pops the lowest `id`, fetches the slice, POSTs to
//! `/v1/conversations/{id}/messages`, and deletes the outbox row on
//! success. On failure it bumps `attempts` and updates `last_error` +
//! `last_attempt_at`. The worker, retry policy, and TUI surfacing of
//! poisoned rows (cap of 10 attempts per spec) are out of scope for this
//! migration — only the storage shape lands here.
//!
//! Dormant alongside `MessageRecord` until the drain worker lands; see that
//! file for the `dead_code` rationale.

#![allow(dead_code)]

use crate::database::schema::sync_outbox;

/// Database model for an existing `sync_outbox` row (read path). The `id`
/// column is `INTEGER PRIMARY KEY AUTOINCREMENT`; on insert use
/// [`NewSyncOutboxRecord`], which omits `id` so SQLite assigns it.
#[derive(Debug, Clone, diesel::Queryable, diesel::Selectable, diesel::AsChangeset)]
#[diesel(table_name = sync_outbox)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct SyncOutboxRecord {
    pub id: i32,
    pub conversation_id: String,
    pub low_ordinal: i64,
    pub high_ordinal: i64,
    pub attempts: i32,
    pub last_attempt_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
}

/// Insert-only shape that lets SQLite assign the AUTOINCREMENT id.
#[derive(Debug, Clone, diesel::Insertable)]
#[diesel(table_name = sync_outbox)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub(crate) struct NewSyncOutboxRecord {
    pub conversation_id: String,
    pub low_ordinal: i64,
    pub high_ordinal: i64,
    pub attempts: i32,
    pub last_attempt_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
}
