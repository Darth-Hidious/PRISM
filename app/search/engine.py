"""SearchEngine -- the federated search orchestrator."""
from __future__ import annotations

import asyncio
import logging
import time
from pathlib import Path

from app.search.cache.engine import SearchCache
from app.search.fusion import fuse_materials
from app.search.providers.base import Provider
from app.search.providers.registry import ProviderRegistry
from app.search.query import MaterialSearchQuery
from app.search.resilience.circuit_breaker import HealthManager
from app.search.result import Material, ProviderQueryLog, SearchResult
from app.search.translator import QueryTranslator

logger = logging.getLogger(__name__)

DEFAULT_CACHE_DIR = Path.home() / ".prism" / "cache"
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
        global_timeout: float = 15.0,
    ):
        self._registry = registry
        self._cache = cache or SearchCache(disk_dir=DEFAULT_CACHE_DIR)
        self._health = health_manager or HealthManager(persist_path=DEFAULT_HEALTH_PATH)
        self._health.load()
        self._global_timeout = global_timeout

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    async def search(self, query: MaterialSearchQuery) -> SearchResult:
        """Fan out to providers, collect, fuse, rank, return."""
        start = time.time()

        # 1. Cache check
        cached = self._cache.get(query)
        if cached is not None:
            return cached

        # 2. Select capable providers with healthy circuits
        capable = self._registry.get_capable(query)
        providers = [
            p for p in capable
            if self._health.get(p.id).should_query()
        ]

        if not providers:
            return SearchResult(
                materials=[],
                total_count=0,
                query=query,
                query_log=[],
                warnings=["No providers available for this query"],
                search_time_ms=(time.time() - start) * 1000,
            )

        # 3. Fan out async
        tasks = {p.id: self._query_provider(p, query) for p in providers}
        results = await asyncio.gather(*tasks.values(), return_exceptions=True)
        provider_results = dict(zip(tasks.keys(), results))

        # 4. Collect results + build audit trail
        all_materials: list[Material] = []
        query_log: list[ProviderQueryLog] = []
        warnings: list[str] = []

        for pid, result in provider_results.items():
            provider = next(p for p in providers if p.id == pid)
            if isinstance(result, BaseException):
                self._health.get(pid).record_failure()
                log = ProviderQueryLog(
                    provider_id=pid,
                    provider_name=provider.name,
                    endpoint_url=self._get_endpoint_url(provider),
                    query_sent=QueryTranslator.to_optimade(query),
                    started_at=start,
                    completed_at=time.time(),
                    latency_ms=(time.time() - start) * 1000,
                    status="http_error",
                    error_type=type(result).__name__,
                    error_message=str(result)[:200],
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

        return search_result

    def get_provider_status(self) -> dict[str, dict]:
        """Health dashboard for all known providers."""
        return {
            pid: h.to_dict()
            for pid, h in self._health._health.items()
        }

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

        # Determine per-provider timeout
        timeout = self._global_timeout
        if hasattr(provider, "_endpoint") and provider._endpoint:
            ep = provider._endpoint
            if hasattr(ep, "behavior") and ep.behavior:
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
