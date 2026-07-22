"""SearchEngine -- the federated search orchestrator."""

from __future__ import annotations

import asyncio
import logging
import time
from pathlib import Path

from app.tools.search_engine.cache.engine import SearchCache
from app.tools.search_engine.fusion import fuse_materials
from app.tools.search_engine.providers.base import Provider
from app.tools.search_engine.providers.registry import ProviderRegistry
from app.tools.search_engine.query import MaterialSearchQuery
from app.tools.search_engine.resilience.circuit_breaker import HealthManager
from app.tools.search_engine.result import Material, ProviderQueryLog, SearchResult
from app.tools.search_engine.translator import QueryTranslator

logger = logging.getLogger(__name__)

DEFAULT_CACHE_DIR = Path.home() / ".prism" / "cache"


def _sanitize_error(msg: str) -> str:
    """Strip HTML tags and truncate to a single readable line."""
    import re

    clean = re.sub(r"<[^>]+>", "", msg)  # strip HTML tags
    clean = re.sub(r"\s+", " ", clean).strip()  # collapse whitespace
    return clean[:120] if clean else "unknown error"


DEFAULT_HEALTH_PATH = Path.home() / ".prism" / "cache" / "provider_health.json"


class SearchEngine:
    """Federated materials database search engine.

    Ties together provider registry, query translation, caching,
    circuit breakers, and result fusion into a single ``search()`` call.
    """

    def __init__(
        self,
        registry: ProviderRegistry,
        cache: SearchCache | None = None,
        health_manager: HealthManager | None = None,
        global_timeout: float = 8.0,
    ):
        self._registry = registry
        self._cache = cache or SearchCache(disk_dir=DEFAULT_CACHE_DIR)
        self._health = health_manager or HealthManager(persist_path=DEFAULT_HEALTH_PATH)
        self._health.load()
        # S5: the default whole-fan-out deadline. Raised from the old 5.0 to 8.0
        # so a healthy provider fan-out completes more often within budget; the
        # hard ceiling is now per-call overridable (search(timeout_seconds=...)).
        self._global_timeout = global_timeout

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    async def search(
        self, query: MaterialSearchQuery, timeout_seconds: float | None = None
    ) -> SearchResult:
        """Fan out to providers, collect, fuse, rank, return.

        ``timeout_seconds`` overrides the engine's default whole-fan-out
        deadline for THIS call only (S5). Capped at 30s so a runaway caller
        can't pin the agent indefinitely.
        """
        # S5: per-call deadline override (the agent may pass timeout_seconds).
        original_timeout = self._global_timeout
        if timeout_seconds is not None:
            self._global_timeout = min(max(float(timeout_seconds), 1.0), 30.0)
        start = time.time()
        # Carries the whole-fan-out deadline notice if S2's deadline fires.
        warnings: list[str] = []

        # 1. Cache check
        cached = self._cache.get(query)
        if cached is not None:
            self._global_timeout = original_timeout
            return cached

        # 2. Select capable providers with healthy circuits
        capable = self._registry.get_capable(query)
        providers = [p for p in capable if self._health.get(p.id).should_query()]

        if not providers:
            self._global_timeout = original_timeout
            return SearchResult(
                materials=[],
                total_count=0,
                query=query,
                query_log=[],
                warnings=["No providers available for this query"],
                search_time_ms=(time.time() - start) * 1000,
            )

        # 3. Fan out async with concurrency limit + early termination
        # Use a semaphore to avoid hammering 20+ providers simultaneously.
        # 8 concurrent connections is a good balance between speed and
        # being a good citizen to the OPTIMADE federation.
        semaphore = asyncio.Semaphore(8)
        # Early-return: once we have 2x the requested limit from fast
        # providers, cancel remaining slow ones. We over-fetch 2x so
        # fusion (dedup across providers) still yields enough results.
        early_target = query.limit * 2
        early_event = asyncio.Event()
        collected = {"count": 0}

        async def _guarded_query(p):
            if early_event.is_set():
                return [], ProviderQueryLog(
                    provider_id=p.id,
                    provider_name=p.name,
                    endpoint_url=self._get_endpoint_url(p),
                    query_sent="",
                    started_at=time.time(),
                    completed_at=time.time(),
                    latency_ms=0,
                    status="skipped",
                    error_message="Early termination — enough results from fast providers",
                )
            async with semaphore:
                materials, log = await self._query_provider(p, query)
                collected["count"] += len(materials)
                if collected["count"] >= early_target and not early_event.is_set():
                    early_event.set()
                return materials, log

        tasks: dict[str, asyncio.Task] = {
            p.id: asyncio.create_task(_guarded_query(p)) for p in providers
        }

        # S3: cancel-slow-on-early-complete. The old code's `early_event` only
        # short-circuited tasks that hadn't entered the semaphore yet — tasks
        # already running (the slow ones blocking the gather) were NEVER
        # cancelled despite the "cancel remaining slow ones" comment. This
        # watcher fires the moment we have enough results and cancels every
        # not-yet-complete task so the gather returns promptly instead of
        # waiting on the laggards.
        async def _early_canceller():
            await early_event.wait()
            for t in tasks.values():
                if not t.done():
                    t.cancel()

        canceller = asyncio.create_task(_early_canceller())

        # S2: a separate, hard whole-fan-out deadline. The per-provider timeout
        # (in _query_provider) is by UNION — each provider gets its OWN configured
        # timeout, never silently clipped to the global (the old `min(global, per)`
        # clipped OQMD's 15s to 5s). This deadline is the only global ceiling: it
        # guarantees the agent gets a result (partial or complete) within ~this bound
        # even if every provider is slow.
        try:
            results = await asyncio.wait_for(
                asyncio.gather(*tasks.values(), return_exceptions=True),
                timeout=self._global_timeout,
            )
        except asyncio.TimeoutError:
            # The whole fan-out exceeded the deadline. Cancel anything still in
            # flight so it doesn't keep running after we return, then collect
            # whatever each task had produced so far (None for not-started ones).
            for t in tasks.values():
                if not t.done():
                    t.cancel()
            # Gather again (no wait_for) to surface CancelledError as values and
            # preserve partial results already completed.
            results = await asyncio.gather(*tasks.values(), return_exceptions=True)
            warnings.append(
                f"Whole-fan-out deadline reached after {self._global_timeout:.0f}s — "
                "returning partial results; some providers were cancelled"
            )
        finally:
            canceller.cancel()

        provider_results = dict(zip(tasks.keys(), results))

        # 4. Collect results + build audit trail
        all_materials: list[Material] = []
        query_log: list[ProviderQueryLog] = []
        # `warnings` may already carry the whole-fan-out deadline notice set above.
        for pid, result in provider_results.items():
            provider = next(p for p in providers if p.id == pid)
            if isinstance(result, BaseException):
                # S1: the breaker + an honest log. Each task records its OWN
                # start (above), so latency here is per-provider, not the old
                # cumulative search-wide `start`.
                self._health.get(pid).record_failure()
                status = (
                    "timeout"
                    if isinstance(result, asyncio.CancelledError)
                    or isinstance(result, asyncio.TimeoutError)
                    else "http_error"
                )
                log = ProviderQueryLog(
                    provider_id=pid,
                    provider_name=provider.name,
                    endpoint_url=self._get_endpoint_url(provider),
                    query_sent=QueryTranslator.to_optimade(query),
                    started_at=start,
                    completed_at=time.time(),
                    latency_ms=(time.time() - start) * 1000,
                    status=status,
                    error_type=type(result).__name__,
                    error_message=_sanitize_error(str(result)),
                )
                query_log.append(log)
                warnings.append(f"Provider '{pid}' failed: {type(result).__name__}")
            else:
                materials, log_entry = result
                all_materials.extend(materials)
                query_log.append(log_entry)

        # 5. Fuse duplicates across providers
        fused = fuse_materials(all_materials)

        # 6. Apply limit
        fused = fused[: query.limit]

        # 7. Build result
        search_result = SearchResult(
            materials=fused,
            total_count=len(fused),
            query=query,
            query_log=query_log,
            warnings=warnings,
            search_time_ms=(time.time() - start) * 1000,
        )

        # 8. Cache and persist health
        self._cache.put(query, search_result)
        self._health.save()

        # S5: restore the default deadline (the override was per-call only).
        self._global_timeout = original_timeout
        return search_result

    def get_provider_status(self) -> dict[str, dict]:
        """Health dashboard for all known providers."""
        return {pid: h.to_dict() for pid, h in self._health._health.items()}

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    async def _query_provider(
        self,
        provider: Provider,
        query: MaterialSearchQuery,
    ) -> tuple[list[Material], ProviderQueryLog]:
        """Query a single provider with timeout and audit logging."""
        start = time.time()
        endpoint_url = self._get_endpoint_url(provider)
        query_sent = QueryTranslator.to_optimade(query)

        # S2: per-provider timeout by UNION. The provider's OWN configured
        # timeout wins; the global is a separate whole-fan-out deadline (see
        # search()), NOT a clip on each provider. The old `min(global, per)`
        # silently clipped OQMD's 15s override to the 5s default. Default to the
        # global only when the provider has no explicit timeout configured.
        timeout = self._global_timeout
        if hasattr(provider, "_endpoint") and provider._endpoint:
            ep = provider._endpoint
            if hasattr(ep, "behavior") and ep.behavior and ep.behavior.timeout_ms:
                timeout = ep.behavior.timeout_ms / 1000

        try:
            materials = await asyncio.wait_for(
                provider.search(query),
                timeout=timeout,
            )
            latency = (time.time() - start) * 1000
            self._health.get(provider.id).record_success(latency)

            log = ProviderQueryLog(
                provider_id=provider.id,
                provider_name=provider.name,
                endpoint_url=endpoint_url,
                query_sent=query_sent,
                started_at=start,
                completed_at=time.time(),
                latency_ms=latency,
                status="success",
                http_status_code=200,
                result_count=len(materials),
            )
            return materials, log

        except asyncio.TimeoutError:
            self._health.get(provider.id).record_failure()
            log = ProviderQueryLog(
                provider_id=provider.id,
                provider_name=provider.name,
                endpoint_url=endpoint_url,
                query_sent=query_sent,
                started_at=start,
                completed_at=time.time(),
                latency_ms=(time.time() - start) * 1000,
                status="timeout",
                error_type="TimeoutError",
                error_message=f"Timed out after {timeout}s",
            )
            return [], log

        except Exception:
            raise  # re-raise for gather(return_exceptions=True) to capture

    @staticmethod
    def _get_endpoint_url(provider: Provider) -> str:
        """Safely extract endpoint URL from provider, or fall back to id."""
        if hasattr(provider, "_endpoint") and provider._endpoint:
            url = getattr(provider._endpoint, "base_url", None)
            if url:
                return url
        return provider.id
