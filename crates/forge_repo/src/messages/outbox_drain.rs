//! Outbox drain — pushes pending message ranges to the MARC27 platform.
//!
//! Step 4 of the conversation server-side mirror (after PR #104 schema +
//! PR #105 dual-write). Reads pending `sync_outbox` rows, fetches the
//! corresponding `messages` rows for each range, POSTs them to
//! `/v1/conversations/{id}/messages`, and deletes the outbox row on
//! success.
//!
//! Server-side endpoint is idempotent on `(conversation_id, ordinal)`,
//! so over-sending (e.g. enqueuing `[0, N-1]` every turn) is harmless —
//! duplicates are silently skipped. The caller still benefits from
//! tracking attempts so we can backoff and surface poisoned rows.
//!
//! Caller responsibility:
//!   - Construct an `OutboxDrain` with HTTP client + base URL + token.
//!   - Call `drain_once(&pool)` periodically (e.g. once per chat turn,
//!     or on a timer in the chat backend). Returns a summary so the
//!     caller can log how many rows landed.
//!
//! What this module does NOT do:
//!   - Auto-firing on a timer. Caller schedules.
//!   - Backfill of conversations created before this code shipped.
//!     The lazy backfill (push on first `prism resume`) is a separate
//!     concern; the drain only handles outbox rows that were enqueued.
//!   - Conversation create. The drain assumes the conversation already
//!     exists on the server — for now that means the caller must POST
//!     `/projects/{pid}/conversations` first. A small enhancement is to
//!     create-on-404 here, but that needs the project_id which the
//!     outbox row doesn't carry. Tracked as a followup.

#![allow(dead_code)] // wired into the chat loop in a follow-up PR

use anyhow::Context as _;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};

use crate::database::PooledSqliteConnection;
use crate::database::schema::{messages, sync_outbox};
use crate::messages::message_record::MessageRecord;
use crate::messages::sync_outbox_record::SyncOutboxRecord;

/// Cap on retries before a row stops being attempted. Per the design
/// spec; surfacing the row to the user happens in a follow-up.
pub const MAX_OUTBOX_ATTEMPTS: i32 = 10;

/// Default batch size — drain up to this many pending outbox rows per
/// `drain_once` call. Tuned for "per chat turn" cadence: small enough
/// to keep the latency invisible, big enough to catch up after a brief
/// network outage.
pub const DEFAULT_BATCH: i64 = 5;

/// Summary of one drain pass. Returned to the caller for logging /
/// metrics, never used to drive control flow.
#[derive(Debug, Clone, Default)]
pub struct DrainSummary {
    /// Outbox rows successfully pushed and deleted.
    pub succeeded: usize,
    /// Outbox rows that failed (attempts bumped, row kept for retry).
    pub failed: usize,
    /// Outbox rows skipped because they exceeded `MAX_OUTBOX_ATTEMPTS`.
    pub poisoned: usize,
}

/// Wire-shape of one message in the request body to
/// `POST /v1/conversations/{id}/messages`. Mirrors the SDK's
/// `MessageInput` shape exactly.
#[derive(Debug, Serialize)]
struct WireMessage {
    id: String,
    ordinal: i64,
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls_json: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_results_json: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage_json: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct PostMessagesBody {
    messages: Vec<WireMessage>,
}

#[derive(Debug, Deserialize)]
struct PostMessagesResponse {
    #[allow(dead_code)] // logged but not driven on
    committed: Vec<i64>,
    #[allow(dead_code)]
    submitted: usize,
}

/// Async drain context. Holds the HTTP client + credentials so the
/// caller can build it once at boot and reuse.
pub struct OutboxDrain {
    http: reqwest::Client,
    api_base: String,
    access_token: String,
}

impl OutboxDrain {
    /// Build a drain context. `api_base` should include the `/api/v1`
    /// prefix (matches PRISM's stored `platform_url` shape).
    pub fn new(api_base: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_base: api_base.into(),
            access_token: access_token.into(),
        }
    }

    /// Drain up to `DEFAULT_BATCH` pending outbox rows in one pass.
    ///
    /// Each row that succeeds is deleted from `sync_outbox`. Each row
    /// that fails has its `attempts` + `last_error` + `last_attempt_at`
    /// updated. Rows that have already exhausted `MAX_OUTBOX_ATTEMPTS`
    /// are counted as poisoned and skipped this pass.
    pub async fn drain_once(
        &self,
        connection: &mut PooledSqliteConnection,
    ) -> anyhow::Result<DrainSummary> {
        self.drain_with_limit(connection, DEFAULT_BATCH).await
    }

