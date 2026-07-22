"""Tests for the free materials-informatics tools (OPTIMADE redesign S8)."""
from unittest.mock import patch, MagicMock

import pytest


def _fake_material(formula, band_gap=None, bulk_modulus=None, sg="Fm-3m"):
    """Build a fused-Material-shaped dict (as materials_search returns)."""
    m = {
        "id": f"id-{formula}",
        "formula": formula,
        "elements": list(set(formula.replace("0123456789", ""))),
        "n_elements": 2,
        "sources": ["mock"],
    }
    if band_gap is not None:
        m["band_gap"] = {"value": band_gap, "source": "mock", "unit": "eV"}
    if bulk_modulus is not None:
        m["bulk_modulus"] = {"value": bulk_modulus, "source": "mock", "unit": "GPa"}
    m["space_group"] = {"value": sg, "source": "mock"}
    return m


def test_tools_register():
    """All three informatics tools register in a fresh registry."""
    from app.tools.base import ToolRegistry
    from app.tools.materials import create_materials_informatics_tools

    reg = ToolRegistry()
    create_materials_informatics_tools(reg)
    names = {t.name for t in reg.list_tools()}
    assert "screen_materials" in names
    assert "compare_materials" in names
    assert "lookup_structure" in names


def test_tools_follow_authoring_contract():
    """S8: tools carry typed input_schema + examples (PRISM-Alpha contract)."""
    from app.tools.base import ToolRegistry
    from app.tools.materials import create_materials_informatics_tools

    reg = ToolRegistry()
    create_materials_informatics_tools(reg)
    screen = reg.get("screen_materials")
    assert screen.input_schema["type"] == "object"
    assert "additionalProperties" in screen.input_schema
    # unit-bearing field names (the contract: units in names)
    assert "band_gap_eV" in screen.input_schema["properties"]
    assert "bulk_modulus_GPa" in screen.input_schema["properties"]
    # examples present (kills arg-fill errors)
    assert screen.examples is not None and len(screen.examples) >= 1


def test_screen_materials_ranks_by_property():
    """screen_materials ranks candidates by the requested property (descending)."""
    from app.tools.base import ToolRegistry
    from app.tools.materials import create_materials_informatics_tools

    reg = ToolRegistry()
    create_materials_informatics_tools(reg)
    screen = reg.get("screen_materials")
    # Patch the inner materials_search call to return controlled candidates.
    fake_ms = MagicMock()
    fake_ms.func.return_value = {
        "materials": [
            _fake_material("A", band_gap=0.5),
            _fake_material("B", band_gap=3.0),
            _fake_material("C", band_gap=1.5),
        ],
        "count": 3,
        "coverage": {"filter_strength": "element"},
        "providers_summary": {"succeeded": 1, "failed": 0},
        "warnings": [],
    }
    with patch("app.plugins.bootstrap.build_full_registry") as mock_boot:
        mock_boot.return_value = (MagicMock(get=MagicMock(return_value=fake_ms)), None, None)
        out = screen.func(elements=["Cu"], rank_by="band_gap", limit=10)
    formulas = [c["formula"] for c in out["candidates"]]
    assert formulas == ["B", "C", "A"], "must be ranked by band_gap descending"
    assert out["ranked_by"] == "band_gap"
    assert out["count"] == 3


def test_compare_materials_builds_matrix():
    """compare_materials returns a per-property matrix across candidates."""
    from app.tools.base import ToolRegistry
    from app.tools.materials import create_materials_informatics_tools

    reg = ToolRegistry()
    create_materials_informatics_tools(reg)
    cmp_tool = reg.get("compare_materials")
    fake_ms = MagicMock()

    def fake_func(**kwargs):
        f = kwargs.get("formula")
        return {"materials": [_fake_material(f, band_gap=1.0)]}

    fake_ms.func.side_effect = fake_func
    with patch("app.plugins.bootstrap.build_full_registry") as mock_boot:
        mock_boot.return_value = (MagicMock(get=MagicMock(return_value=fake_ms)), None, None)
        out = cmp_tool.func(materials=["Si", "Ge"], properties=["band_gap"])
    assert len(out["comparison"]) == 2
    assert "band_gap" in out["matrix"]
    # each formula maps to its band_gap value in the matrix
    assert out["matrix"]["band_gap"]["Si"] == 1.0


def test_lookup_structure_returns_best_hit():
    """lookup_structure returns the hit with the most populated properties."""
    from app.tools.base import ToolRegistry
    from app.tools.materials import create_materials_informatics_tools

    reg = ToolRegistry()
    create_materials_informatics_tools(reg)
    lookup = reg.get("lookup_structure")
    fake_ms = MagicMock()
    fake_ms.func.return_value = {
        "materials": [
            _fake_material("Si", band_gap=1.0),  # has band_gap + bulk_modulus + space_group
            {"id": "bare", "formula": "Si2", "elements": ["Si"], "n_elements": 1, "sources": ["x"]},
        ],
        "providers_summary": {"succeeded": 1, "failed": 0},
    }
    with patch("app.plugins.bootstrap.build_full_registry") as mock_boot:
        mock_boot.return_value = (MagicMock(get=MagicMock(return_value=fake_ms)), None, None)
        out = lookup.func(formula="Si")
    assert out["found"] is True
    assert out["formula"] == "Si"  # the richer hit, not the bare one
    assert "band_gap" in out["properties"]
    assert out["other_hits"]


def test_screen_requires_at_least_one_filter():
    """screen_materials errors honestly with no filters (not an empty search)."""
    from app.tools.base import ToolRegistry
    from app.tools.materials import create_materials_informatics_tools

    reg = ToolRegistry()
    create_materials_informatics_tools(reg)
    out = reg.get("screen_materials").func()
    assert "error" in out
