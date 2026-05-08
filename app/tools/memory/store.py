"""Artifact store — SQLite + FTS5 hybrid retrieval for tool outputs.

Single DB at the configured path (default: ~/.prism/artifacts.db).
WAL mode + production PRAGMAs (busy_timeout, mmap, cache, temp_store).
Hybrid recall: BM25 (FTS5) + cosine (per-row vectors) + RRF fusion.

ATOMICITY: Every record() / update_embedding() is one BEGIN IMMEDIATE
transaction. On any error mid-record the artifact is rolled back.

THREADING: Each public method opens its own connection. The store is
thread-safe under WAL mode + busy_timeout=5000.

PROCESS: Multi-process safe — multiple PRISM agent subprocesses can
share the same DB file under WAL.

See docs/stateful_tools_2026.md for the architecture and Revisions
section for the rationale behind the single-DB + FTS5 hybrid choices.
"""
from __future__ import annotations

import base64
import json
import logging
import os
import secrets
import sqlite3
import struct
import threading
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable, Optional

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Schema. FTS5 virtual table is content-shared with `artifacts.summary` so we
# don't double-store text. Per-record FTS5 mirrors `artifact_records.record_summary`.
# ---------------------------------------------------------------------------
_SCHEMA_VERSION = 1

_SCHEMA = """
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY
);

CREATE TABLE IF NOT EXISTS artifacts (
    id              TEXT PRIMARY KEY,
    session_id      TEXT NOT NULL,
    tool_name       TEXT NOT NULL,
    args_json       TEXT NOT NULL,
    result_json     TEXT NOT NULL,
    summary         TEXT NOT NULL,
    embedding       BLOB,
    record_count    INTEGER,
    bytes_size      INTEGER NOT NULL,
    created_at      TEXT NOT NULL,
    promoted_to_kg  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS artifact_records (
    artifact_id     TEXT NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    record_idx      INTEGER NOT NULL,
    record_json     TEXT NOT NULL,
    record_summary  TEXT NOT NULL,
    embedding       BLOB,
    PRIMARY KEY (artifact_id, record_idx)
);

CREATE INDEX IF NOT EXISTS idx_artifacts_session ON artifacts(session_id);
CREATE INDEX IF NOT EXISTS idx_artifacts_tool    ON artifacts(tool_name);
CREATE INDEX IF NOT EXISTS idx_artifacts_created ON artifacts(created_at);

-- FTS5 over artifact summaries (BM25 ranking). Content-rowid links to artifacts.id
-- via the rowid mapping set up in triggers below.
CREATE VIRTUAL TABLE IF NOT EXISTS artifacts_fts USING fts5(
    summary,
    tool_name UNINDEXED,
    artifact_id UNINDEXED,
    tokenize = 'porter unicode61'
);

CREATE VIRTUAL TABLE IF NOT EXISTS records_fts USING fts5(
    record_summary,
    artifact_id UNINDEXED,
    record_idx UNINDEXED,
    tokenize = 'porter unicode61'
);

-- Triggers: keep FTS5 in sync with the source tables.
-- For artifacts:
CREATE TRIGGER IF NOT EXISTS artifacts_ai AFTER INSERT ON artifacts BEGIN
    INSERT INTO artifacts_fts(rowid, summary, tool_name, artifact_id)
    VALUES (new.rowid, new.summary, new.tool_name, new.id);
END;
CREATE TRIGGER IF NOT EXISTS artifacts_ad AFTER DELETE ON artifacts BEGIN
    DELETE FROM artifacts_fts WHERE rowid = old.rowid;
END;
CREATE TRIGGER IF NOT EXISTS artifacts_au AFTER UPDATE OF summary ON artifacts BEGIN
    UPDATE artifacts_fts SET summary = new.summary WHERE rowid = new.rowid;
END;

-- For artifact_records (composite key, so we use a hash-derived rowid by
-- joining artifact_id + record_idx via concatenation in a string column):
CREATE TRIGGER IF NOT EXISTS records_ai AFTER INSERT ON artifact_records BEGIN
    INSERT INTO records_fts(record_summary, artifact_id, record_idx)
    VALUES (new.record_summary, new.artifact_id, new.record_idx);
END;
"""