    /// Drain up to `limit` rows. Public for tests and scripted operations.
    pub async fn drain_with_limit(
        &self,
        connection: &mut PooledSqliteConnection,
        limit: i64,
    ) -> anyhow::Result<DrainSummary> {
        let pending: Vec<SyncOutboxRecord> = sync_outbox::table
            .order(sync_outbox::id.asc())
            .limit(limit)
            .select(SyncOutboxRecord::as_select())
            .load(connection)
            .context("loading pending outbox rows")?;

        let mut summary = DrainSummary::default();

        for row in pending {
            if row.attempts >= MAX_OUTBOX_ATTEMPTS {
                summary.poisoned += 1;
                tracing::warn!(
                    outbox_id = row.id,
                    conversation_id = %row.conversation_id,
                    attempts = row.attempts,
                    last_error = ?row.last_error,
                    "outbox row poisoned (exceeded MAX_OUTBOX_ATTEMPTS) — skipping"
                );
                continue;
            }

            // Fetch the messages this outbox row references.
            let batch: Vec<MessageRecord> = messages::table
                .filter(messages::conversation_id.eq(&row.conversation_id))
                .filter(messages::ordinal.ge(row.low_ordinal))
                .filter(messages::ordinal.le(row.high_ordinal))
                .order(messages::ordinal.asc())
                .select(MessageRecord::as_select())
                .load(connection)
                .with_context(|| {
                    format!(
                        "loading messages [{}, {}] for conversation {}",
                        row.low_ordinal, row.high_ordinal, row.conversation_id
                    )
                })?;

            // No messages in range = stale outbox row. Delete it; nothing
            // to push. (This shouldn't happen under the dual-write
            // contract but defending against it keeps the drain idempotent
            // against future schema fiddling.)
            if batch.is_empty() {
                diesel::delete(sync_outbox::table.find(row.id))
                    .execute(connection)
                    .ok();
                continue;
            }

            // Build wire shape — JSON columns are TEXT on SQLite, so
            // re-parse to JSON values for the HTTP body.
            let wire_messages: Vec<WireMessage> = batch
                .iter()
                .map(|m| WireMessage {
                    id: m.id.clone(),
                    ordinal: m.ordinal,
                    role: m.role.clone(),
                    content: m.content.clone(),
                    tool_calls_json: m
                        .tool_calls_json
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok()),
                    tool_results_json: m
                        .tool_results_json
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok()),
                    usage_json: m
                        .usage_json
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok()),
                })
                .collect();

            let url = format!(
                "{}/conversations/{}/messages",
                self.api_base.trim_end_matches('/'),
                row.conversation_id
            );

            let result: anyhow::Result<()> = async {
                let resp = self
                    .http
                    .post(&url)
                    .bearer_auth(&self.access_token)
                    .json(&PostMessagesBody {
                        messages: wire_messages,
                    })
                    .send()
                    .await
                    .context("POST /messages")?;

                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(anyhow::anyhow!(
                        "platform returned {status}: {}",
                        body.chars().take(200).collect::<String>()
                    ));
                }

                let _ack: PostMessagesResponse = resp
                    .json()
                    .await
                    .context("decoding /messages response body")?;
                Ok(())
            }
            .await;

            match result {
                Ok(()) => {
                    diesel::delete(sync_outbox::table.find(row.id))
                        .execute(connection)
                        .context("deleting drained outbox row")?;
                    summary.succeeded += 1;
                }
                Err(e) => {
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    diesel::update(sync_outbox::table.find(row.id))
                        .set((
                            sync_outbox::attempts.eq(row.attempts + 1),
                            sync_outbox::last_attempt_at.eq(Some(now_ms)),
                            sync_outbox::last_error
                                .eq(Some(e.to_string().chars().take(500).collect::<String>())),
                        ))
                        .execute(connection)
                        .context("bumping outbox row attempts")?;
                    summary.failed += 1;
                    tracing::warn!(
                        outbox_id = row.id,
                        conversation_id = %row.conversation_id,
                        error = %e,
                        attempts = row.attempts + 1,
                        "outbox push failed; will retry"
                    );
                }
            }
        }

        Ok(summary)
    }
}
