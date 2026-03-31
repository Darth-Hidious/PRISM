//! SQLite-backed append-only audit log for PRISM node operations.
//!
//! Every action (tool execution, data query, config change, subscription event)
//! is recorded as an immutable `AuditEntry`.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Categories of auditable actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditAction {
    ToolExecution,
    DataQuery,
    DataIngest,
    DataPublish,
    ConfigChange,
    UserLogin,
    UserLogout,
    RoleChange,
    NodeStart,
    NodeStop,
    Subscription,
}

impl fmt::Display for AuditAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::ToolExecution => "ToolExecution",
            Self::DataQuery => "DataQuery",
            Self::DataIngest => "DataIngest",
            Self::DataPublish => "DataPublish",
            Self::ConfigChange => "ConfigChange",
            Self::UserLogin => "UserLogin",
            Self::UserLogout => "UserLogout",
            Self::RoleChange => "RoleChange",
            Self::NodeStart => "NodeStart",
            Self::NodeStop => "NodeStop",
            Self::Subscription => "Subscription",
        };
        write!(f, "{s}")
    }
}

impl AuditAction {
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "ToolExecution" => Ok(Self::ToolExecution),
            "DataQuery" => Ok(Self::DataQuery),
            "DataIngest" => Ok(Self::DataIngest),
            "DataPublish" => Ok(Self::DataPublish),
            "ConfigChange" => Ok(Self::ConfigChange),
            "UserLogin" => Ok(Self::UserLogin),
            "UserLogout" => Ok(Self::UserLogout),
            "RoleChange" => Ok(Self::RoleChange),
            "NodeStart" => Ok(Self::NodeStart),
            "NodeStop" => Ok(Self::NodeStop),
            "Subscription" => Ok(Self::Subscription),
            other => anyhow::bail!("unknown AuditAction: {other}"),
        }
    }
}

/// Outcome of an audited operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditOutcome {
    Success,
    Failure,
    Denied,
}

impl fmt::Display for AuditOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Success => "Success",
            Self::Failure => "Failure",
            Self::Denied => "Denied",
        };
        write!(f, "{s}")
    }
}

impl AuditOutcome {
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "Success" => Ok(Self::Success),
            "Failure" => Ok(Self::Failure),
            "Denied" => Ok(Self::Denied),
            other => anyhow::bail!("unknown AuditOutcome: {other}"),
        }
    }
}

/// A single audit record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Auto-increment primary key (set after insertion).
    pub id: i64,
    pub timestamp: DateTime<Utc>,
    pub user_id: String,
    pub action: AuditAction,
    /// What was acted on — tool name, dataset path, config key, etc.
    pub target: String,
    /// Optional additional context (typically a JSON blob).
    pub detail: Option<String>,
    pub outcome: AuditOutcome,
}

/// Filter criteria for querying the audit log.
#[derive(Debug, Default, Clone)]
pub struct AuditFilter {
    pub user_id: Option<String>,
    pub action: Option<AuditAction>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub limit: Option<u32>,
}

// ---------------------------------------------------------------------------
// AuditLog — the SQLite-backed store
// ---------------------------------------------------------------------------

const DEFAULT_LIMIT: u32 = 100;

const CREATE_TABLE: &str = "\
CREATE TABLE IF NOT EXISTS audit_log (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT    NOT NULL,
    user_id   TEXT    NOT NULL,
    action    TEXT    NOT NULL,
    target    TEXT    NOT NULL,
    detail    TEXT,
    outcome   TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_user   ON audit_log(user_id);
CREATE INDEX IF NOT EXISTS idx_audit_action ON audit_log(action);
CREATE INDEX IF NOT EXISTS idx_audit_ts     ON audit_log(timestamp);
";

pub struct AuditLog {
    conn: Connection,
}

