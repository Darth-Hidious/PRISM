"""Tests for the phase_stability tool (E4)."""
from unittest.mock import patch

import pytest


def test_tool_registered():
    from app.tools.base import ToolRegistry
    from app.tools.materials.stability import create_phase_stability_tool

    reg = ToolRegistry()
    create_phase_stability_tool(reg)
    t = reg.get("phase_stability")
    assert t.input_schema["additionalProperties"] is False
    assert t.examples is not None


def test_requires_formula_or_id():
    from app.tools.base import ToolRegistry
    from app.tools.materials.stability import create_phase_stability_tool

    reg = ToolRegistry()
    create_phase_stability_tool(reg)
    out = reg.get("phase_stability").func()
    assert "error" in out


def test_classify_stability_thresholds():
    from app.tools.materials.stability import _classify_stability

    assert "stable" in _classify_stability(0.0)
    assert "near-stable" in _classify_stability(0.03)
    assert "metastable" in _classify_stability(0.07)
    assert "unstable" in _classify_stability(0.5)
    assert _classify_stability(None) == "unknown"


def test_stability_from_mock_proxy():
    """With a mocked MP proxy returning an on-hull entry, the tool reports stable."""
    from app.tools.base import ToolRegistry
    from app.tools.materials.stability import create_phase_stability_tool

    reg = ToolRegistry()
    create_phase_stability_tool(reg)
    fake_result = {
        "results": [
            {"material_id": "mp-1", "formula_pretty": "Cu2O",
             "energy_above_hull": 0.0, "formation_energy_per_atom": -0.64,
             "theoretical": False, "density": 6.2},
            {"material_id": "mp-2", "formula_pretty": "Cu2O",
             "energy_above_hull": 0.12, "formation_energy_per_atom": -0.50},
        ],
        "source": "marc27_platform_proxy",
    }
    with patch("app.tools.data._query_materials_project", return_value=fake_result):
        out = reg.get("phase_stability").func(formula="Cu2O")
    assert out["found"] is True
    assert out["stable"] is True
    assert out["energy_above_hull_eV_per_atom"] == 0.0
    assert out["source"] == "marc27_platform_proxy"
    assert out["other_polymorphs"] == 1  # the second, less-stable entry
    # picks the MOST stable (lowest hull) entry
    assert out["material_id"] == "mp-1"


def test_proxy_error_surfaces_honestly():
    """If the proxy falls through to the keyless hint, surface it + the OPTIMADE fallback hint."""
    from app.tools.base import ToolRegistry
    from app.tools.materials.stability import create_phase_stability_tool

    reg = ToolRegistry()
    create_phase_stability_tool(reg)
    with patch("app.tools.data._query_materials_project",
               return_value={"error": "proxy unavailable"}):
        out = reg.get("phase_stability").func(formula="XYZ")
    assert "error" in out
    assert "materials_search" in out["hint"].lower()
