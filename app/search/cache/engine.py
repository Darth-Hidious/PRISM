"""In-memory + disk-backed search cache."""
from __future__ import annotations

import json
import time
from dataclasses import dataclass, field
from pathlib import Path

from app.search.query import MaterialSearchQuery
from app.search.result import Material, SearchResult


@dataclass
class CachedResult:
    result: SearchResult
    timestamp: float = field(default_factory=time.time)
    ttl: float = 86400  # 24 hours
    hit_count: int = 0

    @property
    def is_fresh(self) -> bool:
        return (time.time() - self.timestamp) < self.ttl


class SearchCache:
    """In-memory + disk-backed search cache."""

    def __init__(self, disk_dir: Path | None = None, default_ttl: float = 86400):
        self._query_cache: dict[str, CachedResult] = {}
        self._material_index: dict[str, Material] = {}
        self._disk_dir = disk_dir
        self._default_ttl = default_ttl

    def get(self, query: MaterialSearchQuery) -> SearchResult | None:
        key = query.query_hash()
        cached = self._query_cache.get(key)
        if cached and cached.is_fresh:
            cached.hit_count += 1
            result = cached.result.model_copy()
            result.cached = True
            return result
        return None

    def put(self, query: MaterialSearchQuery, result: SearchResult) -> None:
        key = query.query_hash()
        self._query_cache[key] = CachedResult(
            result=result, ttl=self._default_ttl,
        )
        for m in result.materials:
            self._material_index[m.id] = m

    def get_material(self, material_id: str) -> Material | None:
        return self._material_index.get(material_id)

    def get_all_materials(self) -> list[Material]:
        return list(self._material_index.values())

    def stats(self) -> dict:
        return {
            "query_count": len(self._query_cache),
            "material_count": len(self._material_index),
            "fresh_queries": sum(1 for c in self._query_cache.values() if c.is_fresh),
            "total_hits": sum(c.hit_count for c in self._query_cache.values()),
        }

    def clear(self) -> None:
        self._query_cache.clear()
        self._material_index.clear()

    def flush_to_disk(self) -> None:
        if not self._disk_dir:
            return
        self._disk_dir.mkdir(parents=True, exist_ok=True)
        for key, cached in self._query_cache.items():
            path = self._disk_dir / f"{key}.json"
            data = {
                "query": cached.result.query.model_dump(mode="json"),
                "result": cached.result.model_dump(mode="json"),
                "timestamp": cached.timestamp,
                "ttl": cached.ttl,
            }
            path.write_text(json.dumps(data))

    def load_from_disk(self) -> None:
        if not self._disk_dir or not self._disk_dir.exists():
            return
        for path in self._disk_dir.glob("*.json"):
            try:
                data = json.loads(path.read_text())
                result = SearchResult.model_validate(data["result"])
                cached = CachedResult(
                    result=result,
                    timestamp=data.get("timestamp", time.time()),
                    ttl=data.get("ttl", self._default_ttl),
                )
                if cached.is_fresh:
                    key = path.stem
                    self._query_cache[key] = cached
                    for m in result.materials:
                        self._material_index[m.id] = m
            except Exception:
                continue  # skip corrupt cache entries