@dataclass(frozen=True)
class ArtifactRow:
    """Read-only view of one artifact row."""
    id: str
    session_id: str
    tool_name: str
    args_json: str
    result_json: str
    summary: str
    record_count: Optional[int]
    bytes_size: int
    created_at: str
    promoted_to_kg: bool

    @property
    def args(self) -> dict:
        return json.loads(self.args_json)

    @property
    def result(self) -> Any:
        return json.loads(self.result_json)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _new_artifact_id() -> str:
    """Stable short id; ~5e9 collisions per billion is fine for laptop scale."""
    raw = secrets.token_bytes(5)
    return "art_" + base64.b32encode(raw).decode("ascii").rstrip("=").lower()


def _canonical_json(obj: Any) -> str:
    """Sorted-keys, no-whitespace JSON for reproducible storage."""
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), default=str)


def _vec_to_blob(vec: Optional[Iterable[float]]) -> Optional[bytes]:
    if vec is None:
        return None
    arr = list(vec)
    return struct.pack(f"<{len(arr)}f", *arr)


def _blob_to_vec(blob: Optional[bytes]) -> Optional[list[float]]:
    if blob is None or len(blob) == 0:
        return None
    n = len(blob) // 4
    return list(struct.unpack(f"<{n}f", blob))


def _cosine(a: list[float], b: list[float]) -> float:
    """Cosine similarity. NumPy if available; pure Python otherwise."""
    if len(a) != len(b):
        return 0.0
    try:
        import numpy as np  # type: ignore
        av = np.asarray(a, dtype=np.float32)
        bv = np.asarray(b, dtype=np.float32)
        denom = float(np.linalg.norm(av) * np.linalg.norm(bv))
        if denom == 0.0:
            return 0.0
        return float(np.dot(av, bv) / denom)
    except Exception:
        dot = sum(x * y for x, y in zip(a, b))
        na = sum(x * x for x in a) ** 0.5
        nb = sum(y * y for y in b) ** 0.5
        if na == 0.0 or nb == 0.0:
            return 0.0
        return dot / (na * nb)


def _summarize(result: Any, tool_name: str, max_len: int = 280) -> str:
    """Compact one-line description for embedding + display."""
    if not isinstance(result, dict):
        text = _canonical_json(result)
        return f"{tool_name}: {text[:max_len]}"

    bits: list[str] = [tool_name]

    for k in ("count", "n", "total", "total_count"):
        if isinstance(result.get(k), (int, float)):
            bits.append(f"{k}={result[k]}")
            break

    for k in ("query", "name", "title", "topic", "question"):
        v = result.get(k)
        if isinstance(v, str) and v:
            bits.append(f"{k}={v[:80]}")
            break

    if isinstance(result.get("source"), str):
        bits.append(f"source={result['source']}")

    tail = _canonical_json(result)
    if len(tail) > max_len:
        tail = tail[:max_len] + "…"
    bits.append(tail)

    return " | ".join(bits)[:max_len * 2]


def _summarize_record(record: Any, idx: int, max_len: int = 200) -> str:
    """One-line description of a single record in a list-shaped result."""
    if isinstance(record, dict):
        for k in ("name", "id", "formula", "title", "doi", "label", "doc_id"):
            v = record.get(k)
            if isinstance(v, str) and v:
                tail = _canonical_json({kk: vv for kk, vv in record.items() if kk != k})
                slack = max(0, max_len - len(v) - 5)
                if len(tail) > slack:
                    tail = tail[:slack] + "…"
                return f"{v} | {tail}"
    text = _canonical_json(record)
    return text[:max_len]


def _extract_records(result: Any) -> Optional[list[Any]]:
    """If `result` is list-shaped, return the list. None otherwise."""
    if not isinstance(result, dict):
        if isinstance(result, list) and len(result) >= 2:
            return result
        return None
    for k in ("results", "data", "papers", "patents", "materials", "entities",
             "services", "subscriptions", "corpora", "providers", "gpus",
             "models", "pretrained_models", "trained_models"):
        v = result.get(k)
        if isinstance(v, list) and len(v) >= 2:
            return v
    return None


# ---------------------------------------------------------------------------
# RRF — Reciprocal Rank Fusion. k=60 is the canonical constant.
# ---------------------------------------------------------------------------

