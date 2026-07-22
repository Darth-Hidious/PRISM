"""Tests for the HEA formability descriptors tool (E3)."""
import math

import pytest

from app.tools.materials.hea import compute_hea_descriptors, _parse_composition


def test_parse_composition_formula():
    elems, fracs = _parse_composition("Cr0.2Fe0.2Ni0.2Co0.2Cu0.2")
    assert set(elems) == {"Cr", "Fe", "Ni", "Co", "Cu"}
    assert abs(sum(fracs) - 1.0) < 1e-9
    assert all(abs(f - 0.2) < 1e-6 for f in fracs)


def test_parse_composition_dict():
    elems, fracs = _parse_composition({"Nb": 1, "Mo": 1, "Ta": 1, "W": 1})
    assert len(elems) == 4
    assert all(abs(f - 0.25) < 1e-6 for f in fracs)  # equal fractions


def test_parse_composition_rejects_invalid():
    assert _parse_composition("not_a_formula") is None or _parse_composition("Fe") is None
    assert _parse_composition({}) is None


def test_cantor_alloy_is_solid_solution_fcc():
    """The Cantor alloy (CrFeNiCoCu) is THE canonical single-phase FCC HEA."""
    elems, fracs = _parse_composition("Cr0.2Fe0.2Ni0.2Co0.2Cu0.2")
    d = compute_hea_descriptors(elems, fracs)
    assert d["phase_prediction"] == "solid_solution"
    # VEC ~8.8 (textbook value) → FCC
    assert 8.5 < d["VEC"] < 9.0
    assert any("FCC" in n for n in d["rationale"])
    # Ω must be comfortably > 1.1
    assert d["omega"] > 1.1
    # δ must be small (similar-size 3d TMs)
    assert d["delta_radius_pct"] < 6.6


def test_senkov_refractory_hea_is_solid_solution_bcc():
    """NbMoTaW is the canonical refractory BCC HEA."""
    elems, fracs = _parse_composition("NbMoTaW")
    d = compute_hea_descriptors(elems, fracs)
    assert d["phase_prediction"] == "solid_solution"
    # VEC ~5.5 → BCC
    assert 5.0 < d["VEC"] < 6.0
    assert any("BCC" in n for n in d["rationale"])


def test_binary_not_hea():
    """A simple binary (FeO) is NOT a solid-solution HEA."""
    elems, fracs = _parse_composition("FeO")
    d = compute_hea_descriptors(elems, fracs)
    assert d["phase_prediction"] != "solid_solution"
    assert d["n_elements"] == 2


def test_mixing_enthalpy_negative_for_forming_pair():
    """Al-Ni has a strongly negative ΔH_mix (-22 kJ/mol) — should pull ΔH_mix down."""
    elems, fracs = _parse_composition("Al0.5Ni0.5")
    d = compute_hea_descriptors(elems, fracs)
    assert d["delta_H_mix_kJ_per_mol"] < -10  # strongly negative


def test_vec_is_concentration_weighted():
    """VEC must be the atomic-fraction-weighted sum of element VECs."""
    elems, fracs = _parse_composition("Cu0.5Ni0.5")
    d = compute_hea_descriptors(elems, fracs)
    # Cu VEC=11, Ni VEC=10 → 10.5
    assert abs(d["VEC"] - 10.5) < 0.1


def test_delta_S_mix_bounded():
    """ΔS_mix for an n-element equimolar alloy = R·ln(n)."""
    elems, fracs = _parse_composition("NbMoTaW")  # 4 elements, equimolar
    d = compute_hea_descriptors(elems, fracs)
    expected = 8.314 * math.log(4)  # R ln(4)
    assert abs(d["delta_S_mix_J_per_molK"] - expected) < 0.1


def test_tool_registered():
    from app.tools.base import ToolRegistry
    from app.tools.materials.hea import create_hea_tools

    reg = ToolRegistry()
    create_hea_tools(reg)
    assert reg.get("hea_descriptors") is not None
    # Authoring contract: typed schema + examples + honest closed schema
    t = reg.get("hea_descriptors")
    assert t.input_schema["additionalProperties"] is False
    assert t.examples is not None and len(t.examples) >= 1
