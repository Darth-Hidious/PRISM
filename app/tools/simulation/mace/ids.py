"""Identifier and hashing helpers (ULIDs, cache keys, git SHA)."""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
from functools import lru_cache
from pathlib import Path

from ulid import ULID

from app.tools import spawn


def new_job_id() -> str:
    """Return a fresh ULID string."""
    return str(ULID())


def canonical_json(obj: object) -> bytes:
    """Stable JSON serialisation with sorted keys + compact separators."""
    return json.dumps(
        obj,
        sort_keys=True,
        separators=(",", ":"),
        default=_json_default,
        ensure_ascii=False,
    ).encode("utf-8")


def _json_default(o: object) -> object:
    if hasattr(o, "tolist"):  # numpy arrays
        return o.tolist()
    if hasattr(o, "isoformat"):  # datetime
        return o.isoformat()
    raise TypeError(f"unserializable: {type(o)!r}")


def sha256_of(obj: object) -> str:
    """SHA-256 hex digest of ``obj`` after canonical-JSON serialisation."""
    return hashlib.sha256(canonical_json(obj)).hexdigest()


def _git(root: Path, *args: str) -> str:
    """stdout of a read-only git command, or ``""`` if it could not run.

    Goes through app.tools.spawn: this is provenance, and it runs in the same
    process as the agent's materials search. A fork() there SIGSEGVs, git dies
    at -11, and the recorded commit silently degrades to "unknown" — a
    provenance record that looks complete and is wrong. See app/tools/spawn.py.
    """
    try:
        result = spawn.run(
            ["git", "-C", str(root), *args],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
    except Exception:
        return ""
    return result.stdout if result.returncode == 0 else ""


@lru_cache(maxsize=1)
def git_sha(repo_root: str | None = None) -> str:
    """Best-effort short SHA of the current commit. Falls back to ``"unknown"``."""
    root = Path(repo_root) if repo_root else Path(__file__).resolve().parents[2]
    if not (root / ".git").exists():
        return "unknown"
    return _git(root, "rev-parse", "--short=12", "HEAD").strip() or "unknown"


def git_dirty(repo_root: str | None = None) -> bool:
    """Whether the working tree has uncommitted changes."""
    root = Path(repo_root) if repo_root else Path(__file__).resolve().parents[2]
    if not (root / ".git").exists():
        return False
    return bool(_git(root, "status", "--porcelain").strip())