impl AuditLog {
    /// Open (or create) the audit database at `db_path`.
    pub fn new<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let conn = Connection::open(db_path).context("opening audit database")?;
        conn.execute_batch(CREATE_TABLE)
            .context("creating audit_log table")?;
        // WAL mode for better concurrent-read performance.
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        Ok(Self { conn })
    }

    /// Create an in-memory audit log (useful for tests).
    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("opening in-memory audit db")?;
        conn.execute_batch(CREATE_TABLE)
            .context("creating audit_log table")?;
        Ok(Self { conn })
    }

    /// Append an entry. Returns the auto-generated row id.
    pub fn log(&self, entry: &AuditEntry) -> Result<i64> {
        let ts = entry.timestamp.to_rfc3339();
        self.conn
            .execute(
                "INSERT INTO audit_log (timestamp, user_id, action, target, detail, outcome)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    ts,
                    entry.user_id,
                    entry.action.to_string(),
                    entry.target,
                    entry.detail,
                    entry.outcome.to_string(),
                ],
            )
            .context("inserting audit entry")?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Query entries matching `filter`, ordered newest-first.
    pub fn query(&self, filter: &AuditFilter) -> Result<Vec<AuditEntry>> {
        let (where_clause, bind_values) = Self::build_where(filter);
        let limit = filter.limit.unwrap_or(DEFAULT_LIMIT);
        let sql = format!(
            "SELECT id, timestamp, user_id, action, target, detail, outcome \
             FROM audit_log {where_clause} ORDER BY id DESC LIMIT {limit}"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bind_values.iter()), |row| {
            Ok(RawRow {
                id: row.get(0)?,
                timestamp: row.get::<_, String>(1)?,
                user_id: row.get(2)?,
                action: row.get::<_, String>(3)?,
                target: row.get(4)?,
                detail: row.get(5)?,
                outcome: row.get::<_, String>(6)?,
            })
        })?;

        let mut entries = Vec::new();
        for row in rows {
            let r = row?;
            entries.push(Self::raw_to_entry(r)?);
        }
        Ok(entries)
    }

    /// Count entries matching `filter`.
    pub fn count(&self, filter: &AuditFilter) -> Result<u64> {
        let (where_clause, bind_values) = Self::build_where(filter);
        let sql = format!("SELECT COUNT(*) FROM audit_log {where_clause}");
        let mut stmt = self.conn.prepare(&sql)?;
        let count: u64 = stmt.query_row(rusqlite::params_from_iter(bind_values.iter()), |row| {
            row.get(0)
        })?;
        Ok(count)
    }

    // -- helpers ------------------------------------------------------------

    /// Build a WHERE clause and positional bind values from a filter.
    fn build_where(filter: &AuditFilter) -> (String, Vec<String>) {
        let mut clauses: Vec<String> = Vec::new();
        let mut values: Vec<String> = Vec::new();
        let mut idx = 1usize;

        if let Some(ref uid) = filter.user_id {
            clauses.push(format!("user_id = ?{idx}"));
            values.push(uid.clone());
            idx += 1;
        }
        if let Some(action) = filter.action {
            clauses.push(format!("action = ?{idx}"));
            values.push(action.to_string());
            idx += 1;
        }
        if let Some(since) = filter.since {
            clauses.push(format!("timestamp >= ?{idx}"));
            values.push(since.to_rfc3339());
            idx += 1;
        }
        if let Some(until) = filter.until {
            clauses.push(format!("timestamp <= ?{idx}"));
            values.push(until.to_rfc3339());
            // idx += 1; // not needed but kept for clarity
        }

        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        (where_clause, values)
    }

    fn raw_to_entry(r: RawRow) -> Result<AuditEntry> {
        Ok(AuditEntry {
            id: r.id,
            timestamp: DateTime::parse_from_rfc3339(&r.timestamp)
                .context("parsing timestamp")?
                .with_timezone(&Utc),
            user_id: r.user_id,
            action: AuditAction::from_str(&r.action)?,
            target: r.target,
            detail: r.detail,
            outcome: AuditOutcome::from_str(&r.outcome)?,
        })
    }
}

/// Intermediate struct for reading rows before conversion.
struct RawRow {
    id: i64,
    timestamp: String,
    user_id: String,
    action: String,
    target: String,
    detail: Option<String>,
    outcome: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn make_entry(user: &str, action: AuditAction, target: &str) -> AuditEntry {
        AuditEntry {
            id: 0,
            timestamp: Utc::now(),
            user_id: user.to_string(),
            action,
            target: target.to_string(),
            detail: None,
            outcome: AuditOutcome::Success,
        }
    }

