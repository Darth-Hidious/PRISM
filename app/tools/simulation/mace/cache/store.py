"""Disk-backed content-addressed cache.

Layout::

    <cache_root>/
      <sha256>/
        result.json         # tool result (serialised)
        structure.cif       # primary structure (if any)
        traj.json           # trajectory (md_equilibrate)
        provenance.json     # full provenance
        meta.json           # bookkeeping (tool, created_at, source_job_id)

Concurrent writes from multiple jobs go to a temp file and are renamed
atomically to avoid partial reads.
"""

from __future__ import annotations

import json
import os
import shutil
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


class CacheStore:
    def __init__(self, root: Path) -> None:
        self.root = Path(root)
        self.root.mkdir(parents=True, exist_ok=True)

    def entry(self, key: str) -> Path:
        d = self.root / key
        d.mkdir(parents=True, exist_ok=True)
        return d

    def has_result(self, key: str) -> bool:
        return (self.root / key / "result.json").exists()

    def write_result(self, key: str, result: dict[str, Any]) -> None:
        self._atomic_write_json(self.entry(key) / "result.json", result)

    def read_result(self, key: str) -> dict[str, Any]:
        return json.loads((self.root / key / "result.json").read_text())

    def write_provenance(self, key: str, prov: dict[str, Any]) -> None:
        self._atomic_write_json(self.entry(key) / "provenance.json", prov)

    def read_provenance(self, key: str) -> dict[str, Any] | None:
        p = self.root / key / "provenance.json"
        if not p.exists():
            return None
        return json.loads(p.read_text())

    def write_structure_cif(self, key: str, cif_text: str) -> None:
        self._atomic_write_text(self.entry(key) / "structure.cif", cif_text)

    def read_structure_cif(self, key: str) -> str | None:
        p = self.root / key / "structure.cif"
        if not p.exists():
            return None
        return p.read_text()

    def write_traj_json(self, key: str, traj: dict[str, Any]) -> None:
        self._atomic_write_json(self.entry(key) / "traj.json", traj)

    def write_meta(self, key: str, meta: dict[str, Any]) -> None:
        meta = dict(meta)
        meta.setdefault("created_at", datetime.now(timezone.utc).isoformat())
        self._atomic_write_json(self.entry(key) / "meta.json", meta)

    def read_meta(self, key: str) -> dict[str, Any] | None:
        p = self.root / key / "meta.json"
        if not p.exists():
            return None
        return json.loads(p.read_text())

    def delete(self, key: str) -> None:
        d = self.root / key
        if d.exists():
            shutil.rmtree(d)

    def list_keys(self) -> list[str]:
        return sorted(p.name for p in self.root.iterdir() if p.is_dir())

    # ------------------------------------------------------------------
    @staticmethod
    def _atomic_write_text(path: Path, text: str) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(
            "w", delete=False, dir=path.parent, suffix=".tmp", encoding="utf-8"
        ) as f:
            f.write(text)
            tmp = f.name
        os.replace(tmp, path)

    @classmethod
    def _atomic_write_json(cls, path: Path, obj: Any) -> None:
        cls._atomic_write_text(path, json.dumps(obj, indent=2, default=_json_default))


def _json_default(o: Any) -> Any:
    if hasattr(o, "tolist"):
        return o.tolist()
    if hasattr(o, "isoformat"):
        return o.isoformat()
    raise TypeError(f"unserializable: {type(o)!r}")
