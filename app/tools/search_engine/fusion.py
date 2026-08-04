"""Cross-provider material fusion with auditable truth-discovery weights.

Source reliability is learned from agreement with a weighted consensus across
all overlapping claims in one fusion batch.  Extraction reliability is a
separate factor: a paper can be sound while an LLM transcription is wrong, or
a structured API response can be transcribed correctly while its source value
is wrong.  The iterative update follows the truth-discovery principle in Dong
et al., "Knowledge-Based Trust" (VLDB 2015, arXiv:1502.03519) and Li et al.,
"A Survey on Truth Discovery" (arXiv:1505.02463).
"""
from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass
import json
from typing import Iterable

from app.tools.search_engine.identity import fusion_key as identity_fusion_key
from app.tools.search_engine.result import (
    FusionCandidate,
    Material,
    PropertyFusionAudit,
    PropertyValue,
)

_STANDARD_PROPERTIES = (
    "space_group",
    "band_gap",
    "formation_energy",
    "energy_above_hull",
    "bulk_modulus",
    "debye_temperature",
    "lattice_vectors",
    "crystal_system",
)
_NEUTRAL_RELIABILITY = 0.5
_MAX_RELIABILITY_ITERATIONS = 12
_CONVERGENCE_TOLERANCE = 1e-9


@dataclass(frozen=True)
class _Claim:
    """A property assertion plus the two independent reliability dimensions."""

    property_name: str
    property_value: PropertyValue
    source: str
    extractor_id: str
    extractor_kind: str

    @property
    def extraction_key(self) -> tuple[str, str]:
        return (self.extractor_kind, self.extractor_id)

    @property
    def value_key(self) -> str:
        """Compare only values expressed in the same unit.

        Unit conversion is intentionally not guessed here; comparing 1 eV to
        1000 meV as if they differed would be wrong, but silently converting
        arbitrary provider units would be another unvalidated extractor.
        """
        try:
            return json.dumps(
                {"value": self.property_value.value, "unit": self.property_value.unit},
                sort_keys=True,
                separators=(",", ":"),
            )
        except (TypeError, ValueError) as exc:
            raise ValueError(
                f"cannot compare {self.property_name!r} from source {self.source!r}: "
                "value is not JSON-serializable"
            ) from exc


def _fusion_key(material: Material) -> str:
    """Return the explicitly supplied domain identity key for one material."""
    if material.identity is None:
        raise ValueError(
            f"material {material.id!r} has no domain-supplied identity and "
            "cannot be fused"
        )
    return identity_fusion_key(material.identity)


def _claim_source(material: Material, property_value: PropertyValue) -> str:
    source = property_value.source.strip()
    if source:
        return source
    if len(material.sources) == 1 and material.sources[0].strip():
        return material.sources[0].strip()
    raise ValueError(
        f"material {material.id!r} property has no attributable source; "
        "refusing to fuse untraceable evidence"
    )


def _claims_for_group(group: list[Material]) -> dict[str, list[_Claim]]:
    """Collect all standard and extra property observations in one identity group."""
    property_names = set(_STANDARD_PROPERTIES)
    property_names.update(
        name for material in group for name in material.extra_properties
    )
    claims_by_property: dict[str, list[_Claim]] = {}
    for property_name in sorted(property_names):
        claims: list[_Claim] = []
        for material in group:
            property_value = (
                getattr(material, property_name)
                if property_name in _STANDARD_PROPERTIES
                else material.extra_properties.get(property_name)
            )
            # A missing value is an abstention, not evidence for a null value.
            if property_value is None or property_value.value is None:
                continue
            extraction = property_value.extraction
            extractor_id = extraction.extractor_id.strip() or "unknown"
            claims.append(
                _Claim(
                    property_name=property_name,
                    property_value=property_value,
                    source=_claim_source(material, property_value),
                    extractor_id=extractor_id,
                    extractor_kind=extraction.kind,
                )
            )
        if claims:
            claims_by_property[property_name] = claims
    return claims_by_property


def _source_weight(source: str, source_reliability: dict[str, float]) -> float:
    """Return learned reliability or the documented neutral seed for no evidence."""
    return source_reliability.get(source, _NEUTRAL_RELIABILITY)


def _extraction_weight(
    claim: _Claim,
    extraction_reliability: dict[tuple[str, str], float],
) -> float:
    """Return learned extraction reliability or its documented initial seed."""
    default = 1.0 if claim.extractor_kind == "structured_api" else _NEUTRAL_RELIABILITY
    return extraction_reliability.get(claim.extraction_key, default)


