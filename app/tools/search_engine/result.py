"""Result models for materials search — every property carries provenance."""
from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, Field

from app.tools.search_engine.query import MaterialSearchQuery


class ExtractionProvenance(BaseModel):
    """How a value was transcribed from its underlying source.

    ``source`` on :class:`PropertyValue` names the provider, paper, or other
    authority that made the claim.  This separate record names the extraction
    path, so an LLM transcription error is not attributed to the paper itself.
    """

    extractor_id: str = "unknown"
    kind: Literal["structured_api", "llm_literature", "other", "unknown"] = "unknown"


class PropertyValue(BaseModel):
    """A single property with source and extraction provenance."""

    value: float | str | list | dict[str, Any] | None = None
    source: str = ""
    method: str | None = None
    unit: str | None = None
    extraction: ExtractionProvenance = Field(default_factory=ExtractionProvenance)


class MaterialIdentity(BaseModel):
    """A domain-supplied identity payload consumed by an identity plugin."""

    domain: str
    representation: str
    attributes: dict[str, str]


class FusionCandidate(BaseModel):
    """One audited input to a reliability-weighted property decision."""

    property_value: PropertyValue
    source: str
    extractor_id: str
    source_reliability: float
    extraction_reliability: float
    combined_weight: float
    selected: bool


class PropertyFusionAudit(BaseModel):
    """All candidates and weights used to select one fused property value."""

    resolved: bool
    selected_source: str | None = None
    candidates: list[FusionCandidate]


class Material(BaseModel):
    """Unified material record fused across providers."""
    id: str
    formula: str
    elements: list[str]
    n_elements: int
    sources: list[str]
    # None is retained only for legacy records.  Fusion leaves such a record
    # unfused rather than guessing it is a crystal from its formula.
    identity: MaterialIdentity | None = None

    space_group: PropertyValue | None = None
    crystal_system: PropertyValue | None = None
    lattice_vectors: PropertyValue | None = None
    band_gap: PropertyValue | None = None
    formation_energy: PropertyValue | None = None
    energy_above_hull: PropertyValue | None = None
    bulk_modulus: PropertyValue | None = None
    debye_temperature: PropertyValue | None = None

    extra_properties: dict[str, PropertyValue] = Field(default_factory=dict)
    # Present whenever multiple candidates for a property were considered.
    fusion_audit: dict[str, PropertyFusionAudit] = Field(default_factory=dict)
    # Set by fusion when this record could not participate in identity-keyed
    # merging (no domain identity, or an identity missing a required
    # discriminator such as symmetry data).  The record is kept as its own
    # material; this field says WHY it stayed alone instead of dropping it or
    # silently grouping it with same-formula lookalikes.
    fusion_exclusion: str | None = None
    raw: dict = Field(default_factory=dict, exclude=True)


class ProviderQueryLog(BaseModel):
    """Full audit of a single provider interaction."""
    provider_id: str
    provider_name: str
    endpoint_url: str
    # The provider's own pre-dispatch description of the query it intended to
    # issue (Provider.describe_query) -- a logical/intended query, NOT a
    # captured wire transcript. Renamed from `query_sent`: that name claimed a
    # wire capture the value never was (computed before search(), never tied
    # to the transport). No migration shim for the rename: nothing in the app
    # calls SearchCache.flush_to_disk/load_from_disk today (only a test
    # round-trips them), so no persisted entries with the old field exist. If
    # disk persistence is ever wired up, old-field entries would fail
    # validation and be skipped as a cache miss.
    query_description: str

    started_at: float
    completed_at: float
    latency_ms: float

    status: Literal["success", "timeout", "http_error",
                    "parse_error", "circuit_open", "skipped",
                    # Refused by the hard-offline policy, NOT a provider fault.
                    # Distinct from "http_error" because it says nothing about
                    # the provider's health and must not be treated as evidence
                    # about it — see `engine.py`'s failure branch.
                    "offline_blocked"]
    # The REAL wire status when one is known: the last page's status on
    # success (providers that report it, e.g. OPTIMADE), the failing
    # response's status on an HTTP error. None means no HTTP status existed
    # or the provider did not report one — never a fabricated 200.
    http_status_code: int | None = None
    result_count: int = 0

    error_type: str | None = None
    error_message: str | None = None
    # The verbatim error, untruncated by the one-line sanitizer (capped only
    # to keep logs bounded). `error_message` is for a glance; this is for
    # diagnosis.
    error_raw: str | None = None

    # How many pages the provider actually walked for this query (0 when the
    # query failed before any page landed).
    pages_fetched: int = 1
    # True when the provider had more matching data but returned fewer than
    # requested — a partial result, distinct from success, never silently
    # folded into it.
    truncated: bool = False
    # The provider's own total of matching records, when it reports one
    # (OPTIMADE meta.data_returned). `result_count < available` means this
    # answer is a slice of what exists.
    available: int | None = None


class ProviderPage(BaseModel):
    """Materials plus fetch accounting from one provider query.

    A provider that pages (or that knows more than "here is a list") returns
    this instead of a bare ``list[Material]`` so the engine can log
    ``pages_fetched``/``truncated``/``available`` from measured values
    instead of leaving them declared-and-unwritten. Providers returning a
    plain list keep working; their accounting fields fall back to the
    single-page defaults.
    """

    materials: list[Material]
    pages_fetched: int = 1
    truncated: bool = False
    available: int | None = None
    http_status_code: int | None = None
    # Why the result is partial, when it is (mid-pagination failure, page
    # safety cap). Verbatim, for the query log.
    note: str | None = None


class SearchResult(BaseModel):
    """Everything that comes back from a search."""
    materials: list[Material]
    total_count: int
    query: MaterialSearchQuery

    query_log: list[ProviderQueryLog]

    warnings: list[str] = Field(default_factory=list)
    cached: bool = False
    search_time_ms: float = 0
    # PARTIAL IS A THIRD STATE: False whenever any consulted provider failed,
    # timed out, was skipped by an open circuit, was refused by the offline
    # policy, or returned a truncated page — the answer may be less than what
    # the federation holds. Cached with a short TTL so one blip cannot pin a
    # 3-of-42 result for a day.
    complete: bool = True
    # S7: which filters were applied server-side vs client-side. OPTIMADE
    # providers can't filter on property ranges server-side, so the engine
    # post-filters locally and reports it here so the agent can cite honestly
    # that e.g. a band_gap constraint was enforced client-side, not by the DB.
    coverage: dict = Field(default_factory=dict)
