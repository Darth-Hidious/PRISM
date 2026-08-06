"""Flexibility descriptor: it must compute, rank sensibly, and never overclaim."""
import json

import pytest

from app.tools.materials.polymer.tools import (
    ROTATABLE_FLEXIBILITY_DEFINITION,
    evaluate_polymer_insulation,
    rotatable_bond_fraction,
)

Chem = pytest.importorskip("rdkit.Chem")


def _flex(payload):
    return evaluate_polymer_insulation(json.dumps(payload))["property_status"][
        "rotatable_bond_fraction"
    ]


def test_ranks_rigid_below_flexible():
    """The one thing a ranking signal must do: order correctly."""
    benzene = rotatable_bond_fraction(Chem.MolFromSmiles("c1ccccc1"))
    hexane = rotatable_bond_fraction(Chem.MolFromSmiles("CCCCCC"))
    assert benzene == 0.0, "a fused aromatic ring has no rotatable bonds"
    assert hexane > benzene


def test_reaches_the_structure_via_the_validated_identity():
    """Regression: the first version read a `_mol` key that `_load_identity`
    never sets — it validates SMILES and discards the mol — so this branch
    could never fire. Assert it computes, not merely that it returns."""
    flex = _flex(
        {
            "representation": "repeat_unit",
            "repeat_unit": "PET-like",
            "repeat_unit_smiles": "COC(=O)c1ccc(cc1)C(=O)OCCO",
        }
    )
    assert flex["status"] == "computed"
    assert 0.0 < flex["value"] < 1.0
    assert flex["unit"] == "QUDT:UNITLESS"


def test_falls_back_to_monomer_when_no_repeat_unit():
    flex = _flex(
        {"representation": "monomer", "monomer": "hexane", "monomer_smiles": "CCCCCC"}
    )
    assert flex["status"] == "computed"


def test_absent_structure_is_unavailable_not_zero():
    """A missing structure must not silently score 0.0 — that would rank an
    unknown candidate as maximally rigid."""
    flex = _flex({"representation": "repeat_unit", "repeat_unit": "unknown"})
    assert flex["status"] == "unavailable"
    assert "value" not in flex


def test_carries_its_own_definition_and_claims_no_tg():
    """It must ship its definition, and must NOT be attributed to Kim et al.
    or produce a Tg — the SI gives neither the formula nor the fit."""
    flex = _flex(
        {
            "representation": "monomer",
            "monomer": "hexane",
            "monomer_smiles": "CCCCCC",
        }
    )
    assert flex["method"] == ROTATABLE_FLEXIBILITY_DEFINITION
    assert "NumRotatableBonds" in flex["method"]
    assert "Kim" not in json.dumps(flex)
