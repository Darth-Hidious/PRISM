"""Tests for the HEA formability descriptors tool (E3)."""
import math

import pytest

from app.tools.materials.hea import compute_hea_descriptors, _parse_composition


def test_parse_composition_formula():
    elems, fracs = _parse_composition("Cr0.2Fe0.2Ni0.2Co0.2Cu0.2")
    assert set(elems) == {"Cr", "Fe", "Ni", "Co", "Cu"}
    assert abs(sum(fracs) - 1.0) < 1e-9
    assert all(abs(f - 0.2) < 1e-6 for f in fracs)


def test_every_descriptor_says_where_it_came_from():
    """Each descriptor the tool reports carries its unit and the table or
    library property it was computed from — a number with no origin is a
    number a reader cannot check. A descriptor the tables could not cover is
    listed with its origin and a None value, never dropped."""
    from app.tools.materials.hea import descriptor_provenance

    result = compute_hea_descriptors(
        ["Co", "Cr", "Fe", "Mn", "Ni"], [0.2, 0.2, 0.2, 0.2, 0.2]
    )
    rows = result["descriptor_provenance"]
    assert rows == descriptor_provenance(result)
    by_name = {row["name"]: row for row in rows}
    for key in (
        "delta_H_mix_kJ_per_mol", "delta_S_mix_J_per_molK", "omega", "VEC",
        "delta_radius_pct", "delta_chi", "Tm_estimate_K",
    ):
        assert key in by_name, f"{key} has no provenance row"
        row = by_name[key]
        assert row["value"] == result[key]
        assert row["unit"], f"{key} has no unit"
        assert row["origin"] and len(row["origin"]) > 20, f"{key} has no origin"
    assert "Takeuchi" in by_name["delta_H_mix_kJ_per_mol"]["origin"]
    assert "Element.metallic_radius" in by_name["delta_radius_pct"]["origin"]
    # A composition the enthalpy table does not cover: the row stays, the
    # value is None, and the origin still says which table was consulted.
    gap = compute_hea_descriptors(["Ba", "Li"], [0.5, 0.5])
    gap_rows = {row["name"]: row for row in gap["descriptor_provenance"]}
    assert gap_rows["delta_H_mix_kJ_per_mol"]["value"] is None
    assert "Takeuchi" in gap_rows["delta_H_mix_kJ_per_mol"]["origin"]


def test_parse_composition_dict():
    elems, fracs = _parse_composition({"Nb": 0.25, "Mo": 0.25, "Ta": 0.25, "W": 0.25})
    assert len(elems) == 4
    assert all(abs(f - 0.25) < 1e-6 for f in fracs)  # equal fractions


def test_parse_composition_expands_standard_hea_shorthand_with_traceability():
    elems, fracs = _parse_composition("NbMoTaW")
    assert elems == ["Nb", "Mo", "Ta", "W"]
    assert fracs == [0.25, 0.25, 0.25, 0.25]

    from app.tools.base import ToolRegistry
    from app.tools.materials.hea import create_hea_tools

    registry = ToolRegistry()
    create_hea_tools(registry)
    result = registry.get("hea_descriptors").func(composition="NbMoTaW")
    assert result["original_composition"] == "NbMoTaW"
    assert result["expanded_composition"] == "Nb0.25Mo0.25Ta0.25W0.25"
    assert result["fractions"] == [0.25, 0.25, 0.25, 0.25]


def test_parse_composition_accepts_percent_and_decimal_shorthand():
    elems, fracs = _parse_composition("Nb25Mo25Ta25W25")
    assert elems == ["Nb", "Mo", "Ta", "W"]
    assert fracs == [0.25, 0.25, 0.25, 0.25]

    elems, fracs = _parse_composition("W0.5Ta0.3Mo0.2")
    assert elems == ["W", "Ta", "Mo"]
    assert fracs == [0.5, 0.3, 0.2]


def test_parse_composition_rejects_invalid():
    assert _parse_composition("not_a_formula") is None or _parse_composition("Fe") is None
    assert _parse_composition({}) is None


def test_parse_composition_rejects_non_unit_fractions_instead_of_normalizing():
    malformed = "W0.6 Mo0.2 Ta0.4 Nb0.4 V0.4"
    assert _parse_composition(malformed) is None
    assert _parse_composition(
        {"W": 0.6, "Mo": 0.2, "Ta": 0.4, "Nb": 0.4, "V": 0.4}
    ) is None


def test_composition_validation_enforces_symbols_finiteness_and_positivity():
    assert _parse_composition("W0.5000005Mo0.4999995") is not None
    assert _parse_composition({"W": float("nan"), "Mo": 0.5}) is None
    assert _parse_composition({"W": 1.0, "Mo": 0.0}) is None
    assert _parse_composition({"Xx": 0.5, "W": 0.5}) is None
    with pytest.raises(ValueError, match="must sum to 1.0"):
        compute_hea_descriptors(
            ["W", "Mo", "Ta", "Nb", "V"], [0.6, 0.2, 0.4, 0.4, 0.4]
        )


