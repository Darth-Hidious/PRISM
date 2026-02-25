"""Provider registry -- discover, register, route queries."""
from __future__ import annotations

from app.search.providers.base import Provider, ProviderCapabilities
from app.search.providers.endpoint import load_registry
from app.search.providers.optimade import OptimadeProvider
from app.search.providers.materials_project import MaterialsProjectProvider
from app.search.query import MaterialSearchQuery


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
    def from_registry_json(cls) -> ProviderRegistry:
        """Build registry from the bundled provider_registry.json."""
        reg = cls()
        endpoints = load_registry()
        for ep in endpoints:
            if not ep.enabled or not ep.base_url:
                continue
            if ep.api_type == "optimade":
                reg.register(OptimadeProvider(endpoint=ep))
            elif ep.api_type == "mp_native":
                reg.register(MaterialsProjectProvider(endpoint=ep))
        return reg
