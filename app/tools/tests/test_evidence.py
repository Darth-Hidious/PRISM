"""Focused tests for evidence-class propagation in computed tool results."""

import json

from app.tools.evidence import (
    EvidenceClass,
    EvidenceSource,
    evidence_for_result,
    roll_up_evidence,
)


def test_executed_result_inherits_worst_input_class() -> None:
    result = evidence_for_result(
        EvidenceSource.EXECUTION,
        [EvidenceClass.REFERENCE_VALIDATED, EvidenceClass.RESEARCH],
    )
    assert result is EvidenceClass.RESEARCH


def test_only_execution_can_produce_green() -> None:
    assert evidence_for_result(EvidenceSource.EXECUTION, []) is EvidenceClass.REFERENCE_VALIDATED
    assert evidence_for_result(EvidenceSource.CITED_COMPUTATION, []) is EvidenceClass.SCREENING
    assert evidence_for_result(EvidenceSource.LITERATURE_EXTRACTION, []) is EvidenceClass.RESEARCH
    assert evidence_for_result(EvidenceSource.MODEL_ASSERTION, []) is EvidenceClass.INDETERMINATE


def test_agreement_cannot_upgrade_literature_extraction() -> None:
    result = evidence_for_result(
        EvidenceSource.LITERATURE_EXTRACTION,
        [EvidenceClass.REFERENCE_VALIDATED] * 10,
    )
    assert result is EvidenceClass.RESEARCH


def test_roll_up_uses_worst_reported_property_and_indeterminate_for_missing() -> None:
    result: dict[str, object] = {}
    evidence_class = roll_up_evidence(
        result,
        [
            {"value": 1.0, "evidence_class": "screening"},
            {"value": 2.0, "evidence_class": "research"},
        ],
    )
    assert evidence_class is EvidenceClass.RESEARCH
    assert result == {"evidence_class": "research", "evidence_color": "orange"}

    missing: dict[str, object] = {}
    assert roll_up_evidence(missing, [{"value": 3.0}]) is EvidenceClass.INDETERMINATE
    assert missing["evidence_class"] == "indeterminate"


def test_evaluator_result_inherits_orange_boundary_condition() -> None:
    from app.tools.evaluation import evaluate_candidate

    result = evaluate_candidate(
        {
            "composition": "W0.5Ta0.3Mo0.2",
            "evidence_class": EvidenceClass.RESEARCH.value,
        },
        tier=0,
    )
    assert result["tiers"]["0"]["properties"]["evidence_class"] == "research"
    assert result["tiers"]["0"]["evidence_class"] == "research"
    assert result["evidence_class"] == "research"


def test_polymer_computation_inherits_literature_input() -> None:
    from app.tools.materials.polymer.tools import evaluate_polymer_insulation

    candidate = json.dumps(
        {
            "representation": "smiles",
            "smiles": "CC",
            "evidence_class": "research",
            "fox_flory": {
                "number_average_molar_mass_g_per_mol": 50000.0,
                "tg_infinity_k": 450.0,
                "k_k_g_per_mol": 100000.0,
                "parameter_citation": "customer literature record",
            },
        }
    )
    result = evaluate_polymer_insulation(candidate)
    tg = result["property_status"]["glass_transition_temperature_k"]
    assert tg["value"] == 448.0
    assert tg["unit"] == "QUDT:K"
    assert tg["evidence_class"] == "research"
    assert result["evidence_class"] == "research"
    assert result["evidence_color"] == "orange"
    assert result["rdkit_version"] == "2026.03.5"
