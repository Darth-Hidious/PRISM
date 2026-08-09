"""Provider endpoint configuration models."""
from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, Field, field_validator


class AuthConfig(BaseModel):
    required: bool = False
    auth_type: Literal["api_key", "oauth", "token", "none"] = "none"
    auth_header: str | None = None
    auth_env_var: str | None = None
    obtain_url: str | None = None
    notes: str | None = None


class BehaviorConfig(BaseModel):
    timeout_ms: int = 5000
    max_results: int = 1000
    rate_limit_rps: float | None = None
    use_https: bool = True


class CapabilitiesConfig(BaseModel):
    filterable_fields: list[str] = Field(default_factory=list)
    returned_properties: list[str] = Field(default_factory=list)
    provider_specific_fields: list[str] = Field(default_factory=list)
    supports_pagination: bool = True
    page_limit_values: list[int] | None = None


class ReliabilityConfig(BaseModel):
    validation_score: str = "unknown"
    known_quirks: list[str] = Field(default_factory=list)


class ProviderEndpoint(BaseModel):
    """Single provider connection config loaded from registry JSON."""
    id: str
    name: str
    description: str = ""
    homepage: str = ""
    base_url: str | None = None
    api_type: str = "optimade"
    api_version: str = "v1"
    structures_approx: int | None = None
    data_type: str = "computational"
    tier: int = 4
    enabled: bool = False
    status: str = "active"

    auth: AuthConfig = Field(default_factory=AuthConfig)
    behavior: BehaviorConfig = Field(default_factory=BehaviorConfig)
    capabilities: CapabilitiesConfig = Field(default_factory=CapabilitiesConfig)
    reliability: ReliabilityConfig = Field(default_factory=ReliabilityConfig)

    @field_validator("api_type")
    @classmethod
    def _normalize_api_type(cls, value: str) -> str:
        """Trim ``api_type`` exactly the way factory keys are trimmed at
        registration: a config entry ``"  optimade  "`` must route to the
        ``"optimade"`` factory, not silently match nothing and vanish."""
        return value.strip()


def load_registry():
    """Legacy compat -- returns list of ProviderEndpoint from the new registry.

    Prefer build_registry() directly.
    """
    from app.tools.search_engine.providers.registry import build_registry
    reg = build_registry()
    return [p._endpoint for p in reg.get_all() if hasattr(p, "_endpoint")]
