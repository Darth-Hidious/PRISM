# Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
"""Cited, deliberately narrow polymer electrical-insulation properties.

The only implemented property method is the Fox-Flory molecular-weight
relation for glass transition temperature::

    Tg = Tg_infinity - K / Mn

It is evaluated only when the candidate supplies positive polymer-specific
``Tg_infinity`` and ``K`` parameters plus a non-empty citation for those
parameters. PRISM does not infer universal constants from a repeat-unit name.

Reference for the relation:
T. G. Fox and P. J. Flory, "Second-Order Transition Temperatures and Related
Properties of Polystyrene. I. Influence of Molecular Weight," Journal of
Applied Physics 21 (1950) 581-591, DOI 10.1063/1.1699711.

No audited van Krevelen/Bicerano group-parameter table and assignment engine
is present in this repository, so this module does not claim a group-
contribution Tg estimate. No model is implemented for dielectric constant,
dielectric breakdown strength, or thermal conductivity. Those fields are
returned as explicitly unavailable, with no numeric value. This is
intentional: an unvalidated high-voltage insulation estimate is worse than an
absent estimate.

A small executable example grounds the one equation without needing RDKit::

    >>> fox_flory_tg_k(50000.0, 450.0, 100000.0)
    448.0
"""
from __future__ import annotations

import json
import math
from typing import Any

from app.tools.base import Tool, ToolRegistry
from app.tools.evidence import (
    EvidenceClass,
    EvidenceSource,
    coerce_evidence_class,
    stamp_evidence,
)
from app.tools.materials.polymer import RDKIT_INSTALL_HINT

FOX_FLORY_CITATION = (
    "T. G. Fox and P. J. Flory, Journal of Applied Physics 21 (1950) "
    "581-591, DOI 10.1063/1.1699711"
)

def fox_flory_tg_k(
    number_average_molar_mass_g_per_mol: float,
    tg_infinity_k: float,
    k_k_g_per_mol: float,
) -> float:
    """Calculate Tg from sourced Fox-Flory inputs; never fit or infer them."""
    values = (
        number_average_molar_mass_g_per_mol,
        tg_infinity_k,
        k_k_g_per_mol,
    )
    if any(isinstance(value, bool) for value in values):
        raise ValueError("Fox-Flory inputs must be numeric, not boolean")
    try:
        mn, tg_inf, constant = (float(value) for value in values)
    except (TypeError, ValueError) as exc:
        raise ValueError("Fox-Flory inputs must be numeric") from exc
    checked = (mn, tg_inf, constant)
    if not all(math.isfinite(value) and value > 0.0 for value in checked):
        raise ValueError("Fox-Flory inputs must be finite and positive")
    tg = tg_inf - constant / mn
    if not math.isfinite(tg) or tg <= 0.0:
        raise ValueError("Fox-Flory inputs produce a non-physical Tg <= 0 K")
    return tg


def _load_identity(candidate_identity: str, chem: Any) -> dict[str, Any]:
    try:
        candidate = json.loads(candidate_identity)
    except (TypeError, json.JSONDecodeError) as exc:
        raise ValueError("candidate_identity must be a JSON object string") from exc
    if not isinstance(candidate, dict):
        raise ValueError("candidate_identity must decode to a JSON object")

    representation = candidate.get("representation")
    if representation == "repeat_unit":
        repeat_unit = candidate.get("repeat_unit")
        if not isinstance(repeat_unit, str) or not repeat_unit.strip():
            raise ValueError("repeat_unit representation needs a non-empty repeat_unit")
        repeat_unit_smiles = candidate.get("repeat_unit_smiles")
        if repeat_unit_smiles is not None:
            if not isinstance(repeat_unit_smiles, str) or chem.MolFromSmiles(
                repeat_unit_smiles
            ) is None:
                raise ValueError("repeat_unit_smiles is not valid RDKit SMILES")
    elif representation == "monomer":
        monomer = candidate.get("monomer")
        if not isinstance(monomer, str) or not monomer.strip():
            raise ValueError("monomer representation needs a non-empty monomer")
        monomer_smiles = candidate.get("monomer_smiles")
        if monomer_smiles is not None:
            if not isinstance(monomer_smiles, str) or chem.MolFromSmiles(
                monomer_smiles
            ) is None:
                raise ValueError("monomer_smiles is not valid RDKit SMILES")
    elif representation == "smiles":
        smiles = candidate.get("smiles")
        if not isinstance(smiles, str) or not smiles.strip():
            raise ValueError("SMILES representation needs a non-empty smiles string")
        if chem.MolFromSmiles(smiles) is None:
            raise ValueError("candidate smiles is not valid RDKit SMILES")
    else:
        raise ValueError(
            "representation must be one of repeat_unit, monomer, or smiles"
        )
    return candidate


def _unavailable(reason: str) -> dict[str, str]:
    result = {"status": "unavailable", "reason": reason}
    stamp_evidence(result, EvidenceSource.MODEL_ASSERTION)
    return result


