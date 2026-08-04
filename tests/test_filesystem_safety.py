"""Shared-filesystem SQLite safety tests."""
from __future__ import annotations

import sqlite3

from app.tools.memory.store import ArtifactStore
from app.tools.simulation.mace.jobs.store import JobStore


def _journal_mode(connection: sqlite3.Connection) -> str:
    return str(connection.execute("PRAGMA journal_mode").fetchone()[0]).lower()


def test_artifact_store_avoids_wal_sidecars(tmp_path) -> None:
    path = tmp_path / "artifacts.db"
    store = ArtifactStore(path)
    conn = store._connect()
    try:
        assert _journal_mode(conn) == "delete"
    finally:
        conn.close()
    assert not path.with_name(path.name + "-wal").exists()
    assert not path.with_name(path.name + "-shm").exists()


def test_mace_job_store_avoids_wal_sidecars(tmp_path) -> None:
    path = tmp_path / "jobs.db"
    store = JobStore(path)
    assert _journal_mode(store._conn) == "delete"
    assert not path.with_name(path.name + "-wal").exists()
    assert not path.with_name(path.name + "-shm").exists()
