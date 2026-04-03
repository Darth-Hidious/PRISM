"""Per-provider circuit breaker with persistent health tracking."""
from __future__ import annotations

import json
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Literal


@dataclass
class ProviderHealth:
    provider_id: str
    consecutive_failures: int = 0
    circuit_state: Literal["closed", "open", "half_open"] = "closed"
    last_failure: float | None = None
    avg_latency_ms: float = 0.0
    success_count: int = 0
    failure_count: int = 0

    def should_query(self, cooldown_seconds: float = 60.0) -> bool:
        if self.circuit_state == "closed":
            return True
        if self.circuit_state == "open":
            if self.last_failure and (time.time() - self.last_failure > cooldown_seconds):
                self.circuit_state = "half_open"
                return True
            return False
        return True  # half_open -- allow one probe

    def record_success(self, latency_ms: float) -> None:
        self.consecutive_failures = 0
        self.circuit_state = "closed"
        self.success_count += 1
        if self.avg_latency_ms == 0:
            self.avg_latency_ms = latency_ms
        else:
            self.avg_latency_ms = 0.9 * self.avg_latency_ms + 0.1 * latency_ms

    def record_failure(self) -> None:
        self.consecutive_failures += 1
        self.failure_count += 1
        self.last_failure = time.time()
        if self.consecutive_failures >= 3:
            self.circuit_state = "open"

    def to_dict(self) -> dict:
        return {
            "provider_id": self.provider_id,
            "consecutive_failures": self.consecutive_failures,
            "circuit_state": self.circuit_state,
            "last_failure": self.last_failure,
            "avg_latency_ms": self.avg_latency_ms,
            "success_count": self.success_count,
            "failure_count": self.failure_count,
        }

    @classmethod
    def from_dict(cls, data: dict) -> ProviderHealth:
        return cls(**data)


class HealthManager:
    """Manages health state for all providers with persistence."""

    def __init__(self, persist_path: Path | None = None):
        self._health: dict[str, ProviderHealth] = {}
        self._persist_path = persist_path

    def get(self, provider_id: str) -> ProviderHealth:
        if provider_id not in self._health:
            self._health[provider_id] = ProviderHealth(provider_id=provider_id)
        return self._health[provider_id]

    def save(self) -> None:
        if not self._persist_path:
            return
        self._persist_path.parent.mkdir(parents=True, exist_ok=True)
        data = {pid: h.to_dict() for pid, h in self._health.items()}
        self._persist_path.write_text(json.dumps(data, indent=2))

    def load(self) -> None:
        if not self._persist_path or not self._persist_path.exists():
            return
        data = json.loads(self._persist_path.read_text())
        for pid, hdata in data.items():
            self._health[pid] = ProviderHealth.from_dict(hdata)