def evaluate_polymer_insulation(
    candidate_identity: str,
    tier: int = 0,
) -> dict[str, Any]:
    """Evaluate only properties supported by a cited method.

    RDKit is imported here as a second defensive gate. Normal bootstrap never
    registers this function unless the same import already succeeded.
    """
    if tier != 0:
        raise ValueError("polymer domain currently supports only evaluator tier 0")
    try:
        from rdkit import Chem
        import rdkit
    except Exception as exc:
        raise RuntimeError(RDKIT_INSTALL_HINT) from exc

    candidate = _load_identity(candidate_identity, Chem)
    input_evidence = coerce_evidence_class(
        candidate.get("evidence_class", EvidenceClass.INDETERMINATE)
    )
    candidate["evidence_class"] = input_evidence.value
    candidate["evidence_color"] = input_evidence.color
    status: dict[str, dict[str, Any]] = {}
    result: dict[str, Any] = {
        "candidate_identity": candidate,
        "evaluator_tier": 0,
        "rdkit_version": rdkit.__version__,
    }

    parameters = candidate.get("fox_flory")
    if parameters is None:
        status["glass_transition_temperature_k"] = _unavailable(
            "Fox-Flory Tg needs polymer-specific Mn, Tg_infinity, K, and a "
            "parameter citation; none were supplied. No group constants or "
            "fit parameters were inferred."
        )
    elif not isinstance(parameters, dict):
        raise ValueError("fox_flory must be an object")
    else:
        parameter_citation = parameters.get("parameter_citation")
        if not isinstance(parameter_citation, str) or not parameter_citation.strip():
            raise ValueError(
                "fox_flory.parameter_citation is required; unsourced fit "
                "parameters are not accepted"
            )
        tg = fox_flory_tg_k(
            parameters.get("number_average_molar_mass_g_per_mol"),
            parameters.get("tg_infinity_k"),
            parameters.get("k_k_g_per_mol"),
        )
        result["glass_transition_temperature_k"] = tg
        status["glass_transition_temperature_k"] = {
            "status": "computed",
            "value": tg,
            "unit": "QUDT:K",
            "method": "Fox-Flory molecular-weight relation: Tg = Tg_infinity - K/Mn",
            "citation": FOX_FLORY_CITATION,
            "parameter_citation": parameter_citation,
        }
        stamp_evidence(
            status["glass_transition_temperature_k"],
            EvidenceSource.CITED_COMPUTATION,
            [input_evidence],
        )

    status["dielectric_constant"] = _unavailable(
        "No citable dielectric-constant method is implemented for this "
        "candidate representation; measured data or a separately validated "
        "model is required."
    )
    status["dielectric_breakdown_strength_kv_per_mm"] = _unavailable(
        "No validated dielectric-breakdown-strength method is implemented. "
        "PRISM will not estimate a high-voltage safety property from identity alone."
    )
    status["thermal_conductivity_w_per_m_k"] = _unavailable(
        "No citable thermal-conductivity method is implemented for this "
        "candidate representation; morphology and measurement evidence are required."
    )
    result["property_status"] = status
    computed_classes = [
        item["evidence_class"]
        for item in status.values()
        if item["status"] == "computed"
    ]
    stamp_evidence(
        result,
        (
            EvidenceSource.CITED_COMPUTATION
            if computed_classes
            else EvidenceSource.MODEL_ASSERTION
        ),
        [input_evidence, *computed_classes],
    )
    return result


def create_polymer_tools(registry: ToolRegistry) -> None:
    """Register the evaluator; caller must gate with check_polymer_available."""
    registry.register(
        Tool(
            name="polymer_insulation_properties",
            description=(
                "Evaluate an RDKit-validated polymer identity for ABB "
                "electrical-insulation targets. Computes Tg only when sourced "
                "Fox-Flory parameters are supplied; reports dielectric constant, "
                "breakdown strength, and thermal conductivity unavailable."
            ),
            input_schema={
                "type": "object",
                "properties": {
                    "candidate_identity": {
                        "type": "string",
                        "description": (
                            "JSON object string with representation repeat_unit, "
                            "monomer, or smiles"
                        ),
                    },
                    "tier": {"type": "integer", "enum": [0], "default": 0},
                },
                "required": ["candidate_identity"],
                "additionalProperties": False,
            },
            output_schema={
                "type": "object",
                "properties": {
                    "glass_transition_temperature_k": {"type": "number"},
                    "property_status": {"type": "object"},
                },
                "required": ["property_status"],
            },
            units={
                "glass_transition_temperature_k": "K",
                "dielectric_breakdown_strength_kv_per_mm": "kV/mm",
                "thermal_conductivity_w_per_m_k": "W/(m K)",
            },
            func=evaluate_polymer_insulation,
            requires_approval=False,
        )
    )


__all__ = [
    "FOX_FLORY_CITATION",
    "create_polymer_tools",
    "evaluate_polymer_insulation",
    "fox_flory_tg_k",
]
