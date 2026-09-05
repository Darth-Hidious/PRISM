"""SQLite-backed job state machine.

One row per job. Status transitions are validated in code (not via SQL
constraints). All writes are serialised through a single connection with
DELETE journaling and a check_same_thread=False so the async runner can write
concurrently with reader tools without WAL sidecars on shared filesystems.
"""

from __future__ import annotations

import json
import os
import sqlite3
import threading
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable

from ..schemas import JobRecord, JobStatus

_SCHEMA = """
CREATE TABLE IF NOT EXISTS jobs (
    job_id        TEXT PRIMARY KEY,
    tool_name     TEXT NOT NULL,
    status        TEXT NOT NULL,
    backend       TEXT,
    input_json    TEXT NOT NULL,
    cache_key     TEXT,
    result_json   TEXT,
    error_json    TEXT,
    progress_pct  REAL DEFAULT 0,
    progress_msg  TEXT DEFAULT '',
    progress_step INTEGER DEFAULT 0,
    progress_total INTEGER DEFAULT 0,
    hf_job_id     TEXT,
    hf_job_url    TEXT,
    provenance_ref TEXT,
    started_at    TEXT NOT NULL,
    finished_at   TEXT,
    owner_pid     INTEGER
);
CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status);
CREATE INDEX IF NOT EXISTS idx_jobs_started ON jobs(started_at);
"""


VALID_TRANSITIONS: dict[JobStatus, set[JobStatus]] = {
    "queued": {"submitted", "cancelling", "cancelled", "failed"},
    "submitted": {"running", "cancelling", "cancelled", "failed"},
    "running": {"succeeded", "failed", "cancelling", "cancelled", "interrupted"},
    "cancelling": {"cancelled", "succeeded", "failed", "interrupted"},  # late completions allowed
    "succeeded": set(),
    "failed": set(),
    "cancelled": set(),
    "interrupted": set(),
}
# Non-terminal states may also be orphaned before they reach "running".
for _s in ("queued", "submitted"):
    VALID_TRANSITIONS[_s].add("interrupted")

TERMINAL: set[str] = {"succeeded", "failed", "cancelled", "interrupted"}


