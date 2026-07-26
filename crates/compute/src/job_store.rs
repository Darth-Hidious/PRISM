//! Durable compute-job records on PRISM's local Turso spine.
//!
//! [`JobTracker`](crate::JobTracker) is an in-process `HashMap`: correct while
//! the submitting process lives, empty the moment it exits. That made
//! `prism run --ssh user@host` fire-and-forget — the job really ran on the
//! remote box, but nothing remembered *which* box, so `prism job-status <id>`
//! had no way to ask.
//!
//! This module adds the missing half: one table, `compute_jobs`, in the SAME
//! Turso database PRISM already uses for provenance (`~/.prism/provenance.db`,
//! opened by `prism_provenance::ProvenanceStore`). No second store, no new
//! service, no Postgres. `CREATE TABLE IF NOT EXISTS` is idempotent and
//! coexists with the provenance/EMMO schema in the same file.
//!
//! What is persisted is deliberately the minimum needed to *re-reach* a job:
//! its backend kind and the target descriptor (SSH host/user/port, K8s
//! context/namespace, SLURM head node, or the platform API base). Credentials
//! are NEVER written — the SSH *key path* is stored, the key is not; the
//! marc27 bearer/API token is not stored at all and is re-resolved from the
//! caller's environment at query time.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use turso::Value;
use uuid::Uuid;

use crate::job::{JobRecord, TrackedStatus};

