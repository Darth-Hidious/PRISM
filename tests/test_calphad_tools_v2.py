"""Tests for the CALPHAD phase-stability + Scheil tools (E5+E6).

On Python 3.14 pycalphad can't install (no symengine wheel), so these tests
verify the honest-degrade path + registration. On 3.12/3.13 the live path runs.
"""
import pytest


def test_tools_register():
    from app.tools.base import ToolRegistry
    from app.tools.materials.calculations import create_calphad_tools

    reg = ToolRegistry()
    create_calphad_tools(reg)
    assert reg.get("hea_phase_stability") is not None
    assert reg.get("scheil_solidification") is not None


def test_tools_have_typed_schemas():
    from app.tools.base import ToolRegistry
    from app.tools.materials.calculations import create_calphad_tools

    reg = ToolRegistry()
    create_calphad_tools(reg)
    for name in ("hea_phase_stability", "scheil_solidification"):
        t = reg.get(name)
        assert t.input_schema["additionalProperties"] is False
        assert t.requires_approval is True, f"{name} is compute-heavy → approval-gated"


def test_honest_degrade_when_pycalphad_missing():
    """When pycalphad isn't installed, the tools return a clear install hint, not a crash."""
    from app.tools.base import ToolRegistry
    from app.tools.materials.calculations import create_calphad_tools, _calphad_available

    reg = ToolRegistry()
    create_calphad_tools(reg)
    if _calphad_available():
        pytest.skip("pycalphad IS installed on this venv — the degrade path can't be tested here")
    out = reg.get("hea_phase_stability").func(composition="Fe0.7Cr0.2Ni0.1")
    assert out["tool_available"] is False
    assert "calphad" in out["error"].lower()
    out2 = reg.get("scheil_solidification").func(composition="Al0.9Cu0.1")
    assert out2["tool_available"] is False


def test_bundled_tdb_present():
    """The open steel TDB ships with the tool (no download needed at runtime)."""
    from app.tools.materials.calculations import _resolve_tdb_path, DEFAULT_TDB

    p = _resolve_tdb_path(DEFAULT_TDB)
    assert p is not None and p.exists(), "the bundled steel_odbl.tdb must ship"
    assert p.name == "steel_odbl.tdb"


def test_tdb_covers_hea_elements():
    """The bundled TDB must cover the HEA-relevant elements (Cr/Ni/Co/Cu/Mo/Nb/Fe)."""
    from app.tools.materials.calculations import _resolve_tdb_path

    p = _resolve_tdb_path("steel_odbl")
    text = p.read_text()
    for el in ["CR", "NI", "CO", "CU", "MO", "NB", "FE"]:
        assert f"ELEMENT {el}" in text or f"Element {el}" in text, f"{el} missing from TDB"


def test_requires_composition():
    from app.tools.base import ToolRegistry
    from app.tools.materials.calculations import create_calphad_tools, _calphad_available

    reg = ToolRegistry()
    create_calphad_tools(reg)
    if not _calphad_available():
        pytest.skip("needs pycalphad to reach the composition-validation branch")
    out = reg.get("hea_phase_stability").func()
    assert "error" in out
