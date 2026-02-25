"""Provider endpoint configuration — loaded from provider_registry.json."""
from __future__ import annotations

import json
from pathlib import Path
from typing import Literal

from pydantic import BaseModel, Field


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


_REGISTRY_PATH = Path(__file__).parent / "provider_registry.json"


def load_registry(path: Path | None = None) -> list[ProviderEndpoint]:
    """Load all provider endpoints from the registry JSON."""
    p = path or _REGISTRY_PATH
    data = json.loads(p.read_text())
    return [ProviderEndpoint.model_validate(entry) for entry in data["providers"]]
