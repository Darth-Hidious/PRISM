"""Result models for materials search — every property carries provenance."""
from __future__ import annotations

from typing import Literal

from pydantic import BaseModel, Field

from app.search.query import MaterialSearchQuery


class PropertyValue(BaseModel):
    """A single property with tracked provenance."""
    value: float | str | list | None = None
    source: str = ""
    method: str | None = None
    unit: str | None = None


class Material(BaseModel):
    """Unified material record fused across providers."""
    id: str
    formula: str
    elements: list[str]
    n_elements: int
    sources: list[str]

    space_group: PropertyValue | None = None
    crystal_system: PropertyValue | None = None
    lattice_vectors: PropertyValue | None = None
    band_gap: PropertyValue | None = None
    formation_energy: PropertyValue | None = None
    energy_above_hull: PropertyValue | None = None
    bulk_modulus: PropertyValue | None = None
    debye_temperature: PropertyValue | None = None

    extra_properties: dict[str, PropertyValue] = Field(default_factory=dict)
    raw: dict = Field(default_factory=dict, exclude=True)


class ProviderQueryLog(BaseModel):
    """Full audit of a single provider interaction."""
    provider_id: str
    provider_name: str
    endpoint_url: str
    query_sent: str

    started_at: float
    completed_at: float
    latency_ms: float

    status: Literal["success", "timeout", "http_error",
                    "parse_error", "circuit_open", "skipped"]
    http_status_code: int | None = None
    result_count: int = 0

    error_type: str | None = None
    error_message: str | None = None
    error_raw: str | None = None

    pages_fetched: int = 1
    truncated: bool = False


class SearchResult(BaseModel):
    """Everything that comes back from a search."""
    materials: list[Material]
    total_count: int
    query: MaterialSearchQuery

    query_log: list[ProviderQueryLog]

    warnings: list[str] = Field(default_factory=list)
    cached: bool = False
    search_time_ms: float = 0
    cache_stats: dict | None = None
