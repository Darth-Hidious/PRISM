"""Polymer evaluator contract: every documented identity, and an honest gate.

`app/tools/tests/test_polymer_flexibility.py` covers the descriptor's ranking
behaviour, but it lives outside `testpaths` (pyproject: `testpaths = ["tests"]`)
so it only runs when named explicitly. These two regressions guard rules that
must not silently rot, so they live where the default run collects them.
"""

import builtins
import json
from unittest.mock import patch

import pytest

from app.tools import _extras
from app.tools.materials.polymer.tools import evaluate_polymer_insulation


def _flex(payload: dict) -> dict:
    return evaluate_polymer_insulation(json.dumps(payload))["property_status"][
        "rotatable_bond_fraction"
    ]


def test_smiles_representation_is_measured_not_declared_structureless():
    """`smiles` is one of the three representations `_load_identity` accepts and
    validates with RDKit. The flexibility lookup read only `repeat_unit_smiles`
    and `monomer_smiles`, so that path answered "no structure exists to measure
    flexibility on" about a structure the tool had just parsed — the same
    molecule scored 0.6 when handed over under a different key.
    """
    pytest.importorskip("rdkit.Chem")

    via_smiles = _flex({"representation": "smiles", "smiles": "CCCCCC"})
    via_repeat_unit = _flex(
        {
            "representation": "repeat_unit",
            "repeat_unit": "hexane-like",
            "repeat_unit_smiles": "CCCCCC",
        }
    )

    assert via_smiles["status"] == "computed", via_smiles.get("reason")
    assert via_smiles["value"] == pytest.approx(via_repeat_unit["value"])
    assert via_smiles["value"] > 0.0


def test_repeat_unit_still_wins_over_a_stray_smiles_key():
    """The repeat unit governs backbone flexibility, so it keeps priority; a
    `smiles` key on a repeat-unit candidate is not validated by
    `_load_identity` and must not displace it.
    """
    pytest.importorskip("rdkit.Chem")

    both = _flex(
        {
            "representation": "repeat_unit",
            "repeat_unit": "PET-like",
            "repeat_unit_smiles": "COC(=O)c1ccc(cc1)C(=O)OCCO",
            "smiles": "CCCCCC",
        }
    )
    repeat_only = _flex(
        {
            "representation": "repeat_unit",
            "repeat_unit": "PET-like",
            "repeat_unit_smiles": "COC(=O)c1ccc(cc1)C(=O)OCCO",
        }
    )

    assert both["status"] == "computed"
    assert both["value"] == pytest.approx(repeat_only["value"])


def test_missing_rdkit_returns_the_install_hint_instead_of_raising():
    """A dependency-gated tool RETURNS the one `_extras` shape, never raises.

    The RDKit gate raised `RuntimeError(RDKIT_INSTALL_HINT)`, so the caller got
    an exception with no `requires_extra` to branch on and no `install_hint`
    key at all — while the LPBF and precipitation gates in the same tree
    already returned the structured dict. Simulated so this runs identically
    with and without RDKit installed.
    """
    real_import = builtins.__import__

    def import_without_rdkit(name, *args, **kwargs):
        if name == "rdkit" or name.startswith("rdkit."):
            raise ImportError("simulated missing rdkit")
        return real_import(name, *args, **kwargs)

    payload = json.dumps({"representation": "smiles", "smiles": "CC"})
    with patch("builtins.__import__", side_effect=import_without_rdkit):
        result = evaluate_polymer_insulation(payload)  # must not raise

    assert result["requires_extra"] == "polymer"
    assert result["install_hint"] == _extras.install_command("polymer")
    assert "prism-platform[" not in result["install_hint"]
    assert "RDKit" in result["error"]
    # No property may be reported when nothing could be evaluated.
    assert "property_status" not in result