def _rrf(rankings: list[list[Any]], k: int = 60) -> list[tuple[Any, float]]:
    """Merge multiple rankings by reciprocal rank fusion.

    Each ranking is an ordered list of identifiers (any hashable). Items
    appearing in multiple rankings get summed `1/(k+rank)` scores.
    """
    scores: dict[Any, float] = {}
    for ranking in rankings:
        for rank, item in enumerate(ranking, start=1):
            scores[item] = scores.get(item, 0.0) + 1.0 / (k + rank)
    return sorted(scores.items(), key=lambda t: t[1], reverse=True)


# ---------------------------------------------------------------------------
# Default DB path
# ---------------------------------------------------------------------------

def default_db_path() -> Path:
    """Resolve the default artifact DB location.

    Honors PRISM_ARTIFACT_DB env var, else ~/.prism/artifacts.db.
    Directory is created on first access (via ArtifactStore.__init__).
    """
    override = os.environ.get("PRISM_ARTIFACT_DB")
    if override:
        return Path(override).expanduser()
    return Path.home() / ".prism" / "artifacts.db"


# ---------------------------------------------------------------------------
# Store
# ---------------------------------------------------------------------------

class ArtifactStore:
    """SQLite + FTS5 hybrid artifact store.

    Single DB file. WAL mode. Production-tuned PRAGMAs. Hybrid recall
    via BM25 (FTS5) + cosine (per-row vectors) + RRF fusion.
    """

    def __init__(self, db_path: Optional[Path | str] = None) -> None:
        self.db_path = Path(db_path) if db_path else default_db_path()
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        # serialize writes within this process so we don't trip
        # SQLITE_BUSY at high tool-call rates; readers go straight through
        self._write_lock = threading.Lock()
        self._init_schema()

    def _connect(self) -> sqlite3.Connection:
        # isolation_level=None lets us drive transactions explicitly with BEGIN/COMMIT
        conn = sqlite3.connect(self.db_path, timeout=10.0, isolation_level=None)
        # Production PRAGMAs — these MUST be set on every connection
        conn.execute("PRAGMA journal_mode = WAL")
        conn.execute("PRAGMA synchronous = NORMAL")
        conn.execute("PRAGMA busy_timeout = 5000")
        conn.execute("PRAGMA mmap_size = 8388608")
        conn.execute("PRAGMA cache_size = -2000")
        conn.execute("PRAGMA temp_store = MEMORY")
        conn.execute("PRAGMA foreign_keys = ON")
        return conn

    def _init_schema(self) -> None:
        conn = self._connect()
        try:
            conn.executescript(_SCHEMA)
            cur = conn.execute("SELECT version FROM schema_version LIMIT 1")
            row = cur.fetchone()
            if row is None:
                conn.execute("INSERT INTO schema_version(version) VALUES (?)", (_SCHEMA_VERSION,))
        finally:
            conn.close()

    # ------------------------------------------------------------------
    # Recording
    # ------------------------------------------------------------------

    def record(
        self,
        *,
        tool_name: str,
        args: dict,
        result: Any,
        session_id: str,
        summary: Optional[str] = None,
        embedding: Optional[Iterable[float]] = None,
        record_embeddings: Optional[list[Optional[Iterable[float]]]] = None,
    ) -> str:
        """Insert one artifact + per-record rows atomically. Returns the artifact id."""
        if not tool_name:
            raise ValueError("tool_name required")
        if not session_id:
            raise ValueError("session_id required")

        artifact_id = _new_artifact_id()
        args_json = _canonical_json(args)
        result_json = _canonical_json(result)
        sum_text = summary or _summarize(result, tool_name)
        records = _extract_records(result)
        record_count = len(records) if records else None
        bytes_size = len(result_json.encode("utf-8"))
        created_at = datetime.now(timezone.utc).isoformat(timespec="seconds")

        with self._write_lock:
            conn = self._connect()
            try:
                conn.execute("BEGIN IMMEDIATE")
                conn.execute(
                    """
                    INSERT INTO artifacts (id, session_id, tool_name, args_json,
                                           result_json, summary, embedding,
                                           record_count, bytes_size, created_at,
                                           promoted_to_kg)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)
                    """,
                    (artifact_id, session_id, tool_name, args_json,
                     result_json, sum_text, _vec_to_blob(embedding),
                     record_count, bytes_size, created_at),
                )
                if records:
                    rec_embs = record_embeddings or [None] * len(records)
                    if len(rec_embs) != len(records):
                        rec_embs = [None] * len(records)
                    rows = []
                    for i, rec in enumerate(records):
                        rec_summary = _summarize_record(rec, i)
                        rec_emb = _vec_to_blob(rec_embs[i])
                        rows.append((artifact_id, i, _canonical_json(rec), rec_summary, rec_emb))
                    conn.executemany(
                        """
                        INSERT INTO artifact_records
                            (artifact_id, record_idx, record_json,
                             record_summary, embedding)
                        VALUES (?, ?, ?, ?, ?)
                        """,
                        rows,
                    )
                conn.execute("COMMIT")
            except Exception:
                conn.execute("ROLLBACK")
                raise
            finally:
                conn.close()
        return artifact_id

    def update_embedding(
        self,
        *,
        artifact_id: str,
        embedding: Optional[Iterable[float]] = None,
        record_embeddings: Optional[list[Optional[Iterable[float]]]] = None,
    ) -> None:
        """Backfill embeddings asynchronously after record() returned."""
        with self._write_lock:
            conn = self._connect()
            try:
                conn.execute("BEGIN IMMEDIATE")
                if embedding is not None:
                    conn.execute(
                        "UPDATE artifacts SET embedding = ? WHERE id = ?",
                        (_vec_to_blob(embedding), artifact_id),
                    )
                if record_embeddings is not None:
                    for i, emb in enumerate(record_embeddings):
                        if emb is None:
                            continue
                        conn.execute(
                            """
                            UPDATE artifact_records
                            SET embedding = ?
                            WHERE artifact_id = ? AND record_idx = ?
                            """,
                            (_vec_to_blob(emb), artifact_id, i),
                        )
                conn.execute("COMMIT")
            except Exception:
                conn.execute("ROLLBACK")
                raise
            finally:
                conn.close()

    def mark_promoted(self, artifact_id: str) -> None:
        with self._write_lock:
            conn = self._connect()
            try:
                conn.execute(
                    "UPDATE artifacts SET promoted_to_kg = 1 WHERE id = ?",
                    (artifact_id,),
                )
            finally:
                conn.close()

    # ------------------------------------------------------------------
    # Reading
    # ------------------------------------------------------------------

    def get(self, artifact_id: str) -> Optional[ArtifactRow]:
        conn = self._connect()
        try:
            row = conn.execute(
                """
                SELECT id, session_id, tool_name, args_json, result_json,
                       summary, record_count, bytes_size, created_at,
                       promoted_to_kg
                FROM artifacts WHERE id = ?
                """,
                (artifact_id,),
            ).fetchone()
            if row is None:
                return None
            return ArtifactRow(
                id=row[0], session_id=row[1], tool_name=row[2],
                args_json=row[3], result_json=row[4], summary=row[5],
                record_count=row[6], bytes_size=row[7], created_at=row[8],
                promoted_to_kg=bool(row[9]),
            )
        finally:
            conn.close()

    def get_record(self, artifact_id: str, record_idx: int) -> Optional[Any]:
        conn = self._connect()
        try:
            row = conn.execute(
                "SELECT record_json FROM artifact_records WHERE artifact_id = ? AND record_idx = ?",
                (artifact_id, record_idx),
            ).fetchone()
            return json.loads(row[0]) if row else None
        finally:
            conn.close()

    def list_artifacts(
        self,
        *,
        session_id: Optional[str] = None,
        tool_name: Optional[str] = None,
        since: Optional[str] = None,
        limit: int = 20,
    ) -> list[dict]:
        clauses = ["1=1"]
        params: list[Any] = []
        if session_id:
            clauses.append("session_id = ?")
            params.append(session_id)
        if tool_name:
            clauses.append("tool_name = ?")
            params.append(tool_name)
        if since:
            clauses.append("created_at >= ?")
            params.append(since)
        params.append(limit)
        conn = self._connect()
        try:
            rows = conn.execute(
                f"""
                SELECT id, tool_name, summary, record_count, bytes_size,
                       created_at, promoted_to_kg, session_id
                FROM artifacts
                WHERE {' AND '.join(clauses)}
                ORDER BY created_at DESC, rowid DESC
                LIMIT ?
                """,
                params,
            ).fetchall()
            return [
                {
                    "artifact_id": r[0], "tool": r[1], "summary": r[2],
                    "record_count": r[3], "bytes_size": r[4],
                    "created_at": r[5], "promoted_to_kg": bool(r[6]),
                    "session_id": r[7],
                }
                for r in rows
            ]
        finally:
            conn.close()

    # ------------------------------------------------------------------
    # Hybrid recall — BM25 + vector + RRF fusion
    # ------------------------------------------------------------------

    def recall(
        self,
        *,
        query_text: str,
        query_embedding: Optional[list[float]] = None,
        session_id: Optional[str] = None,
        tool_name: Optional[str] = None,
        limit: int = 10,
        candidate_pool: int = 100,
    ) -> list[dict]:
        """Hybrid recall: BM25 (FTS5) + cosine over vectors, RRF-fused.

        If `query_embedding` is None, falls back to BM25-only.

        Returns hits ordered by RRF score descending. Each hit can be
        artifact-level (no `record_idx`) or record-level (`record_idx` set).
        """
        # BM25 candidate sets
        bm25_artifacts = self._bm25_artifact_search(
            query_text, session_id=session_id, tool_name=tool_name,
            limit=candidate_pool,
        )
        bm25_records = self._bm25_record_search(
            query_text, session_id=session_id, tool_name=tool_name,
            limit=candidate_pool,
        )

        # Vector candidate sets (only if embedding provided)
        if query_embedding is not None:
            vec_artifacts = self._vec_artifact_search(
                query_embedding, session_id=session_id, tool_name=tool_name,
                limit=candidate_pool,
            )
            vec_records = self._vec_record_search(
                query_embedding, session_id=session_id, tool_name=tool_name,
                limit=candidate_pool,
            )
        else:
            vec_artifacts = []
            vec_records = []

        # Build ranking lists keyed by a hashable item identifier.
        # Artifact-level hits use (id, None); record-level use (artifact_id, record_idx).
        def _key(hit: dict) -> tuple[str, Optional[int]]:
            return (hit["artifact_id"], hit.get("record_idx"))

        # Map keys → metadata for final lookup
        meta: dict[tuple[str, Optional[int]], dict] = {}
        for hit in bm25_artifacts + vec_artifacts + bm25_records + vec_records:
            k = _key(hit)
            if k not in meta:
                meta[k] = hit

        ranked = _rrf([
            [_key(h) for h in bm25_artifacts],
            [_key(h) for h in vec_artifacts],
            [_key(h) for h in bm25_records],
            [_key(h) for h in vec_records],
        ])

        out: list[dict] = []
        for key, score in ranked[:limit]:
            entry = dict(meta[key])
            entry["score"] = score
            out.append(entry)
        return out

    # ------------------------------------------------------------------
    # Internal: BM25 + vector candidate generators
    # ------------------------------------------------------------------

    def _bm25_artifact_search(
        self,
        query: str,
        *,
        session_id: Optional[str],
        tool_name: Optional[str],
        limit: int,
    ) -> list[dict]:
        if not query.strip():
            return []
        # FTS5 MATCH against artifact summaries; join back to artifacts for filters
        conn = self._connect()
        try:
            rows = conn.execute(
                """
                SELECT a.id, a.tool_name, a.summary, a.created_at,
                       a.record_count, a.session_id, bm25(artifacts_fts) AS rank
                FROM artifacts_fts
                JOIN artifacts a ON a.rowid = artifacts_fts.rowid
                WHERE artifacts_fts MATCH ?
                  AND (? IS NULL OR a.session_id = ?)
                  AND (? IS NULL OR a.tool_name = ?)
                ORDER BY rank
                LIMIT ?
                """,
                (self._fts_escape(query), session_id, session_id,
                 tool_name, tool_name, limit),
            ).fetchall()
            return [
                {
                    "artifact_id": r[0], "tool": r[1], "summary": r[2],
                    "created_at": r[3], "record_count": r[4],
                    "session_id": r[5],
                }
                for r in rows
            ]
        except sqlite3.OperationalError as e:
            logger.debug("FTS5 artifact search failed (%s); skipping", e)
            return []
        finally:
            conn.close()

    def _bm25_record_search(
        self,
        query: str,
        *,
        session_id: Optional[str],
        tool_name: Optional[str],
        limit: int,
    ) -> list[dict]:
        if not query.strip():
            return []
        conn = self._connect()
        try:
            rows = conn.execute(
                """
                SELECT records_fts.artifact_id, records_fts.record_idx,
                       records_fts.record_summary, a.tool_name,
                       a.session_id, a.created_at,
                       bm25(records_fts) AS rank
                FROM records_fts
                JOIN artifacts a ON a.id = records_fts.artifact_id
                WHERE records_fts MATCH ?
                  AND (? IS NULL OR a.session_id = ?)
                  AND (? IS NULL OR a.tool_name = ?)
                ORDER BY rank
                LIMIT ?
                """,
                (self._fts_escape(query), session_id, session_id,
                 tool_name, tool_name, limit),
            ).fetchall()
            return [
                {
                    "artifact_id": r[0], "record_idx": r[1],
                    "summary": r[2], "tool": r[3],
                    "session_id": r[4], "created_at": r[5],
                }
                for r in rows
            ]
        except sqlite3.OperationalError as e:
            logger.debug("FTS5 record search failed (%s); skipping", e)
            return []
        finally:
            conn.close()

    def _vec_artifact_search(
        self,
        query_embedding: list[float],
        *,
        session_id: Optional[str],
        tool_name: Optional[str],
        limit: int,
    ) -> list[dict]:
        clauses = ["a.embedding IS NOT NULL"]
        params: list[Any] = []
        if session_id:
            clauses.append("a.session_id = ?")
            params.append(session_id)
        if tool_name:
            clauses.append("a.tool_name = ?")
            params.append(tool_name)
        params.append(min(limit * 5, 500))  # candidate pool for cosine

        conn = self._connect()
        try:
            rows = conn.execute(
                f"""
                SELECT a.id, a.tool_name, a.summary, a.created_at,
                       a.embedding, a.record_count, a.session_id
                FROM artifacts a
                WHERE {' AND '.join(clauses)}
                ORDER BY a.created_at DESC
                LIMIT ?
                """,
                params,
            ).fetchall()

            scored: list[tuple[float, dict]] = []
            for r in rows:
                emb = _blob_to_vec(r[4])
                if not emb:
                    continue
                s = _cosine(query_embedding, emb)
                scored.append((s, {
                    "artifact_id": r[0], "tool": r[1], "summary": r[2],
                    "created_at": r[3], "record_count": r[5],
                    "session_id": r[6],
                }))
            scored.sort(key=lambda t: t[0], reverse=True)
            return [h for _, h in scored[:limit]]
        finally:
            conn.close()

    def _vec_record_search(
        self,
        query_embedding: list[float],
        *,
        session_id: Optional[str],
        tool_name: Optional[str],
        limit: int,
    ) -> list[dict]:
        clauses = ["ar.embedding IS NOT NULL"]
        params: list[Any] = []
        if session_id:
            clauses.append("a.session_id = ?")
            params.append(session_id)
        if tool_name:
            clauses.append("a.tool_name = ?")
            params.append(tool_name)
        params.append(min(limit * 5, 500))

        conn = self._connect()
        try:
            rows = conn.execute(
                f"""
                SELECT ar.artifact_id, ar.record_idx, ar.record_summary,
                       ar.embedding, a.tool_name, a.session_id, a.created_at
                FROM artifact_records ar
                JOIN artifacts a ON a.id = ar.artifact_id
                WHERE {' AND '.join(clauses)}
                ORDER BY a.created_at DESC
                LIMIT ?
                """,
                params,
            ).fetchall()

            scored: list[tuple[float, dict]] = []
            for r in rows:
                emb = _blob_to_vec(r[3])
                if not emb:
                    continue
                s = _cosine(query_embedding, emb)
                scored.append((s, {
                    "artifact_id": r[0], "record_idx": r[1],
                    "summary": r[2], "tool": r[4],
                    "session_id": r[5], "created_at": r[6],
                }))
            scored.sort(key=lambda t: t[0], reverse=True)
            return [h for _, h in scored[:limit]]
        finally:
            conn.close()

    @staticmethod
    def _fts_escape(query: str) -> str:
        """Make user input safe for FTS5 MATCH. Quote tokens to disable
        FTS5's syntax (NEAR, AND, OR, NOT, ^, etc.) so user queries can't
        accidentally break the parser."""
        # Strip quotes from the user input then wrap the whole thing.
        # FTS5 phrase queries: "tokens" → matches the whole phrase.
        # We split into tokens and rejoin with OR semantics for recall-style.
        tokens = [t for t in query.replace('"', " ").split() if t.strip()]
        if not tokens:
            return ""
        # Quote each token to escape; OR them.
        return " OR ".join(f'"{t}"' for t in tokens)