def test_hea_tool_rejects_non_unit_composition_without_descriptors():
    from app.tools.base import ToolRegistry
    from app.tools.materials.hea import create_hea_tools

    registry = ToolRegistry()
    create_hea_tools(registry)
    result = registry.get("hea_descriptors").func(
        composition="W0.6 Mo0.2 Ta0.4 Nb0.4 V0.4"
    )
    assert "must sum to 1.0" in result["error"]
    assert "Tm_estimate_K" not in result


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
    elems, fracs = _parse_composition("Nb0.25Mo0.25Ta0.25W0.25")
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
    bcc = compute_hea_descriptors(*_parse_composition("Nb0.25Mo0.25Ta0.25W0.25"))
    assert any("BCC favored" in n for n in bcc["rationale"])
    # Al0.3CoCrFeNi: VEC = (0.3·3 + 9 + 6 + 8 + 10)/4.3 ≈ 7.88 → duplex window
    mixed = compute_hea_descriptors(
        *_parse_composition(
            "Al0.06976744Co0.23255814Cr0.23255814Fe0.23255814Ni0.23255814"
        )
    )
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
    elems, fracs = _parse_composition("Fe0.5O0.5")
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
    elems, fracs = _parse_composition("Nb0.25Mo0.25Ta0.25W0.25")
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


def test_untabulated_pair_reports_a_gap_instead_of_an_ideal_solution():
    """A missing Miedema pair must not be read as ΔH_mix = 0.

    CoCrFeNiPd is a real HEA, but Pd has no Takeuchi-Inoue pairs in the
    bundled table. The old code contributed 0.0 for each Pd pair, which is a
    positive claim of ideality — and it errs the dangerous way: Ω scales as
    1/|ΔH_mix|, so invented zeros inflate Ω and push the verdict toward
    solid_solution. The screen must say it cannot answer, and say why.
    """
    d = compute_hea_descriptors(*_parse_composition("CoCrFeNiPd"))

    assert d["delta_H_mix_kJ_per_mol"] is None
    assert d["omega"] is None
    assert d["phase_prediction"] == "undetermined"
    assert d["segregation_risk"] is False

    gaps = " ".join(d["data_gaps"])
    assert "mixing enthalpy" in gaps
    assert "Pd" in gaps
    # δ and VEC do not depend on the pair table and must survive the gap.
    assert d["VEC"] is not None
    assert d["delta_radius_pct"] is not None


def test_a_tabulated_zero_is_a_number_not_a_gap():
    """The distinguishing case: Fe-W is a MEASURED 0.0 kJ/mol.

    This is why the fix could not simply treat 0.0 as "missing" — the table
    carries genuine zeros, so absence and ideality have to be different
    values, not the same one. An Fe-W-bearing alloy whose every pair is
    tabulated still gets a real ΔH_mix.
    """
    from app.tools.materials.hea import _dh_mix_for_pair

    assert _dh_mix_for_pair("Fe", "W") == 0.0
    assert _dh_mix_for_pair("Pd", "Fe") is None

    d = compute_hea_descriptors(*_parse_composition("FeWNbTa"))
    assert d["delta_H_mix_kJ_per_mol"] is not None
    assert d["data_gaps"] == []


def test_missing_vec_does_not_become_a_structure_call():
    """VEC = Σ c_i VEC_i with a 0.0 default drags the mean down and hands the
    Guo/Liu threshold a fabricated crystal-structure verdict. An element with
    no tabulated VEC must null the descriptor and name itself."""
    # Er is a real element (so composition validation accepts it) with no
    # tabulated VEC — the reachable form of this gap, not a synthetic symbol.
    d = compute_hea_descriptors(["Fe", "W", "Er"], [0.4, 0.4, 0.2])

    assert d["VEC"] is None
    assert not any("BCC favored" in note or "FCC favored" in note for note in d["rationale"])
    assert any("valence electron concentration" in gap and "Er" in gap for gap in d["data_gaps"])


def test_undetermined_always_names_the_missing_datum():
    """`undetermined` with an EMPTY data_gaps is the null-with-no-reason this
    contract exists to prevent, and the pair table's genuine zeros reach it.

    Nb0.5Ta0.5 has every pair tabulated (Nb-Ta = 0), so there is no missing
    pair and no missing VEC — but ΔH_mix sums to exactly 0.0, which makes
    Yang's Ω = Tm·ΔS_mix/|ΔH_mix| a division by zero. The screen reported
    phase_prediction 'undetermined', omega null and data_gaps [], with
    nothing saying which datum was missing. Same for a missing metallic
    radius.
    """
    d = compute_hea_descriptors(["Nb", "Ta"], [0.5, 0.5])
    assert d["delta_H_mix_kJ_per_mol"] == 0.0
    assert d["omega"] is None
    assert d["phase_prediction"] == "undetermined"
    assert d["data_gaps"], (
        "an undetermined verdict must name the datum it is missing, not return "
        f"an empty data_gaps: {d}"
    )
    gaps = " ".join(d["data_gaps"])
    assert "omega" in gaps and "division by zero" in gaps, gaps
    # The rationale the caller reads must carry the reason too.
    assert any("division by zero" in note for note in d["rationale"])

    # A missing Goldschmidt radius nulls δ, and that gap must be named as well.
    no_radius = compute_hea_descriptors(["Cu", "O"], [0.5, 0.5])
    assert no_radius["delta_radius_pct"] is None
    assert any(
        "metallic radius" in gap for gap in no_radius["data_gaps"]
    ), no_radius["data_gaps"]