def _combined_weight(
    claim: _Claim,
    source_reliability: dict[str, float],
    extraction_reliability: dict[tuple[str, str], float],
) -> float:
    return _source_weight(claim.source, source_reliability) * _extraction_weight(
        claim, extraction_reliability
    )


def _consensus_value_key(
    claims: Iterable[_Claim],
    source_reliability: dict[str, float],
    extraction_reliability: dict[tuple[str, str], float],
) -> str | None:
    """Return a unique weighted consensus, or None when evidence is tied."""
    weights: dict[str, float] = defaultdict(float)
    for claim in claims:
        weights[claim.value_key] += _combined_weight(
            claim, source_reliability, extraction_reliability
        )
    if not weights:
        return None
    highest = max(weights.values())
    winners = [
        value_key
        for value_key, weight in weights.items()
        if abs(weight - highest) <= _CONVERGENCE_TOLERANCE
    ]
    return winners[0] if len(winners) == 1 else None


def _estimate_reliability(
    claim_groups: list[list[_Claim]],
) -> tuple[dict[str, float], dict[tuple[str, str], float]]:
    """Estimate source and extractor reliability by iterative agreement.

    The source seed is a Beta(1, 1) mean of 0.5: a neutral pseudocount used
    only to make the first consensus computable, not an asserted source-quality
    prior.  Non-structured extractors use the same neutral seed and are updated
    separately from sources.  An explicitly marked ``structured_api`` extractor
    starts and stays at 1.0 because it is a direct machine-readable field
    mapping; this encodes the stated near-zero extraction-error assumption, not
    trust in the underlying database or paper.
    """
    all_claims = [claim for claims in claim_groups for claim in claims]
    source_reliability = {
        source: _NEUTRAL_RELIABILITY
        for source in {claim.source for claim in all_claims}
    }
    extraction_reliability = {
        extraction_key: (
            1.0 if extraction_key[0] == "structured_api" else _NEUTRAL_RELIABILITY
        )
        for extraction_key in {claim.extraction_key for claim in all_claims}
    }

    for _ in range(_MAX_RELIABILITY_ITERATIONS):
        source_counts: dict[str, list[int]] = {
            source: [0, 0] for source in source_reliability
        }
        extraction_counts: dict[tuple[str, str], list[int]] = {
            extraction_key: [0, 0] for extraction_key in extraction_reliability
        }
        for claims in claim_groups:
            consensus = _consensus_value_key(
                claims, source_reliability, extraction_reliability
            )
            # A tie is deliberately not treated as truth for either side.
            if consensus is None:
                continue
            for claim in claims:
                agrees = claim.value_key == consensus
                source_counts[claim.source][1] += 1
                source_counts[claim.source][0] += int(agrees)
                if claim.extractor_kind != "structured_api":
                    extraction_counts[claim.extraction_key][1] += 1
                    extraction_counts[claim.extraction_key][0] += int(agrees)

        next_source_reliability = dict(source_reliability)
        next_extraction_reliability = dict(extraction_reliability)
        for source, (agreements, observations) in source_counts.items():
            if observations:
                next_source_reliability[source] = (1 + agreements) / (2 + observations)
        for extraction_key, (agreements, observations) in extraction_counts.items():
            if observations:
                next_extraction_reliability[extraction_key] = (
                    (1 + agreements) / (2 + observations)
                )

        changed = max(
            [
                *(
                    abs(next_source_reliability[key] - source_reliability[key])
                    for key in source_reliability
                ),
                *(
                    abs(
                        next_extraction_reliability[key]
                        - extraction_reliability[key]
                    )
                    for key in extraction_reliability
                ),
            ],
            default=0.0,
        )
        source_reliability = next_source_reliability
        extraction_reliability = next_extraction_reliability
        if changed <= _CONVERGENCE_TOLERANCE:
            break
    return source_reliability, extraction_reliability


def _ordered_claims(claims: Iterable[_Claim]) -> list[_Claim]:
    """Provide deterministic audit ordering independent of provider arrival."""
    return sorted(
        claims,
        key=lambda claim: (
            claim.source,
            claim.extractor_kind,
            claim.extractor_id,
            claim.value_key,
        ),
    )


