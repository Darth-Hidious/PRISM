"""Coverage-aware resolution for separately licensed scientific sources.

Licensed bytes and credentials never belong in this repository. Local entries
only point at files the operator already owns; remote entries come from an
authenticated platform response, where entitlement is decided server-side.
The resolver order is deliberately fixed: local file, entitled platform
source, refusal.

Local configuration lives at ``~/.prism/licensed_sources.json``::

    {"sources": [{
      "id": "owned-hea-tdb",
      "type": "thermodynamic_database",
      "name": "Owned HEA database",
      "version": "2026.1",
      "licence": "Commercial licence",
      "evidence_class": "screening",
      "path": "/outside/the/repo/owned.tdb",
      "coverage": {
        "elements": ["Mo", "Nb", "Ta", "W", "Hf"],
        "systems": [["Mo", "Nb", "Ta", "W", "Hf"]]
      }
    }]}

Element presence and validated-system coverage are separate on purpose: a TDB
containing five element symbols is not necessarily assessed for their
five-component system.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from enum import StrEnum
from pathlib import Path
from typing import Any, Mapping, Protocol, Sequence

from app.tools.evidence import EvidenceClass, coerce_evidence_class

DEFAULT_CONFIG_PATH = Path.home() / ".prism" / "licensed_sources.json"
# Read-only access endpoint. Its server implementation must resolve against the
# existing marketplace project_resources grants used by ontology purchases;
# this client has no second entitlement store or grant-creation path.
PLATFORM_RESOLVE_PATH = "/marketplace/licensed-sources/resolve"


class SourceType(StrEnum):
    THERMODYNAMIC_DATABASE = "thermodynamic_database"
    PROPERTY_DATASET = "property_dataset"
    INSTRUMENT_DATA = "instrument_data"


class SourceOrigin(StrEnum):
    LOCAL_FILE = "local_file"
    ENTITLED_REMOTE = "entitled_remote"


def _element(value: object) -> str:
    text = str(value).strip()
    if not text:
        raise ValueError("element names must not be empty")
    return text[:1].upper() + text[1:].lower()


def _unique(values: Sequence[object], normalizer) -> tuple[str, ...]:
    result: list[str] = []
    seen: set[str] = set()
    for value in values:
        normalized = normalizer(value)
        key = normalized.casefold()
        if key not in seen:
            seen.add(key)
            result.append(normalized)
    return tuple(result)


def _sequence(value: object, field_name: str) -> Sequence[object]:
    if value is None:
        return ()
    if isinstance(value, (str, bytes)) or not isinstance(value, Sequence):
        raise ValueError(f"{field_name} must be an array")
    return value


def _system_name(system: Sequence[str]) -> str:
    return "-".join(system)


@dataclass(frozen=True)
class SourceCoverage:
    elements: tuple[str, ...] = ()
    systems: tuple[tuple[str, ...], ...] = ()
    property_domains: tuple[str, ...] = ()

    @classmethod
    def from_mapping(cls, raw: Mapping[str, object] | None) -> "SourceCoverage":
        raw = raw or {}
        systems = tuple(
            _unique(_sequence(system, "coverage.systems[]"), _element)
            for system in _sequence(raw.get("systems"), "coverage.systems")
        )
        elements = _unique(
            _sequence(raw.get("elements"), "coverage.elements"), _element
        )
        if not elements and systems:
            elements = _unique(
                [element for system in systems for element in system], _element
            )
        domains = _unique(
            _sequence(raw.get("property_domains"), "coverage.property_domains"),
            lambda value: str(value).strip().lower(),
        )
        if any(not domain for domain in domains):
            raise ValueError("property domains must not be empty")
        return cls(elements=elements, systems=systems, property_domains=domains)

    def as_dict(self) -> dict[str, object]:
        return {
            "elements": list(self.elements),
            "systems": [list(system) for system in self.systems],
            "property_domains": list(self.property_domains),
        }

    def gaps_for(self, request: "SourceRequest") -> "CoverageGap":
        available_elements = {item.casefold() for item in self.elements}
        missing_elements = tuple(
            item for item in request.elements if item.casefold() not in available_elements
        )

        declared_systems = [
            {item.casefold() for item in system} for system in self.systems
        ]
        uncovered_systems = tuple(
            system
            for system in request.systems
            if not any(
                {item.casefold() for item in system} <= declared
                for declared in declared_systems
            )
        )

        available_domains = {item.casefold() for item in self.property_domains}
        missing_domains = tuple(
            item
            for item in request.property_domains
            if item.casefold() not in available_domains
        )
        return CoverageGap(
            missing_elements=missing_elements,
            uncovered_systems=uncovered_systems,
            missing_property_domains=missing_domains,
        )


@dataclass(frozen=True)
class SourceRequest:
    source_type: SourceType
    elements: tuple[str, ...] = ()
    systems: tuple[tuple[str, ...], ...] = ()
    property_domains: tuple[str, ...] = ()
    preferred_source: str | None = None

    @classmethod
    def thermodynamic_database(
        cls, components: Sequence[object], preferred_source: str | None = None
    ) -> "SourceRequest":
        elements = _unique(
            [item for item in components if str(item).strip().upper() != "VA"],
            _element,
        )
        return cls(
            source_type=SourceType.THERMODYNAMIC_DATABASE,
            elements=elements,
            systems=(elements,) if elements else (),
            preferred_source=preferred_source,
        )

    def coverage_dict(self) -> dict[str, object]:
        return {
            "elements": list(self.elements),
            "systems": [list(system) for system in self.systems],
            "property_domains": list(self.property_domains),
        }

    def platform_payload(self) -> dict[str, object]:
        payload: dict[str, object] = {
            "source_type": self.source_type.value,
            "required_coverage": self.coverage_dict(),
        }
        if self.preferred_source:
            payload["preferred_source"] = self.preferred_source
        return payload

    @property
    def coverage_name(self) -> str:
        if self.systems:
            return ", ".join(_system_name(system) for system in self.systems)
        if self.property_domains:
            return ", ".join(self.property_domains)
        return ", ".join(self.elements) or "the requested domain"


@dataclass(frozen=True)
class CoverageGap:
    missing_elements: tuple[str, ...] = ()
    uncovered_systems: tuple[tuple[str, ...], ...] = ()
    missing_property_domains: tuple[str, ...] = ()

    @property
    def covered(self) -> bool:
        return not (
            self.missing_elements
            or self.uncovered_systems
            or self.missing_property_domains
        )

    def as_dict(self) -> dict[str, object]:
        return {
            "missing_elements": list(self.missing_elements),
            "uncovered_systems": [
                _system_name(system) for system in self.uncovered_systems
            ],
            "missing_property_domains": list(self.missing_property_domains),
        }


@dataclass(frozen=True)
class LicensedSource:
    source_id: str
    source_type: SourceType
    name: str
    version: str
    licence: str
    evidence_class: EvidenceClass
    coverage: SourceCoverage
    origin: SourceOrigin
    access_kind: str
    path: Path | None = None
    access_reference: str | None = None

    def matches(self, preferred_source: str | None) -> bool:
        if not preferred_source:
            return True
        wanted = preferred_source.casefold()
        values = {self.source_id.casefold(), self.name.casefold()}
        if self.path:
            values.update({self.path.name.casefold(), self.path.stem.casefold()})
        return wanted in values

    def provenance(self) -> dict[str, object]:
        metadata: dict[str, object] = {
            "source_id": self.source_id,
            "source_type": self.source_type.value,
            "source_name": self.name,
            "version": self.version,
            "licence": self.licence,
            "origin": self.origin.value,
            "evidence_class": self.evidence_class.value,
            "coverage": self.coverage.as_dict(),
        }
        if self.source_type == SourceType.THERMODYNAMIC_DATABASE:
            metadata["database"] = self.name
        return metadata


@dataclass(frozen=True)
class ProviderResponse:
    sources: tuple[LicensedSource, ...] = ()
    errors: tuple[str, ...] = ()


class LicensedSourceProvider(Protocol):
    def find(self, request: SourceRequest) -> ProviderResponse: ...


def _required_text(raw: Mapping[str, object], *keys: str) -> str:
    for key in keys:
        value = raw.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    raise ValueError(f"missing required field: {' or '.join(keys)}")


class LocalFileSourceProvider:
    """Read metadata only; licensed file bytes remain at their configured path."""

    def __init__(self, config_path: Path | None = None):
        self.config_path = config_path or DEFAULT_CONFIG_PATH

    def find(self, request: SourceRequest) -> ProviderResponse:
        if not self.config_path.exists():
            return ProviderResponse()
        try:
            data = json.loads(self.config_path.read_text())
            rows = _sequence(data.get("sources"), "sources")
        except Exception as exc:
            return ProviderResponse(errors=(f"invalid {self.config_path}: {exc}",))

        sources: list[LicensedSource] = []
        errors: list[str] = []
        for index, raw in enumerate(rows):
            try:
                if not isinstance(raw, Mapping):
                    raise ValueError("source entry must be an object")
                source_type = SourceType(_required_text(raw, "type", "source_type"))
                if source_type != request.source_type:
                    continue
                path = Path(_required_text(raw, "path")).expanduser()
                if not path.is_file():
                    raise ValueError(f"configured file does not exist: {path}")
                if source_type == SourceType.THERMODYNAMIC_DATABASE:
                    if path.suffix.lower() != ".tdb":
                        raise ValueError(f"thermodynamic database is not a .tdb file: {path}")
                sources.append(
                    LicensedSource(
                        source_id=_required_text(raw, "id", "source_id"),
                        source_type=source_type,
                        name=_required_text(raw, "name"),
                        version=_required_text(raw, "version"),
                        licence=_required_text(raw, "licence", "license"),
                        evidence_class=coerce_evidence_class(raw.get("evidence_class")),
                        coverage=SourceCoverage.from_mapping(
                            raw.get("coverage")
                            if isinstance(raw.get("coverage"), Mapping)
                            else None
                        ),
                        origin=SourceOrigin.LOCAL_FILE,
                        access_kind="file",
                        path=path,
                    )
                )
            except Exception as exc:
                errors.append(f"{self.config_path} source[{index}]: {exc}")
        return ProviderResponse(tuple(sources), tuple(errors))


class PlatformSourceProvider:
    """Resolve existing marketplace grants; never submit an entitlement claim."""

    def __init__(self, client: Any | None = None):
        if client is None:
            from app.tools._platform_client import platform

            client = platform()
        self.client = client

    def find(self, request: SourceRequest) -> ProviderResponse:
        response = self.client.post(
            PLATFORM_RESOLVE_PATH,
            json=request.platform_payload(),
            timeout=15,
        )
        if not isinstance(response, Mapping):
            return ProviderResponse(errors=("platform returned an invalid source response",))
        if response.get("error"):
            return ProviderResponse(errors=(str(response["error"]),))

        raw_sources: object = response.get("sources")
        if raw_sources is None and response.get("source") is not None:
            raw_sources = [response["source"]]
        try:
            rows = _sequence(raw_sources, "platform sources")
        except ValueError as exc:
            return ProviderResponse(errors=(str(exc),))

        sources: list[LicensedSource] = []
        errors: list[str] = []
        for index, raw in enumerate(rows):
            try:
                if not isinstance(raw, Mapping):
                    raise ValueError("source entry must be an object")
                # This value comes from the authenticated server response. The
                # request body has no corresponding field a client can forge.
                if raw.get("entitled") is not True:
                    raise ValueError("platform did not confirm entitlement")
                source_type = SourceType(_required_text(raw, "type", "source_type"))
                if source_type != request.source_type:
                    continue
                access = raw.get("access")
                if not isinstance(access, Mapping):
                    access = {}
                path_value = access.get("path")
                path = (
                    Path(path_value).expanduser()
                    if isinstance(path_value, str) and path_value
                    else None
                )
                reference = access.get("reference")
                sources.append(
                    LicensedSource(
                        source_id=_required_text(raw, "id", "source_id"),
                        source_type=source_type,
                        name=_required_text(raw, "name"),
                        version=_required_text(raw, "version"),
                        licence=_required_text(raw, "licence", "license"),
                        evidence_class=coerce_evidence_class(raw.get("evidence_class")),
                        coverage=SourceCoverage.from_mapping(
                            raw.get("coverage")
                            if isinstance(raw.get("coverage"), Mapping)
                            else None
                        ),
                        origin=SourceOrigin.ENTITLED_REMOTE,
                        access_kind=str(access.get("kind") or "platform_reference"),
                        path=path,
                        access_reference=(
                            str(reference) if reference is not None else None
                        ),
                    )
                )
            except Exception as exc:
                errors.append(f"platform source[{index}]: {exc}")
        return ProviderResponse(tuple(sources), tuple(errors))


@dataclass(frozen=True)
class SourceRefusal:
    request: SourceRequest
    checked: tuple[tuple[LicensedSource, CoverageGap], ...] = ()
    provider_errors: tuple[str, ...] = ()

    def as_dict(self) -> dict[str, object]:
        label = {
            SourceType.THERMODYNAMIC_DATABASE: "TDB",
            SourceType.PROPERTY_DATASET: "property dataset",
            SourceType.INSTRUMENT_DATA: "instrument data source",
        }[self.request.source_type]
        needed = self.request.coverage_name
        preferred = (
            f" Requested source: {self.request.preferred_source}."
            if self.request.preferred_source
            else ""
        )
        return {
            "status": "refused",
            "error": f"No validated {label} covers {needed}",
            "refusal": {
                "code": "licensed_source_coverage_gap",
                "source_type": self.request.source_type.value,
                "required_coverage": self.request.coverage_dict(),
                "coverage_gap": {
                    "elements": list(self.request.elements),
                    "systems": [
                        _system_name(system) for system in self.request.systems
                    ],
                    "property_domains": list(self.request.property_domains),
                },
                "preferred_source": self.request.preferred_source,
                "sources_checked": [
                    {
                        "source_id": source.source_id,
                        "origin": source.origin.value,
                        "gap": gap.as_dict(),
                    }
                    for source, gap in self.checked
                ],
                "provider_errors": list(self.provider_errors),
            },
            "install_hint": (
                f"Configure an owned {label} in ~/.prism/licensed_sources.json "
                f"with declared validated coverage for {needed}, or acquire and "
                f"enable an entitled platform source with that coverage.{preferred}"
            ),
        }


@dataclass
class LicensedSourceResolver:
    local_provider: LicensedSourceProvider = field(
        default_factory=LocalFileSourceProvider
    )
    remote_provider: LicensedSourceProvider = field(
        default_factory=PlatformSourceProvider
    )

    def resolve(self, request: SourceRequest) -> LicensedSource | SourceRefusal:
        checked: list[tuple[LicensedSource, CoverageGap]] = []
        errors: list[str] = []

        local = self.local_provider.find(request)
        errors.extend(local.errors)
        source = self._covering(local.sources, request, checked)
        if source is not None:
            return source

        remote = self.remote_provider.find(request)
        errors.extend(remote.errors)
        source = self._covering(remote.sources, request, checked)
        if source is not None:
            return source

        return SourceRefusal(request, tuple(checked), tuple(errors))

    @staticmethod
    def _covering(
        sources: Sequence[LicensedSource],
        request: SourceRequest,
        checked: list[tuple[LicensedSource, CoverageGap]],
    ) -> LicensedSource | None:
        for source in sources:
            if not source.matches(request.preferred_source):
                continue
            gap = source.coverage.gaps_for(request)
            checked.append((source, gap))
            if gap.covered:
                return source
        return None


_RESOLVER: LicensedSourceResolver | None = None


def get_licensed_source_resolver() -> LicensedSourceResolver:
    global _RESOLVER
    if _RESOLVER is None:
        _RESOLVER = LicensedSourceResolver()
    return _RESOLVER
