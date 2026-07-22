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
    # S6: half-open probe guard. Without this, when a circuit transitions
    # open→half_open, every concurrent query in the fan-out sees half_open and
    # ALL probe a still-suspect provider at once (a thundering herd). This flag
    # ensures only ONE canonical probe runs per cooldown window; the rest skip
    # until the probe's result closes or re-opens the circuit.
    half_open_probe_claimed: bool = False

    def should_query(self, cooldown_seconds: float = 300.0) -> bool:
        """Check if this provider should be queried.

        Cooldown is 5 minutes (300s) — dead OPTIMADE endpoints don't
        come back quickly, so re-probing every 60s just wastes time.
        """
        if self.circuit_state == "closed":
            return True
        if self.circuit_state == "open":
            if self.last_failure and (
                time.time() - self.last_failure > cooldown_seconds
            ):
                self.circuit_state = "half_open"
                # Fall through to the half_open branch — only the FIRST caller
                # to reach here claims the probe; concurrent callers skip.
                self.half_open_probe_claimed = True
                return True
            return False
        # half_open: allow exactly ONE probe per cooldown window. Concurrent
        # callers skip until that probe's result (record_success/failure) clears
        # the claim.
        if self.half_open_probe_claimed:
            return False
        self.half_open_probe_claimed = True
        return True

    def record_success(self, latency_ms: float) -> None:
        self.consecutive_failures = 0
        self.circuit_state = "closed"
        self.half_open_probe_claimed = False
        self.success_count += 1
        if self.avg_latency_ms == 0:
            self.avg_latency_ms = latency_ms
        else:
            self.avg_latency_ms = 0.9 * self.avg_latency_ms + 0.1 * latency_ms

    def record_failure(self) -> None:
        self.consecutive_failures += 1
        self.failure_count += 1
        self.last_failure = time.time()
        self.half_open_probe_claimed = False
        if self.consecutive_failures >= 2:
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
        # half_open_probe_claimed is intentionally NOT persisted: it's a
        # per-process in-flight guard. A fresh process starts with no claim,
        # which is correct (the first query re-probes).
        data.pop("half_open_probe_claimed", None)
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