def pid_alive(pid: int) -> bool:
    """Whether a process with this pid exists (signal 0; EPERM counts as alive)."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


class JobStoreError(RuntimeError):
    pass


def now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="microseconds")


class JobStore:
    def __init__(self, db_path: Path) -> None:
        self.db_path = Path(db_path)
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        self._lock = threading.RLock()
        self._conn = sqlite3.connect(
            str(self.db_path), check_same_thread=False, isolation_level=None
        )
        self._conn.row_factory = sqlite3.Row
        with self._lock:
            # Avoid WAL/-shm sidecars: job state may live on Lustre/GPFS.
            self._conn.execute("PRAGMA journal_mode=DELETE")
            self._conn.executescript(_SCHEMA)
            # Stores created before the owner column existed.
            cols = {r["name"] for r in self._conn.execute("PRAGMA table_info(jobs)")}
            if "owner_pid" not in cols:
                self._conn.execute("ALTER TABLE jobs ADD COLUMN owner_pid INTEGER")

    # ------------------------------------------------------------------
    def create(
        self,
        job_id: str,
        tool_name: str,
        input_payload: dict[str, Any],
        cache_key: str | None = None,
        backend: str | None = None,
    ) -> None:
        with self._lock:
            self._conn.execute(
                """INSERT INTO jobs (job_id, tool_name, status, backend,
                                     input_json, cache_key, started_at)
                   VALUES (?, ?, ?, ?, ?, ?, ?)""",
                (
                    job_id,
                    tool_name,
                    "queued",
                    backend,
                    json.dumps(input_payload, default=str),
                    cache_key,
                    now_iso(),
                ),
            )

    def get(self, job_id: str) -> JobRecord | None:
        with self._lock:
            row = self._conn.execute(
                "SELECT * FROM jobs WHERE job_id = ?", (job_id,)
            ).fetchone()
        if row is None:
            return None
        return _row_to_record(row)

    def list(
        self,
        limit: int = 50,
        status_filter: JobStatus | None = None,
        since_iso: str | None = None,
    ) -> list[JobRecord]:
        q = "SELECT * FROM jobs WHERE 1=1"
        args: list[Any] = []
        if status_filter is not None:
            q += " AND status = ?"
            args.append(status_filter)
        if since_iso is not None:
            q += " AND started_at >= ?"
            args.append(since_iso)
        q += " ORDER BY started_at DESC LIMIT ?"
        args.append(limit)
        with self._lock:
            rows = self._conn.execute(q, args).fetchall()
        return [_row_to_record(r) for r in rows]

    def transition(
        self, job_id: str, new_status: JobStatus, owner_pid: int | None | object = ...
    ) -> None:
        """Move a job to `new_status`. Taking a job to "running" records the
        owner process (this one unless `owner_pid` is given explicitly), so a
        later process can tell a live job from an orphan."""
        with self._lock:
            row = self._conn.execute(
                "SELECT status FROM jobs WHERE job_id = ?", (job_id,)
            ).fetchone()
            if row is None:
                raise JobStoreError(f"job_id {job_id!r} unknown")
            current: JobStatus = row["status"]
            if current == new_status:
                return
            if new_status not in VALID_TRANSITIONS.get(current, set()):
                # Terminal-state idempotency: do nothing rather than raise
                if current in {"succeeded", "failed", "cancelled"}:
                    return
                raise JobStoreError(
                    f"invalid transition {current!r} -> {new_status!r}"
                )
            finished = now_iso() if new_status in TERMINAL else None
            if finished is not None:
                self._conn.execute(
                    "UPDATE jobs SET status = ?, finished_at = ? WHERE job_id = ?",
                    (new_status, finished, job_id),
                )
            elif new_status == "running":
                owner = os.getpid() if owner_pid is ... else owner_pid
                self._conn.execute(
                    "UPDATE jobs SET status = ?, owner_pid = ? WHERE job_id = ?",
                    (new_status, owner, job_id),
                )
            else:
                self._conn.execute(
                    "UPDATE jobs SET status = ? WHERE job_id = ?",
                    (new_status, job_id),
                )

    def reap_orphans(self, alive=pid_alive) -> list[str]:
        """Mark every non-terminal job whose owner process is gone (or was
        never recorded) as "interrupted", with what progress it had reached and
        the instruction to resubmit. Returns the job ids reaped.

        A fresh runner has no live tasks, so any job it finds "running" belongs
        to another process; it is left alone only while that process exists.
        Measured 2026-09-05: three MD jobs stayed "running" at 1150/2000 steps
        for the rest of the evening after their tool server died, and every
        poll repeated it."""
        reaped: list[str] = []
        with self._lock:
            rows = self._conn.execute(
                "SELECT job_id, status, owner_pid, progress_step, progress_total FROM jobs "
                "WHERE status IN ('queued', 'submitted', 'running', 'cancelling')"
            ).fetchall()
            for row in rows:
                owner = row["owner_pid"]
                if owner is not None and alive(int(owner)):
                    continue
                where = (
                    f"pid {owner}" if owner is not None else "an earlier process (no owner recorded)"
                )
                error = {
                    "code": "orphaned",
                    "error": (
                        f"interrupted: {where} was running this job and exited at step "
                        f"{row['progress_step']}/{row['progress_total']}; the job did not finish — resubmit it"
                    ),
                }
                self._conn.execute(
                    "UPDATE jobs SET status = 'interrupted', error_json = ?, finished_at = ? WHERE job_id = ?",
                    (json.dumps(error), now_iso(), row["job_id"]),
                )
                reaped.append(row["job_id"])
        return reaped

    def update_progress(
        self,
        job_id: str,
        percent: float,
        message: str = "",
        step: int = 0,
        total: int = 0,
    ) -> None:
        with self._lock:
            self._conn.execute(
                """UPDATE jobs
                   SET progress_pct = ?, progress_msg = ?,
                       progress_step = ?, progress_total = ?
                   WHERE job_id = ?""",
                (float(percent), str(message)[:512], int(step), int(total), job_id),
            )

    def set_result(self, job_id: str, result: dict[str, Any]) -> None:
        with self._lock:
            self._conn.execute(
                "UPDATE jobs SET result_json = ? WHERE job_id = ?",
                (json.dumps(result, default=str), job_id),
            )

    def set_error(self, job_id: str, error: dict[str, Any]) -> None:
        with self._lock:
            self._conn.execute(
                "UPDATE jobs SET error_json = ? WHERE job_id = ?",
                (json.dumps(error, default=str), job_id),
            )

    def set_backend(self, job_id: str, backend: str) -> None:
        with self._lock:
            self._conn.execute(
                "UPDATE jobs SET backend = ? WHERE job_id = ?", (backend, job_id)
            )

    def set_hf(self, job_id: str, hf_job_id: str, hf_job_url: str | None) -> None:
        with self._lock:
            self._conn.execute(
                "UPDATE jobs SET hf_job_id = ?, hf_job_url = ? WHERE job_id = ?",
                (hf_job_id, hf_job_url, job_id),
            )

    def set_provenance_ref(self, job_id: str, ref: str) -> None:
        with self._lock:
            self._conn.execute(
                "UPDATE jobs SET provenance_ref = ? WHERE job_id = ?", (ref, job_id)
            )

    def close(self) -> None:
        with self._lock:
            self._conn.close()


def _row_to_record(row: sqlite3.Row) -> JobRecord:
    from ..schemas import JobProgress  # local import to avoid cycle on test load

    return JobRecord(
        job_id=row["job_id"],
        tool_name=row["tool_name"],
        status=row["status"],
        backend=row["backend"],
        progress=JobProgress(
            percent=row["progress_pct"] or 0.0,
            message=row["progress_msg"] or "",
            step=row["progress_step"] or 0,
            total=row["progress_total"] or 0,
        ),
        result=json.loads(row["result_json"]) if row["result_json"] else None,
        error=json.loads(row["error_json"]) if row["error_json"] else None,
        started_at=row["started_at"],
        finished_at=row["finished_at"],
        owner_pid=row["owner_pid"] if "owner_pid" in row.keys() else None,
        hf_job_id=row["hf_job_id"],
        hf_job_url=row["hf_job_url"],
        cache_key=row["cache_key"],
        provenance_ref=row["provenance_ref"],
        summary_input=_safe_summary(row["input_json"]),
    )


def _safe_summary(s: str | None) -> dict[str, Any] | None:
    if not s:
        return None
    try:
        d = json.loads(s)
    except json.JSONDecodeError:
        return None
    # Truncate to a small subset of top-level keys to keep list views readable.
    if not isinstance(d, dict):
        return None
    return {k: d[k] for k in list(d)[:6]}
