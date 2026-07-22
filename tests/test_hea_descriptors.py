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
    """The REAL Cantor alloy is CoCrFeMnNi — the canonical single-phase FCC HEA.

    Literature anchors: VEC = 8.00 exactly (Guo & Liu 2011: VEC ≥ 8.0 → FCC);
    ΔH_mix = -4.16 kJ/mol (Takeuchi-Inoue 2005 pair table); δ ≈ 1.1% with
    Goldschmidt CN12 metallic radii (Yang & Zhang 2012 convention).
    """
    elems, fracs = _parse_composition("Co0.2Cr0.2Fe0.2Mn0.2Ni0.2")
    d = compute_hea_descriptors(elems, fracs)
    assert d["phase_prediction"] == "solid_solution"
    assert d["segregation_risk"] is False
    # VEC = (9+6+8+7+10)/5 = 8.00 exactly → FCC (Guo & Liu 2011 threshold)
    assert abs(d["VEC"] - 8.0) < 1e-6
    assert any("FCC favored" in n for n in d["rationale"])
    assert not any("mixed" in n for n in d["rationale"])
    # ΔH_mix must reproduce the Takeuchi-Inoue value exactly
    assert abs(d["delta_H_mix_kJ_per_mol"] - (-4.16)) < 0.01
    # Ω must be comfortably > 1.1
    assert d["omega"] > 1.1
    # δ ≈ 1.1% with metallic (CN12) radii. The old Slater .atomic_radius set
    # gave 1.77% — assert tightly enough to catch a regression to Slater radii.
    assert 0.9 < d["delta_radius_pct"] < 1.4


def test_senkov_refractory_hea_is_solid_solution_bcc():
    """NbMoTaW is the canonical refractory BCC HEA (Senkov et al.)."""
    elems, fracs = _parse_composition("NbMoTaW")
    d = compute_hea_descriptors(elems, fracs)
    assert d["phase_prediction"] == "solid_solution"
    # VEC = (5+6+5+6)/4 = 5.5 < 6.87 → BCC (Guo & Liu 2011)
    assert abs(d["VEC"] - 5.5) < 1e-6
    assert any("BCC favored" in n for n in d["rationale"])
    # ΔH_mix = 4/16·(Mo-Nb -6 + Mo-Ta -5 + Nb-W -8 + Ta-W -7) = -6.5 kJ/mol
    # (Senkov pair set from Takeuchi-Inoue)
    assert abs(d["delta_H_mix_kJ_per_mol"] - (-6.5)) < 0.01


def test_cocrfenicu_flags_segregation_risk():
    """CrFeNiCoCu (NOT the Cantor alloy) has ΔH_mix = +3.2 kJ/mol: Cu has
    positive mixing enthalpy with Fe/Cr/Co/Ni and segregates into a Cu-rich
    second FCC phase — the tool must flag this, never return a plain
    solid_solution verdict."""
    elems, fracs = _parse_composition("Cr0.2Fe0.2Ni0.2Co0.2Cu0.2")
    d = compute_hea_descriptors(elems, fracs)
    assert abs(d["delta_H_mix_kJ_per_mol"] - 3.2) < 0.01  # Takeuchi-Inoue pairs
    assert d["segregation_risk"] is True
    assert d["phase_prediction"] == "solid_solution_segregation_risk"
    assert any("segregation" in n.lower() for n in d["rationale"])


def test_vec_thresholds_guo_liu_2011():
    """Guo & Liu 2011: FCC stable at VEC ≥ 8.0, BCC at VEC < 6.87, duplex in
    [6.87, 8.0). The old 8.6/8.0 thresholds mislabeled CoCrFeMnNi (VEC 8.00,
    real single-phase FCC) as mixed."""
    fcc = compute_hea_descriptors(*_parse_composition("Co0.2Cr0.2Fe0.2Mn0.2Ni0.2"))
    assert any("FCC favored" in n for n in fcc["rationale"])
    bcc = compute_hea_descriptors(*_parse_composition("NbMoTaW"))
    assert any("BCC favored" in n for n in bcc["rationale"])
    # Al0.3CoCrFeNi: VEC = (0.3·3 + 9 + 6 + 8 + 10)/4.3 ≈ 7.88 → duplex window
    mixed = compute_hea_descriptors(*_parse_composition("Al0.3CoCrFeNi"))
    assert 6.87 <= mixed["VEC"] < 8.0
    assert any("mixed" in n for n in mixed["rationale"])


def test_miedema_pairs_match_takeuchi_inoue():
    """Spot-check the ΔH_mix pair table against the printed Takeuchi-Inoue 2005
    values that the adversarial review verified (plus the Senkov refractory
    set). These were wrong before (e.g. Ni-Ti was -18)."""
    from app.tools.materials.hea import _dh_mix_for_pair

    expected = {
        ("Ni", "Ti"): -35, ("Ni", "Nb"): -30, ("Ni", "Zr"): -49,
        ("Mo", "Si"): -35, ("Cr", "Ta"): -7, ("Co", "Ti"): -28,
        ("Nb", "W"): -8, ("Ta", "W"): -7, ("Mo", "Ta"): -5,
        ("Al", "Ni"): -22,  # Al row was verified correct
    }
    for (a, b), v in expected.items():
        assert _dh_mix_for_pair(a, b) == v, f"{a}-{b}: {_dh_mix_for_pair(a, b)} != {v}"
        assert _dh_mix_for_pair(b, a) == v  # symmetric


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