def _resolve_property(
    claims: list[_Claim],
    source_reliability: dict[str, float],
    extraction_reliability: dict[tuple[str, str], float],
) -> tuple[PropertyValue | None, PropertyFusionAudit, set[str]]:
    """Select a property value and retain every candidate in an audit record."""
    ordered = _ordered_claims(claims)
    consensus = _consensus_value_key(
        ordered, source_reliability, extraction_reliability
    )
    matching = [claim for claim in ordered if claim.value_key == consensus]
    selected_claim = (
        max(
            matching,
            key=lambda claim: (
                _combined_weight(claim, source_reliability, extraction_reliability),
                claim.source,
                claim.extractor_kind,
                claim.extractor_id,
            ),
        )
        if matching
        else None
    )
    candidates = [
        FusionCandidate(
            property_value=claim.property_value.model_copy(deep=True),
            source=claim.source,
            extractor_id=claim.extractor_id,
            source_reliability=_source_weight(claim.source, source_reliability),
            extraction_reliability=_extraction_weight(
                claim, extraction_reliability
            ),
            combined_weight=_combined_weight(
                claim, source_reliability, extraction_reliability
            ),
            selected=claim.value_key == consensus,
        )
        for claim in ordered
    ]
    return (
        selected_claim.property_value.model_copy(deep=True)
        if selected_claim is not None
        else None,
        PropertyFusionAudit(
            resolved=selected_claim is not None,
            selected_source=(
                selected_claim.source if selected_claim is not None else None
            ),
            candidates=candidates,
        ),
        {claim.value_key for claim in matching},
    )


def _conflict_extra_key(
    extra_properties: dict[str, PropertyValue],
    property_name: str,
    claim: _Claim,
) -> str:
    """Make a readable, non-overwriting key for a retained losing candidate."""
    key = f"{property_name}:{claim.source}"
    suffix = 2
    while key in extra_properties:
        key = f"{property_name}:{claim.source}:{suffix}"
        suffix += 1
    return key


def _merge_group(
    group: list[Material],
    source_reliability: dict[str, float],
    extraction_reliability: dict[tuple[str, str], float],
) -> Material:
    """Merge one known-identity group without letting input order choose a value."""
    base = min(
        group,
        key=lambda material: (material.id, tuple(sorted(material.sources))),
    )
    fused = base.model_copy(deep=True)
    fused.sources = sorted(
        {source for material in group for source in material.sources}
    )
    fused.extra_properties = {}
    fused.fusion_audit = {}
    for property_name in _STANDARD_PROPERTIES:
        setattr(fused, property_name, None)

    for property_name, claims in _claims_for_group(group).items():
        selected_value, audit, selected_value_keys = _resolve_property(
            claims, source_reliability, extraction_reliability
        )
        fused.fusion_audit[property_name] = audit
        if property_name in _STANDARD_PROPERTIES:
            setattr(fused, property_name, selected_value)
        elif selected_value is not None:
            fused.extra_properties[property_name] = selected_value

        # A resolved value remains in its normal slot.  Every competing value
        # remains visible in both the audit record and extra_properties.  A tie
        # has no primary value, so all candidates are retained as extras.
        for claim in _ordered_claims(claims):
            if selected_value_keys and claim.value_key in selected_value_keys:
                continue
            key = _conflict_extra_key(fused.extra_properties, property_name, claim)
            fused.extra_properties[key] = claim.property_value.model_copy(deep=True)
    return fused


def fuse_materials(materials: list[Material]) -> list[Material]:
    """Fuse known domain identities using reliability, never provider arrival.

    Legacy records without a domain-supplied identity are returned as isolated
    records.  They are not assigned a guessed crystal key, so they cannot
    silently collide.  An explicitly supplied but unregistered domain raises a
    ``ValueError`` instead of falling back to a crystal formula key.
    """
    if not materials:
        return []

    groups: dict[str, list[Material]] = defaultdict(list)
    unfused_legacy: list[Material] = []
    for material in materials:
        if material.identity is None:
            unfused_legacy.append(material)
            continue
        groups[_fusion_key(material)].append(material)

    claim_groups = [
        claims
        for group in groups.values()
        if len(group) > 1
        for property_name, claims in _claims_for_group(group).items()
        # The group is already keyed on space group.  Counting agreement on
        # that identity component as evidence would circularly inflate trust.
        if property_name != "space_group"
        if len({claim.source for claim in claims}) > 1
    ]
    source_reliability, extraction_reliability = _estimate_reliability(claim_groups)

    fused = [
        _merge_group(group, source_reliability, extraction_reliability)
        if len(group) > 1
        else group[0]
        for group in groups.values()
    ]
    return fused + unfused_legacy
