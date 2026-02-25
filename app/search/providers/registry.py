"""Provider registry -- discover, build, route queries."""
from __future__ import annotations

import logging

from app.search.providers.base import Provider, ProviderCapabilities
from app.search.providers.endpoint import ProviderEndpoint
from app.search.providers.optimade import OptimadeProvider
from app.search.providers.materials_project import MaterialsProjectProvider
from app.search.query import MaterialSearchQuery

logger = logging.getLogger(__name__)


class ProviderRegistry:
    """Manages all registered providers."""

    def __init__(self):
        self._providers: dict[str, Provider] = {}

    def register(self, provider: Provider) -> None:
        self._providers[provider.id] = provider

    def get_all(self) -> list[Provider]:
        return list(self._providers.values())

    def get_capable(self, query: MaterialSearchQuery) -> list[Provider]:
        """Return only providers that can handle this query's filters."""
        capable = []
        for p in self._providers.values():
            if query.providers and p.id not in query.providers:
                continue
            if p.capabilities.can_handle(query):
                capable.append(p)
        return capable

    @classmethod
    def from_endpoints(cls, endpoints: list[dict]) -> ProviderRegistry:
        """Build registry from resolved endpoint dicts."""
        reg = cls()
        for ep_data in endpoints:
            if not ep_data.get("enabled", True):
                continue
            if not ep_data.get("base_url"):
                continue
            try:
                ep = ProviderEndpoint.model_validate(ep_data)
                if ep.api_type == "optimade":
                    reg.register(OptimadeProvider(endpoint=ep))
                elif ep.api_type == "mp_native":
                    reg.register(MaterialsProjectProvider(endpoint=ep))
            except Exception as e:
                logger.debug("Skipping provider %s: %s", ep_data.get("id"), e)
        return reg

    # Keep backward compat -- delegates to build_registry
    @classmethod
    def from_registry_json(cls) -> ProviderRegistry:
        """Legacy entry point -- calls build_registry()."""
        return build_registry()


def build_registry(
    cache_path=None,
    overrides_path=None,
    skip_network: bool = False,
) -> ProviderRegistry:
    """Build the provider registry from discovery cache + overrides.

    1. Load discovery cache (or run discovery if missing/stale)
    2. Apply bundled overrides
    3. Return ProviderRegistry
    """
    from pathlib import Path
    from app.search.providers.discovery import (
        load_cache, save_cache, is_cache_fresh, discover_providers,
        load_overrides, apply_overrides, DEFAULT_CACHE_PATH,
    )
    import asyncio

    c_path = cache_path or DEFAULT_CACHE_PATH
    cache = load_cache(c_path)

    if cache and is_cache_fresh(cache):
        endpoints = cache["endpoints"]
    elif not skip_network:
        # Discovery needed
        try:
            overrides_data = load_overrides(overrides_path)
            fallbacks = overrides_data.get("fallback_index_urls", {})
            endpoints = asyncio.run(discover_providers(fallback_index_urls=fallbacks))
            if endpoints:
                save_cache(endpoints, c_path)
            elif cache:
                # Discovery failed but stale cache exists -- use it
                logger.warning("Discovery failed, using stale cache")
                endpoints = cache["endpoints"]
            else:
                endpoints = []
        except Exception as e:
            logger.error("Discovery error: %s", e)
            endpoints = cache["endpoints"] if cache else []
    else:
        endpoints = cache["endpoints"] if cache else []

    # Apply overrides
    overrides_data = load_overrides(overrides_path)
    overrides = overrides_data.get("overrides", {})
    defaults = overrides_data.get("defaults", {})
    resolved = apply_overrides(endpoints, overrides, defaults)

    return ProviderRegistry.from_endpoints(resolved)