    #[test]
    fn log_and_retrieve() {
        let log = AuditLog::in_memory().unwrap();
        let entry = AuditEntry {
            id: 0,
            timestamp: Utc::now(),
            user_id: "alice".into(),
            action: AuditAction::ToolExecution,
            target: "web_search".into(),
            detail: Some(r#"{"query":"rust audit"}"#.into()),
            outcome: AuditOutcome::Success,
        };

        let id = log.log(&entry).unwrap();
        assert!(id > 0);

        let results = log.query(&AuditFilter::default()).unwrap();
        assert_eq!(results.len(), 1);

        let got = &results[0];
        assert_eq!(got.id, id);
        assert_eq!(got.user_id, "alice");
        assert_eq!(got.action, AuditAction::ToolExecution);
        assert_eq!(got.target, "web_search");
        assert_eq!(got.detail.as_deref(), Some(r#"{"query":"rust audit"}"#));
        assert_eq!(got.outcome, AuditOutcome::Success);
    }

    #[test]
    fn filter_by_user_id() {
        let log = AuditLog::in_memory().unwrap();
        log.log(&make_entry("alice", AuditAction::DataQuery, "dataset_a"))
            .unwrap();
        log.log(&make_entry("bob", AuditAction::DataQuery, "dataset_b"))
            .unwrap();
        log.log(&make_entry("alice", AuditAction::ConfigChange, "model_key"))
            .unwrap();

        let filter = AuditFilter {
            user_id: Some("alice".into()),
            ..Default::default()
        };
        let results = log.query(&filter).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|e| e.user_id == "alice"));
    }

    #[test]
    fn filter_by_time_range() {
        let log = AuditLog::in_memory().unwrap();

        let old = AuditEntry {
            id: 0,
            timestamp: Utc::now() - Duration::hours(2),
            user_id: "alice".into(),
            action: AuditAction::NodeStart,
            target: "node-1".into(),
            detail: None,
            outcome: AuditOutcome::Success,
        };
        let recent = AuditEntry {
            id: 0,
            timestamp: Utc::now(),
            user_id: "alice".into(),
            action: AuditAction::NodeStop,
            target: "node-1".into(),
            detail: None,
            outcome: AuditOutcome::Failure,
        };

        log.log(&old).unwrap();
        log.log(&recent).unwrap();

        let filter = AuditFilter {
            since: Some(Utc::now() - Duration::hours(1)),
            ..Default::default()
        };
        let results = log.query(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, AuditAction::NodeStop);
    }

    #[test]
    fn count_queries() {
        let log = AuditLog::in_memory().unwrap();
        for i in 0..5 {
            log.log(&make_entry(
                "alice",
                AuditAction::DataQuery,
                &format!("ds_{i}"),
            ))
            .unwrap();
        }
        log.log(&make_entry("bob", AuditAction::ToolExecution, "calc"))
            .unwrap();

        // Total count.
        assert_eq!(log.count(&AuditFilter::default()).unwrap(), 6);

        // Filtered count.
        let filter = AuditFilter {
            action: Some(AuditAction::DataQuery),
            ..Default::default()
        };
        assert_eq!(log.count(&filter).unwrap(), 5);

        let filter = AuditFilter {
            user_id: Some("bob".into()),
            ..Default::default()
        };
        assert_eq!(log.count(&filter).unwrap(), 1);
    }

    #[test]
    fn limit_respected() {
        let log = AuditLog::in_memory().unwrap();
        for i in 0..10 {
            log.log(&make_entry(
                "alice",
                AuditAction::DataIngest,
                &format!("file_{i}"),
            ))
            .unwrap();
        }

        let filter = AuditFilter {
            limit: Some(3),
            ..Default::default()
        };
        let results = log.query(&filter).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn all_outcomes_roundtrip() {
        let log = AuditLog::in_memory().unwrap();
        for outcome in [
            AuditOutcome::Success,
            AuditOutcome::Failure,
            AuditOutcome::Denied,
        ] {
            let mut entry = make_entry("alice", AuditAction::UserLogin, "session");
            entry.outcome = outcome;
            log.log(&entry).unwrap();
        }
        let results = log.query(&AuditFilter::default()).unwrap();
        assert_eq!(results.len(), 3);
        let outcomes: Vec<_> = results.iter().map(|e| e.outcome).collect();
        assert!(outcomes.contains(&AuditOutcome::Success));
        assert!(outcomes.contains(&AuditOutcome::Failure));
        assert!(outcomes.contains(&AuditOutcome::Denied));
    }

    #[test]
    fn all_actions_roundtrip() {
        let log = AuditLog::in_memory().unwrap();
        let actions = [
            AuditAction::ToolExecution,
            AuditAction::DataQuery,
            AuditAction::DataIngest,
            AuditAction::DataPublish,
            AuditAction::ConfigChange,
            AuditAction::UserLogin,
            AuditAction::UserLogout,
            AuditAction::RoleChange,
            AuditAction::NodeStart,
            AuditAction::NodeStop,
            AuditAction::Subscription,
        ];
        for action in actions {
            log.log(&make_entry("test", action, "target")).unwrap();
        }
        let results = log.query(&AuditFilter::default()).unwrap();
        assert_eq!(results.len(), actions.len());
    }

    // -- Edge cases and error paths -------------------------------------------

    #[test]
    fn unicode_in_fields() {
        let log = AuditLog::in_memory().unwrap();
        let mut entry = make_entry("用户🧪", AuditAction::DataIngest, "数据集/résumé.csv");
        entry.detail = Some("détails: «données brutes»".into());
        let id = log.log(&entry).unwrap();

        let results = log
            .query(&AuditFilter {
                user_id: Some("用户🧪".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, id);
        assert_eq!(results[0].target, "数据集/résumé.csv");
    }

    #[test]
    fn empty_detail_vs_none() {
        let log = AuditLog::in_memory().unwrap();

        let mut entry = make_entry("alice", AuditAction::ConfigChange, "key");
        entry.detail = None;
        log.log(&entry).unwrap();

        entry.detail = Some(String::new());
        log.log(&entry).unwrap();

        let results = log.query(&AuditFilter::default()).unwrap();
        assert_eq!(results.len(), 2);
        // Query returns newest first — empty string was inserted second.
        assert_eq!(results[0].detail.as_deref(), Some(""));
        assert!(results[1].detail.is_none());
    }

    #[test]
    fn combined_filters() {
        let log = AuditLog::in_memory().unwrap();
        log.log(&make_entry("alice", AuditAction::DataQuery, "ds_a"))
            .unwrap();
        log.log(&make_entry(
            "alice",
            AuditAction::ToolExecution,
            "web_search",
        ))
        .unwrap();
        log.log(&make_entry("bob", AuditAction::DataQuery, "ds_b"))
            .unwrap();

        let filter = AuditFilter {
            user_id: Some("alice".into()),
            action: Some(AuditAction::DataQuery),
            ..Default::default()
        };
        let results = log.query(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].target, "ds_a");
    }

    #[test]
    fn audit_action_display_matches_from_str() {
        let actions = [
            AuditAction::ToolExecution,
            AuditAction::DataQuery,
            AuditAction::DataIngest,
            AuditAction::DataPublish,
            AuditAction::ConfigChange,
            AuditAction::UserLogin,
            AuditAction::UserLogout,
            AuditAction::RoleChange,
            AuditAction::NodeStart,
            AuditAction::NodeStop,
            AuditAction::Subscription,
        ];
        for action in actions {
            let s = action.to_string();
            let parsed = AuditAction::from_str(&s).unwrap();
            assert_eq!(parsed, action);
        }
    }

    #[test]
    fn audit_action_from_str_invalid() {
        assert!(AuditAction::from_str("NotAnAction").is_err());
        assert!(AuditAction::from_str("").is_err());
    }

    #[test]
    fn audit_outcome_from_str_invalid() {
        assert!(AuditOutcome::from_str("Maybe").is_err());
    }

    #[test]
    fn large_detail_field() {
        let log = AuditLog::in_memory().unwrap();
        let big_detail = "x".repeat(100_000);
        let mut entry = make_entry("alice", AuditAction::ToolExecution, "heavy");
        entry.detail = Some(big_detail.clone());
        log.log(&entry).unwrap();

        let results = log.query(&AuditFilter::default()).unwrap();
        assert_eq!(results[0].detail.as_deref(), Some(big_detail.as_str()));
    }

    #[test]
    fn audit_entry_serde_roundtrip() {
        let entry = AuditEntry {
            id: 42,
            timestamp: Utc::now(),
            user_id: "alice".into(),
            action: AuditAction::DataPublish,
            target: "dataset-1".into(),
            detail: Some("published to mesh".into()),
            outcome: AuditOutcome::Success,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: AuditEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.user_id, "alice");
        assert_eq!(parsed.action, AuditAction::DataPublish);
        assert_eq!(parsed.outcome, AuditOutcome::Success);
    }

    #[test]
    fn count_empty_log() {
        let log = AuditLog::in_memory().unwrap();
        assert_eq!(log.count(&AuditFilter::default()).unwrap(), 0);
    }

    #[test]
    fn query_empty_log() {
        let log = AuditLog::in_memory().unwrap();
        let results = log.query(&AuditFilter::default()).unwrap();
        assert!(results.is_empty());
    }
}
