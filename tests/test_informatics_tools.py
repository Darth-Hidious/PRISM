"""Tests for the informatics tools (E7-E12)."""
from unittest.mock import patch, MagicMock

import pytest


def test_all_informatics_tools_register():
    from app.tools.base import ToolRegistry
    from app.tools.materials.informatics import create_informatics_tools

    reg = ToolRegistry()
    create_informatics_tools(reg)
    names = {t.name for t in reg.list_tools()}
    for n in ("structure_similarity", "compute_descriptor", "predict_property",
              "pareto_screen", "suggest_next_experiments"):
        assert n in names, f"{n} must register"


def test_structure_desc_tools_register():
    from app.tools.base import ToolRegistry
    from app.tools.materials.structure_desc import create_structure_desc_tools

    reg = ToolRegistry()
    create_structure_desc_tools(reg)
    names = {t.name for t in reg.list_tools()}
    assert "describe_structure" in names
    assert "predict_synthesizability" in names


# ---- E10: pareto_screen (deterministic, no deps) ----

def test_pareto_front_simple():
    """A known 2-objective Pareto set."""
    from app.tools.base import ToolRegistry
    from app.tools.materials.informatics import create_informatics_tools

    reg = ToolRegistry()
    create_informatics_tools(reg)
    candidates = [
        {"formula": "A", "density": 1, "modulus": 100},  # Pareto (lightest)
        {"formula": "B", "density": 5, "modulus": 200},   # Pareto (stiffest)
        {"formula": "C", "density": 5, "modulus": 100},   # dominated by both
        {"formula": "D", "density": 3, "modulus": 150},   # Pareto (compromise)
    ]
    out = reg.get("pareto_screen").func(
        candidates=candidates,
        objectives=[{"property": "density", "direction": "min"},
                    {"property": "modulus", "direction": "max"}],
    )
    front_formulas = {f["candidate"]["formula"] for f in out["pareto_front"]}
    assert front_formulas == {"A", "B", "D"}, f"Pareto front wrong: {front_formulas}"
    assert out["dominated_count"] == 1  # only C is dominated
    assert out["pareto_count"] == 3


# ---- E11: suggest_next_experiments ----

def test_suggest_next_experiments_ranks_by_ei():
    """EI should favor high-uncertainty + near-best candidates."""
    from app.tools.base import ToolRegistry
    from app.tools.materials.informatics import create_informatics_tools

    reg = ToolRegistry()
    create_informatics_tools(reg)
    # Candidate with high uncertainty but below best should still rank (exploration).
    candidates = [
        {"formula": "certain_good", "predicted": 9.5, "uncertainty": 0.1},
        {"formula": "uncertain_mid", "predicted": 7.0, "uncertainty": 3.0},  # high σ → high EI
        {"formula": "certain_bad", "predicted": 1.0, "uncertainty": 0.1},
    ]
    out = reg.get("suggest_next_experiments").func(candidates=candidates, n_suggestions=2, acquisition="ei", direction="max")
    top = out["suggestions"][0]["formula"]
    # The high-uncertainty one should win on EI (exploration value).
    assert top == "uncertain_mid", f"EI should favor high-σ exploration: {top}"
    assert out["acquisition"] == "ei"


# ---- E8: compute_descriptor ----

def test_compute_descriptor_magpie():
    """matminer magpie featurization (132 features)."""
    from app.tools.base import ToolRegistry
    from app.tools.materials.informatics import create_informatics_tools

    reg = ToolRegistry()
    create_informatics_tools(reg)
    try:
        from matminer.featurizers.composition import ElementProperty  # noqa
    except ImportError:
        pytest.skip("matminer not installed")
    out = reg.get("compute_descriptor").func(formulas=["Cu2O"])
    assert len(out["descriptors"]) == 1
    assert out["descriptors"][0]["n_features"] == 132


# ---- E9: predict_property (needs ML stack — verify it degrades honestly if missing) ----

def test_predict_property_requires_formulas():
    from app.tools.base import ToolRegistry
    from app.tools.materials.informatics import create_informatics_tools

    reg = ToolRegistry()
    create_informatics_tools(reg)
    out = reg.get("predict_property").func()
    assert "error" in out


def test_predict_property_reports_actual_feature_backend():
    """C6 honesty: the provenance must report the feature backend that ACTUALLY
    ran (matminer Magpie only when matminer is installed; otherwise the builtin
    22-feature fallback) — the old code hardcoded 'matminer magpie' even when
    the fallback ran. ML plumbing is mocked; this tests the provenance logic.
    """
    from app.tools.base import ToolRegistry
    from app.tools.materials.informatics import create_informatics_tools

    try:
        import sklearn  # noqa: F401
        import numpy  # noqa: F401
    except ImportError:
        pytest.skip("sklearn/numpy not installed")

    reg = ToolRegistry()
    create_informatics_tools(reg)

    formulas = [f"El{i}O{i % 3 + 1}" for i in range(30)]
    fake_mp = {
        "results": [
            {"formula_pretty": f, "formation_energy_per_atom": -0.05 * i}
            for i, f in enumerate(formulas)
        ]
    }

    def _fake_features(formula):
        # deterministic small vector so train/predict run without matminer
        return [float(len(formula)), float(sum(map(ord, formula)) % 97), 1.0]

    with (
        patch("app.tools.data._query_materials_project", return_value=fake_mp),
        patch("app.tools.ml.features.composition_features", side_effect=_fake_features),
    ):
        out = reg.get("predict_property").func(formulas=["Cu2O"])

    assert "predictions" in out, f"predict_property failed: {out}"
    backend = out["model_meta"]["feature_backend"]

    from app.tools.ml.features import get_feature_backend

    if get_feature_backend() == "matminer":
        assert "matminer" in backend
    else:
        # matminer absent → must NOT claim magpie; must name the real fallback
        assert "matminer magpie" != backend
        assert "builtin" in backend and "matminer not installed" in backend
        assert "matminer magpie" not in out["provenance"]
        assert "builtin" in out["provenance"]


# ---- E12: predict_synthesizability (heuristic) ----

def test_synthesizability_is_honest_heuristic():
    from app.tools.base import ToolRegistry
    from app.tools.materials.structure_desc import create_structure_desc_tools

    reg = ToolRegistry()
    create_structure_desc_tools(reg)
    # Mock the MP lookup so the hull factor returns a stable value.
    with patch("app.tools.data._query_materials_project",
               return_value={"results": [{"material_id": "mp-1", "energy_above_hull": 0.0}]}):
        out = reg.get("predict_synthesizability").func(formula="Cu2O")
    assert "score" in out
    assert out["likely_synthesizable"] is True  # on-hull → synthesizable
    assert "HEURISTIC" in out["note"]
    assert len(out["factors"]) >= 1


def test_describe_structure_degrades_without_robocrystallographer():
    """On py3.14 robocrystallographer isn't installed — honest degrade."""
    from app.tools.base import ToolRegistry
    from app.tools.materials.structure_desc import create_structure_desc_tools

    reg = ToolRegistry()
    create_structure_desc_tools(reg)
    try:
        import robocrystallographer  # noqa
        pytest.skip("robocrystallographer installed — degrade path can't be tested")
    except ImportError:
        out = reg.get("describe_structure").func(formula="Cu2O")
        assert out.get("tool_available") is False
        assert "robocrystallographer" in out["error"].lower()