/// Default on-disk location of the local spine: `~/.prism/provenance.db`.
///
/// Same file the ingest pipeline and workflows engine resolve to, so job
/// records live beside the provenance they belong to.
pub fn default_db_path() -> Option<PathBuf> {
    dirs_home().map(|h| h.join(".prism/provenance.db"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Durable store for [`JobRecord`]s.
pub struct JobStore {
    conn: turso::Connection,
}

impl JobStore {
    /// Open (creating if absent) the job table in the Turso database at `path`.
    ///
    /// The parent directory is created on demand so a first-ever `prism run`
    /// on a clean machine does not fail with ENOENT.
    pub async fn open(path: &Path) -> Result<Self> {
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("job database path is not valid UTF-8: {path:?}"))?;

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let db = turso::Builder::new_local(path_str)
            .build()
            .await
            .context("failed to open Turso database for compute jobs")?;
        let conn = db.connect()?;
        Self::init_schema(&conn).await?;
        Ok(Self { conn })
    }

    /// Open the default local spine (`~/.prism/provenance.db`).
    pub async fn open_default() -> Result<Self> {
        let path = default_db_path()
            .ok_or_else(|| anyhow::anyhow!("cannot resolve HOME to locate ~/.prism"))?;
        Self::open(&path).await
    }

    async fn init_schema(conn: &turso::Connection) -> Result<()> {
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS compute_jobs (
                job_id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                image TEXT NOT NULL,
                backend TEXT NOT NULL,
                target_json TEXT NOT NULL,
                status_json TEXT NOT NULL,
                output_json TEXT,
                submitted_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )"#,
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_compute_jobs_submitted ON compute_jobs(submitted_at)",
            (),
        )
        .await?;
        Ok(())
    }

    /// Insert or update a job record (keyed on `job_id`).
    pub async fn save(&self, rec: &JobRecord) -> Result<()> {
        self.conn
            .execute(
                r#"INSERT INTO compute_jobs
                   (job_id, name, image, backend, target_json, status_json,
                    output_json, submitted_at, updated_at)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                   ON CONFLICT(job_id) DO UPDATE SET
                     status_json = excluded.status_json,
                     output_json = COALESCE(excluded.output_json, compute_jobs.output_json),
                     updated_at  = excluded.updated_at"#,
                (
                    Value::Text(rec.job_id.to_string()),
                    Value::Text(rec.name.clone()),
                    Value::Text(rec.image.clone()),
                    Value::Text(rec.backend.clone()),
                    Value::Text(serde_json::to_string(&rec.target)?),
                    Value::Text(serde_json::to_string(&rec.status)?),
                    match &rec.output {
                        Some(o) => Value::Text(o.clone()),
                        None => Value::Null,
                    },
                    Value::Text(rec.submitted_at.to_rfc3339()),
                    Value::Text(rec.updated_at.to_rfc3339()),
                ),
            )
            .await
            .context("failed to persist compute job record")?;
        Ok(())
    }

    /// Load one job record by id. `Ok(None)` means "we have no record of this
    /// job" — an honest answer, distinct from an error talking to the store.
    pub async fn load(&self, job_id: Uuid) -> Result<Option<JobRecord>> {
        let mut rows = self
            .conn
            .query(
                "SELECT job_id, name, image, backend, target_json, status_json, \
                 output_json, submitted_at, updated_at \
                 FROM compute_jobs WHERE job_id = ?1",
                (Value::Text(job_id.to_string()),),
            )
            .await?;
        match rows.next().await? {
            Some(row) => Ok(Some(row_to_record(&row)?)),
            None => Ok(None),
        }
    }

    /// List the most recently submitted jobs, newest first.
    pub async fn list(&self, limit: usize) -> Result<Vec<JobRecord>> {
        let limit = limit.clamp(1, 1000) as i64;
        let mut rows = self
            .conn
            .query(
                "SELECT job_id, name, image, backend, target_json, status_json, \
                 output_json, submitted_at, updated_at \
                 FROM compute_jobs ORDER BY submitted_at DESC LIMIT ?1",
                (Value::Integer(limit),),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(row_to_record(&row)?);
        }
        Ok(out)
    }
}

fn get_text(row: &turso::Row, idx: usize) -> Result<String> {
    match row.get_value(idx)? {
        Value::Text(s) => Ok(s),
        other => anyhow::bail!("expected TEXT at column {idx}, got {other:?}"),
    }
}

fn get_opt_text(row: &turso::Row, idx: usize) -> Result<Option<String>> {
    match row.get_value(idx)? {
        Value::Text(s) => Ok(Some(s)),
        Value::Null => Ok(None),
        other => anyhow::bail!("expected TEXT or NULL at column {idx}, got {other:?}"),
    }
}

fn row_to_record(row: &turso::Row) -> Result<JobRecord> {
    let job_id: Uuid = get_text(row, 0)?
        .parse()
        .context("stored job_id is not a UUID")?;
    let target: serde_json::Value =
        serde_json::from_str(&get_text(row, 4)?).context("stored target_json is not valid JSON")?;
    let status: TrackedStatus = serde_json::from_str(&get_text(row, 5)?)
        .context("stored status_json is not a valid TrackedStatus")?;
    let submitted_at = chrono::DateTime::parse_from_rfc3339(&get_text(row, 7)?)
        .context("stored submitted_at is not RFC3339")?
        .with_timezone(&chrono::Utc);
    let updated_at = chrono::DateTime::parse_from_rfc3339(&get_text(row, 8)?)
        .context("stored updated_at is not RFC3339")?
        .with_timezone(&chrono::Utc);

    Ok(JobRecord {
        job_id,
        name: get_text(row, 1)?,
        image: get_text(row, 2)?,
        backend: get_text(row, 3)?,
        target,
        status,
        output: get_opt_text(row, 6)?,
        submitted_at,
        updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db() -> PathBuf {
        std::env::temp_dir().join(format!("prism-jobstore-{}.db", Uuid::new_v4().as_simple()))
    }

    fn sample(job_id: Uuid, backend: &str) -> JobRecord {
        let now = chrono::Utc::now();
        JobRecord {
            job_id,
            name: "echo-test".into(),
            image: "alpine:3.20".into(),
            backend: backend.into(),
            target: serde_json::json!({"kind": "ssh", "host": "box.lab", "port": 22}),
            status: TrackedStatus::Queued,
            output: None,
            submitted_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn record_survives_store_reopen() {
        // THE bug this module exists to fix: a BYOC job must still be
        // answerable after the submitting process is gone. Closing and
        // reopening the store simulates exactly that.
        let path = tmp_db();
        let id = Uuid::new_v4();

        {
            let store = JobStore::open(&path).await.unwrap();
            store.save(&sample(id, "byoc")).await.unwrap();
        } // store dropped — process "exited"

        let store = JobStore::open(&path).await.unwrap();
        let loaded = store.load(id).await.unwrap().expect("record must survive");
        assert_eq!(loaded.job_id, id);
        assert_eq!(loaded.backend, "byoc");
        assert_eq!(loaded.target["host"], serde_json::json!("box.lab"));
        assert!(matches!(loaded.status, TrackedStatus::Queued));
    }

    #[tokio::test]
    async fn unknown_job_is_none_not_error() {
        let path = tmp_db();
        let store = JobStore::open(&path).await.unwrap();
        assert!(store.load(Uuid::new_v4()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn save_upserts_status_and_output() {
        let path = tmp_db();
        let id = Uuid::new_v4();
        let store = JobStore::open(&path).await.unwrap();

        store.save(&sample(id, "byoc")).await.unwrap();

        let mut rec = sample(id, "byoc");
        rec.status = TrackedStatus::Completed { duration_secs: 3 };
        rec.output = Some(r#"{"stdout":"hello\n"}"#.into());
        rec.updated_at = chrono::Utc::now();
        store.save(&rec).await.unwrap();

        let loaded = store.load(id).await.unwrap().unwrap();
        assert!(matches!(
            loaded.status,
            TrackedStatus::Completed { duration_secs: 3 }
        ));
        assert_eq!(loaded.output.as_deref(), Some(r#"{"stdout":"hello\n"}"#));

        // Exactly one row — an upsert, not a duplicate insert.
        assert_eq!(store.list(100).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn output_is_not_clobbered_by_a_later_status_only_save() {
        // A status refresh that carries no output must not erase the output we
        // already captured.
        let path = tmp_db();
        let id = Uuid::new_v4();
        let store = JobStore::open(&path).await.unwrap();

        let mut rec = sample(id, "byoc");
        rec.output = Some("captured".into());
        store.save(&rec).await.unwrap();

        let mut later = sample(id, "byoc");
        later.output = None;
        later.status = TrackedStatus::Cancelled;
        store.save(&later).await.unwrap();

        let loaded = store.load(id).await.unwrap().unwrap();
        assert_eq!(loaded.output.as_deref(), Some("captured"));
        assert!(matches!(loaded.status, TrackedStatus::Cancelled));
    }

    #[tokio::test]
    async fn list_returns_newest_first() {
        let path = tmp_db();
        let store = JobStore::open(&path).await.unwrap();

        let older = Uuid::new_v4();
        let newer = Uuid::new_v4();
        let mut a = sample(older, "local");
        a.submitted_at = chrono::Utc::now() - chrono::Duration::seconds(60);
        let mut b = sample(newer, "byoc");
        b.submitted_at = chrono::Utc::now();
        store.save(&a).await.unwrap();
        store.save(&b).await.unwrap();

        let all = store.list(10).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].job_id, newer, "newest must sort first");
    }

    #[tokio::test]
    async fn schema_init_is_idempotent_across_opens() {
        let path = tmp_db();
        let id = Uuid::new_v4();
        JobStore::open(&path).await.unwrap();
        let store = JobStore::open(&path).await.unwrap();
        store.save(&sample(id, "local")).await.unwrap();
        assert!(store.load(id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn coexists_with_the_provenance_schema_in_one_file() {
        // The whole point of reusing the spine: the job table must live in the
        // SAME database file as provenance without either clobbering the other.
        let path = tmp_db();
        let id = Uuid::new_v4();

        let prov = prism_provenance::ProvenanceStore::open(&path)
            .await
            .unwrap();
        let jobs = JobStore::open(&path).await.unwrap();
        jobs.save(&sample(id, "byoc")).await.unwrap();

        // Provenance still works after the job table was created alongside it.
        let stats = prov.stats().await.unwrap();
        assert_eq!(stats.total_records, 0);
        assert!(jobs.load(id).await.unwrap().is_some());
    }
}
