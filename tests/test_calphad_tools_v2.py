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
    """When pycalphad isn't installed, the tools return a clear install hint, not a crash.

    Uses IN-WINDOW steel compositions so the scope guard passes and the
    pycalphad-availability check is actually reached.
    """
    from app.tools.base import ToolRegistry
    from app.tools.materials.calculations import create_calphad_tools, _calphad_available

    reg = ToolRegistry()
    create_calphad_tools(reg)
    if _calphad_available():
        pytest.skip("pycalphad IS installed on this venv — the degrade path can't be tested here")
    out = reg.get("hea_phase_stability").func(composition="Fe0.7Cr0.2Ni0.1")
    assert out["tool_available"] is False
    assert "calphad" in out["error"].lower()
    out2 = reg.get("scheil_solidification").func(composition="Fe0.8Cr0.15Ni0.05")
    assert out2["tool_available"] is False


def test_bundled_tdb_present():
    """The open steel TDB ships with the tool (no download needed at runtime)."""
    from app.tools.materials.calculations import _resolve_tdb_path, DEFAULT_TDB

    p = _resolve_tdb_path(DEFAULT_TDB)
    assert p is not None and p.exists(), "the bundled steel_odbl.tdb must ship"
    assert p.name == "steel_odbl.tdb"


def test_tdb_covers_steel_alloying_elements():
    """The bundled TDB is a DILUTE-STEEL database: it contains the common steel
    alloying elements, and it does NOT contain Ta (so refractory HEAs like
    NbMoTaW cannot even be represented — they must be refused, not run)."""
    from app.tools.materials.calculations import _resolve_tdb_path

    p = _resolve_tdb_path("steel_odbl")
    text = p.read_text()
    for el in ["CR", "NI", "CO", "CU", "MO", "NB", "FE"]:
        assert f"ELEMENT {el}" in text or f"Element {el}" in text, f"{el} missing from TDB"
    assert "ELEMENT TA" not in text and "Element TA" not in text, (
        "mc_fe has no Ta assessment — if this changes, revisit the scope guard"
    )


# ---- SCI-6: scope-honesty guard (the bundled TDB is dilute steel, NOT HEA) ----

def _make_registry():
    from app.tools.base import ToolRegistry
    from app.tools.materials.calculations import create_calphad_tools

    reg = ToolRegistry()
    create_calphad_tools(reg)
    return reg


def test_out_of_window_refractory_hea_refused():
    """NbMoTaW (the flagship refractory HEA) is 100% outside the dilute-steel
    DB window (no Ta in the DB at all, no Fe) — the tool must REFUSE, not
    silently extrapolate. Works regardless of whether pycalphad is installed."""
    reg = _make_registry()
    out = reg.get("hea_phase_stability").func(composition="NbMoTaW")
    assert out.get("out_of_scope") is True
    assert "error" in out
    assert "Ta" in out["error"]
    assert "steel" in out["error"].lower()
    # the refusal must point at honest alternatives, not leave a dead end
    assert "hea_descriptors" in out["error"]


def test_out_of_window_equimolar_3d_hea_refused():
    """Cr20Ni20Co20Fe20Cu20 (the old schema's own example!) is 10-25x outside
    the assessed wt% limits (Co<3, Cu<1 wt%) — must be refused."""
    reg = _make_registry()
    out = reg.get("hea_phase_stability").func(composition="Cr0.2Ni0.2Co0.2Fe0.2Cu0.2")
    assert out.get("out_of_scope") is True
    joined = " ".join(out["violations"])
    assert "Co" in joined and "Cu" in joined


def test_scheil_refuses_out_of_window_composition():
    """The Scheil tool shares the same steel-DB scope guard (Al0.9Cu0.1 is
    ~79 wt% Al with no Fe — nowhere near a dilute steel)."""
    reg = _make_registry()
    out = reg.get("scheil_solidification").func(composition="Al0.9Cu0.1")
    assert out.get("out_of_scope") is True
    assert "steel" in out["error"].lower()


def test_in_window_steel_passes_the_guard():
    """An Fe-base composition inside the assessed window must NOT be refused
    for scope (it proceeds to the pycalphad-availability check / computation)."""
    reg = _make_registry()
    out = reg.get("hea_phase_stability").func(composition="Fe0.7Cr0.2Ni0.1")
    assert out.get("out_of_scope") is None


def test_temperature_window_enforced():
    """The DB is assessed 673-2000 K: the default grid must start inside it,
    and an explicit request below the floor must be refused."""
    from app.tools.materials.calculations import _DEFAULT_T_RANGE, _STEEL_DB_T_RANGE_K

    assert _DEFAULT_T_RANGE[0] >= _STEEL_DB_T_RANGE_K[0], (
        "default T floor must be inside the DB's assessed window (was 500 K < 673 K)"
    )
    reg = _make_registry()
    out = reg.get("hea_phase_stability").func(
        composition="Fe0.7Cr0.2Ni0.1", temperature_range=[500, 2000, 100]
    )
    assert out.get("out_of_scope") is True
    assert any("temperature" in v for v in out["violations"])


def test_no_tchea_claims_in_tool_surface():
    """The dilute-steel TDB must never be marketed as TCHEA / a free Thermo-Calc.

    Mentioning TCHEA in a NEGATION ("not a TCHEA substitute") is fine; claiming
    equivalence is not.
    """
    reg = _make_registry()
    for name in ("hea_phase_stability", "scheil_solidification"):
        t = reg.get(name)
        surface = (t.description + " " + str(t.input_schema)).lower()
        assert "free thermo-calc" not in surface, f"{name} claims free Thermo-Calc"
        assert "tchea equivalent" not in surface, f"{name} claims TCHEA equivalence"
        assert "tchea-equilibrium equivalent" not in surface
        assert "steel" in surface, f"{name} must state its steel scope"


def test_requires_composition():
    from app.tools.base import ToolRegistry
    from app.tools.materials.calculations import create_calphad_tools, _calphad_available

    reg = ToolRegistry()
    create_calphad_tools(reg)
    if not _calphad_available():
        pytest.skip("needs pycalphad to reach the composition-validation branch")
    out = reg.get("hea_phase_stability").func()
    assert "error" in out
