"""Shared four-level evidence classification and propagation.

The serialized values align with RHEA-JAX ``ClaimStatus``. Color names are
presentation metadata, not a second classification vocabulary:

- ``reference_validated`` / GREEN: executed or physically measured;
- ``screening`` / YELLOW: computed by a cited method;
- ``research`` / ORANGE: extracted from literature, unverified;
- ``indeterminate`` / RED: ungrounded model assertion.

A producer has a maximum class, and a result is capped by its worst input.
Consequently confidence, agreement, and source count can never manufacture
GREEN evidence.
"""

from __future__ import annotations

from enum import StrEnum
from typing import Iterable, MutableMapping


class EvidenceClass(StrEnum):
    INDETERMINATE = "indeterminate"
    RESEARCH = "research"
    SCREENING = "screening"
    REFERENCE_VALIDATED = "reference_validated"

    @property
    def color(self) -> str:
        return _COLORS[self]


class EvidenceSource(StrEnum):
    EXECUTION = "execution"
    CITED_COMPUTATION = "cited_computation"
    LITERATURE_EXTRACTION = "literature_extraction"
    MODEL_ASSERTION = "model_assertion"


_RANK = {
    EvidenceClass.INDETERMINATE: 0,
    EvidenceClass.RESEARCH: 1,
    EvidenceClass.SCREENING: 2,
    EvidenceClass.REFERENCE_VALIDATED: 3,
}

_COLORS = {
    EvidenceClass.INDETERMINATE: "red",
    EvidenceClass.RESEARCH: "orange",
    EvidenceClass.SCREENING: "yellow",
    EvidenceClass.REFERENCE_VALIDATED: "green",
}

_SOURCE_CEILING = {
    EvidenceSource.EXECUTION: EvidenceClass.REFERENCE_VALIDATED,
    EvidenceSource.CITED_COMPUTATION: EvidenceClass.SCREENING,
    EvidenceSource.LITERATURE_EXTRACTION: EvidenceClass.RESEARCH,
    EvidenceSource.MODEL_ASSERTION: EvidenceClass.INDETERMINATE,
}


def coerce_evidence_class(value: EvidenceClass | str) -> EvidenceClass:
    """Parse the stable RHEA-aligned value; reject unknown fifth vocabularies."""
    if isinstance(value, EvidenceClass):
        return value
    try:
        return EvidenceClass(value)
    except (TypeError, ValueError) as exc:
        allowed = ", ".join(item.value for item in EvidenceClass)
        raise ValueError(f"evidence_class must be one of: {allowed}") from exc


def evidence_for_result(
    source: EvidenceSource | str,
    inputs: Iterable[EvidenceClass | str],
) -> EvidenceClass:
    """Return the producer ceiling capped by the worst input class."""
    try:
        source = EvidenceSource(source)
    except (TypeError, ValueError) as exc:
        allowed = ", ".join(item.value for item in EvidenceSource)
        raise ValueError(f"evidence source must be one of: {allowed}") from exc

    result = _SOURCE_CEILING[source]
    for input_class in inputs:
        candidate = coerce_evidence_class(input_class)
        if _RANK[candidate] < _RANK[result]:
            result = candidate
    return result


def stamp_evidence(
    result: MutableMapping[str, object],
    source: EvidenceSource | str,
    inputs: Iterable[EvidenceClass | str] = (),
) -> EvidenceClass:
    """Attach the machine class and its required color rendering to a result."""
    evidence_class = evidence_for_result(source, inputs)
    result["evidence_class"] = evidence_class.value
    result["evidence_color"] = evidence_class.color
    return evidence_class
