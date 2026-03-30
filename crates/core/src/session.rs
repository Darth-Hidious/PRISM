//! SQLite-backed session management for PRISM nodes.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

/// A single authenticated session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub user_id: String,
    pub display_name: Option<String>,
    pub platform_role: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
}

/// Manages session lifecycle backed by SQLite.
pub struct SessionManager {
    conn: Connection,
    default_timeout: Duration,
}

impl SessionManager {
    /// Open (or create) a session store at `db_path`.
    /// Pass `":memory:"` for an in-memory database.
    pub fn new<P: AsRef<Path>>(db_path: P, default_timeout: Duration) -> Result<Self> {
        let conn = Connection::open(db_path).context("failed to open session database")?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id           TEXT PRIMARY KEY,
                user_id      TEXT NOT NULL,
                display_name TEXT,
                platform_role TEXT,
                created_at   TEXT NOT NULL,
                expires_at   TEXT NOT NULL,
                last_active  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_user_id ON sessions(user_id);
            CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);",
        )?;
        Ok(Self {
            conn,
            default_timeout,
        })
    }

    /// Create a new session for the given user.
    pub fn create_session(
        &self,
        user_id: &str,
        display_name: Option<&str>,
        platform_role: Option<&str>,
    ) -> Result<Session> {
        let now = Utc::now();
        let session = Session {
            id: Uuid::new_v4().to_string(),
            user_id: user_id.to_owned(),
            display_name: display_name.map(str::to_owned),
            platform_role: platform_role.map(str::to_owned),
            created_at: now,
            expires_at: now + self.default_timeout,
            last_active: now,
        };
        self.conn.execute(
            "INSERT INTO sessions (id, user_id, display_name, platform_role, created_at, expires_at, last_active)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session.id,
                session.user_id,
                session.display_name,
                session.platform_role,
                session.created_at.to_rfc3339(),
                session.expires_at.to_rfc3339(),
                session.last_active.to_rfc3339(),
            ],
        )?;
        Ok(session)
    }

    /// Validate a session by ID.
    ///
    /// Returns `None` when the session does not exist or has expired.
    /// Touches `last_active` on every successful validation.
    pub fn validate_session(&self, session_id: &str) -> Result<Option<Session>> {
        let now = Utc::now();
        let mut stmt = self.conn.prepare(
            "SELECT id, user_id, display_name, platform_role, created_at, expires_at, last_active
             FROM sessions WHERE id = ?1",
        )?;

        let session = stmt
            .query_row(params![session_id], |row| {
                Ok(Session {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    display_name: row.get(2)?,
                    platform_role: row.get(3)?,
                    created_at: parse_dt(&row.get::<_, String>(4)?),
                    expires_at: parse_dt(&row.get::<_, String>(5)?),
                    last_active: parse_dt(&row.get::<_, String>(6)?),
                })
            })
            .optional()?;

        match session {
            Some(s) if s.expires_at > now => {
                self.conn.execute(
                    "UPDATE sessions SET last_active = ?1 WHERE id = ?2",
                    params![now.to_rfc3339(), session_id],
                )?;
                Ok(Some(Session {
                    last_active: now,
                    ..s
                }))
            }
            Some(_) => {
                // Expired — treat as invalid.
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// Destroy a single session.
    pub fn destroy_session(&self, session_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
        Ok(())
    }

    /// Destroy every session belonging to `user_id` (logout everywhere).
    pub fn destroy_user_sessions(&self, user_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM sessions WHERE user_id = ?1",
            params![user_id],
        )?;
        Ok(())
    }

    /// Remove all expired sessions. Returns the number of rows deleted.
    pub fn cleanup_expired(&self) -> Result<u64> {
        let now = Utc::now().to_rfc3339();
        let count = self
            .conn
            .execute("DELETE FROM sessions WHERE expires_at <= ?1", params![now])?;
        Ok(count as u64)
    }

    /// List every non-expired session.
    pub fn active_sessions(&self) -> Result<Vec<Session>> {
        let now = Utc::now().to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT id, user_id, display_name, platform_role, created_at, expires_at, last_active
             FROM sessions WHERE expires_at > ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![now], |row| {
            Ok(Session {
                id: row.get(0)?,
                user_id: row.get(1)?,
                display_name: row.get(2)?,
                platform_role: row.get(3)?,
                created_at: parse_dt(&row.get::<_, String>(4)?),
                expires_at: parse_dt(&row.get::<_, String>(5)?),
                last_active: parse_dt(&row.get::<_, String>(6)?),
            })
        })?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Parse an RFC-3339 timestamp, falling back to epoch on malformed input.
fn parse_dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_default()
}

/// Convenience trait so `query_row` can return `Option`.
trait OptionalRow<T> {
    fn optional(self) -> Result<Option<T>>;
}

impl<T> OptionalRow<T> for std::result::Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(timeout: Duration) -> SessionManager {
        SessionManager::new(":memory:", timeout).expect("in-memory db")
    }

    #[test]
    fn create_and_validate() {
        let mgr = manager(Duration::hours(1));
        let session = mgr
            .create_session("user-1", Some("Alice"), Some("admin"))
            .unwrap();

        assert_eq!(session.user_id, "user-1");
        assert_eq!(session.display_name.as_deref(), Some("Alice"));
        assert_eq!(session.platform_role.as_deref(), Some("admin"));

        let found = mgr.validate_session(&session.id).unwrap();
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.id, session.id);
        // last_active should have been bumped
        assert!(found.last_active >= session.last_active);
    }

    #[test]
    fn expired_session_returns_none() {
        // Timeout of zero seconds → immediately expired.
        let mgr = manager(Duration::zero());
        let session = mgr.create_session("user-2", None, None).unwrap();

        // Give a tiny margin; the session was created with expires_at == created_at,
        // so validation should see it as expired.
        std::thread::sleep(std::time::Duration::from_millis(10));

        let found = mgr.validate_session(&session.id).unwrap();
        assert!(found.is_none(), "expired session must return None");
    }

    #[test]
    fn destroy_session() {
        let mgr = manager(Duration::hours(1));
        let session = mgr.create_session("user-3", None, None).unwrap();

        mgr.destroy_session(&session.id).unwrap();

        let found = mgr.validate_session(&session.id).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn destroy_user_sessions() {
        let mgr = manager(Duration::hours(1));
        let _s1 = mgr.create_session("user-4", None, None).unwrap();
        let _s2 = mgr.create_session("user-4", None, None).unwrap();
        let other = mgr.create_session("user-5", None, None).unwrap();

        mgr.destroy_user_sessions("user-4").unwrap();

        let active = mgr.active_sessions().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, other.id);
    }

    #[test]
    fn cleanup_expired() {
        let mgr = manager(Duration::zero());
        mgr.create_session("a", None, None).unwrap();
        mgr.create_session("b", None, None).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        let cleaned = mgr.cleanup_expired().unwrap();
        assert_eq!(cleaned, 2);

        let active = mgr.active_sessions().unwrap();
        assert!(active.is_empty());
    }

    #[test]
    fn active_sessions_excludes_expired() {
        let mgr = manager(Duration::hours(1));
        let _live = mgr.create_session("live", None, None).unwrap();

        // Manually insert an already-expired session.
        let past = (Utc::now() - Duration::hours(2)).to_rfc3339();
        mgr.conn
            .execute(
                "INSERT INTO sessions (id,user_id,display_name,platform_role,created_at,expires_at,last_active)
                 VALUES ('dead','expired-user',NULL,NULL,?1,?1,?1)",
                params![past],
            )
            .unwrap();

        let active = mgr.active_sessions().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].user_id, "live");
    }

    // -- Edge cases and error paths -------------------------------------------

    #[test]
    fn validate_nonexistent_session_returns_none() {
        let mgr = manager(Duration::hours(1));
        assert!(mgr.validate_session("does-not-exist").unwrap().is_none());
    }

    #[test]
    fn destroy_nonexistent_session_is_ok() {
        let mgr = manager(Duration::hours(1));
        // Should not error — just a no-op DELETE.
        mgr.destroy_session("ghost-session").unwrap();
    }

    #[test]
    fn destroy_user_sessions_nonexistent_user() {
        let mgr = manager(Duration::hours(1));
        mgr.destroy_user_sessions("nobody").unwrap();
    }

    #[test]
    fn cleanup_expired_empty_db() {
        let mgr = manager(Duration::hours(1));
        let cleaned = mgr.cleanup_expired().unwrap();
        assert_eq!(cleaned, 0);
    }

    #[test]
    fn active_sessions_empty_db() {
        let mgr = manager(Duration::hours(1));
        let active = mgr.active_sessions().unwrap();
        assert!(active.is_empty());
    }

    #[test]
    fn create_session_none_optional_fields() {
        let mgr = manager(Duration::hours(1));
        let s = mgr.create_session("user-x", None, None).unwrap();
        assert!(s.display_name.is_none());
        assert!(s.platform_role.is_none());

        let validated = mgr.validate_session(&s.id).unwrap().unwrap();
        assert!(validated.display_name.is_none());
        assert!(validated.platform_role.is_none());
    }

    #[test]
    fn multiple_sessions_same_user() {
        let mgr = manager(Duration::hours(1));
        let s1 = mgr.create_session("user-multi", Some("A"), None).unwrap();
        let s2 = mgr.create_session("user-multi", Some("B"), None).unwrap();
        let s3 = mgr.create_session("user-multi", Some("C"), None).unwrap();

        // All three should be individually valid.
        assert!(mgr.validate_session(&s1.id).unwrap().is_some());
        assert!(mgr.validate_session(&s2.id).unwrap().is_some());
        assert!(mgr.validate_session(&s3.id).unwrap().is_some());

        let active = mgr.active_sessions().unwrap();
        assert_eq!(active.len(), 3);

        // Destroy all for that user.
        mgr.destroy_user_sessions("user-multi").unwrap();
        assert!(mgr.active_sessions().unwrap().is_empty());
    }

    #[test]
    fn session_id_is_valid_uuid() {
        let mgr = manager(Duration::hours(1));
        let s = mgr.create_session("user-uuid", None, None).unwrap();
        assert!(uuid::Uuid::parse_str(&s.id).is_ok());
    }

    #[test]
    fn unicode_user_fields() {
        let mgr = manager(Duration::hours(1));
        let s = mgr
            .create_session("用户🧪", Some("Ünïcödé Nàme"), Some("管理员"))
            .unwrap();
        let validated = mgr.validate_session(&s.id).unwrap().unwrap();
        assert_eq!(validated.user_id, "用户🧪");
        assert_eq!(validated.display_name.as_deref(), Some("Ünïcödé Nàme"));
        assert_eq!(validated.platform_role.as_deref(), Some("管理员"));
    }

    #[test]
    fn session_timestamps_are_sane() {
        let before = Utc::now();
        let mgr = manager(Duration::hours(2));
        let s = mgr.create_session("time-test", None, None).unwrap();
        let after = Utc::now();

        assert!(s.created_at >= before && s.created_at <= after);
        assert!(s.last_active >= before && s.last_active <= after);
        // Expiry should be ~2 hours in the future.
        let expected_expiry = before + Duration::hours(2);
        assert!(s.expires_at >= expected_expiry - Duration::seconds(1));
    }

    #[test]
    fn session_serde_roundtrip() {
        let mgr = manager(Duration::hours(1));
        let s = mgr
            .create_session("serde-user", Some("Test"), Some("admin"))
            .unwrap();
        let json = serde_json::to_string(&s).unwrap();
        let parsed: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, s.id);
        assert_eq!(parsed.user_id, s.user_id);
        assert_eq!(parsed.display_name, s.display_name);
        assert_eq!(parsed.platform_role, s.platform_role);
    }

    #[test]
    fn parse_dt_malformed_returns_epoch() {
        let dt = parse_dt("not-a-date");
        assert_eq!(dt, DateTime::<Utc>::default());
    }

    #[test]
    fn parse_dt_valid_roundtrips() {
        let now = Utc::now();
        let s = now.to_rfc3339();
        let parsed = parse_dt(&s);
        // Within 1 second tolerance (rfc3339 may truncate sub-second).
        assert!((parsed - now).num_seconds().abs() <= 1);
    }

    #[test]
    fn file_backed_session_persists() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap();
        let session_id;
        {
            let mgr = SessionManager::new(path, Duration::hours(1)).unwrap();
            let s = mgr.create_session("persist-user", Some("Bob"), None).unwrap();
            session_id = s.id;
        }
        // Re-open from disk.
        let mgr = SessionManager::new(path, Duration::hours(1)).unwrap();
        let found = mgr.validate_session(&session_id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().user_id, "persist-user");
    }

    #[test]
    fn many_sessions_scale() {
        let mgr = manager(Duration::hours(1));
        for i in 0..200 {
            mgr.create_session(&format!("user-{i}"), None, None).unwrap();
        }
        let active = mgr.active_sessions().unwrap();
        assert_eq!(active.len(), 200);
    }
}
