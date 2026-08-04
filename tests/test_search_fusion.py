"""Tests for cross-provider material fusion and its audit trail."""
import pytest

from app.tools.search_engine.result import Material, MaterialIdentity, PropertyValue


def _mat(
    pid,
    formula="Fe2O3",
    sg="R-3c",
    band_gap=None,
    extra=None,
):
    return Material(
        id=f"{pid}-1",
        formula=formula,
        elements=["Fe", "O"],
        n_elements=2,
        sources=[pid],
        identity=MaterialIdentity(
            domain="crystal",
            representation="formula_space_group",
            attributes={"formula": formula, "space_group": sg},
        ),
        space_group=PropertyValue(value=sg, source=f"optimade:{pid}") if sg else None,
        band_gap=(
            PropertyValue(value=band_gap, source=f"optimade:{pid}", unit="eV")
            if band_gap is not None
            else None
        ),
        extra_properties=extra or {},
    )


def _polymer(pid, repeat_unit, dielectric_constant):
    return Material(
        id=f"{pid}-polymer-1",
        formula="",
        elements=[],
        n_elements=0,
        sources=[pid],
        identity=MaterialIdentity(
            domain="polymer",
            representation="repeat_unit",
            attributes={"repeat_unit": repeat_unit},
        ),
        extra_properties={
            "dielectric_constant": PropertyValue(
                value=dielectric_constant,
                source=f"literature:{pid}",
            )
        },
    )


def test_fusion_merges_same_material():
    from app.tools.search_engine.fusion import fuse_materials

    m1 = _mat("mp", band_gap=2.2)
    m2 = _mat(
        "aflow",
        band_gap=None,
        extra={
            "_aflow_bulk_modulus": PropertyValue(
                value=220,
                source="optimade:aflow",
                unit="GPa",
            ),
        },
    )
    fused = fuse_materials([m1, m2])
    assert len(fused) == 1
    fused_material = fused[0]
    assert set(fused_material.sources) == {"mp", "aflow"}
    assert fused_material.band_gap.value == 2.2
    assert "_aflow_bulk_modulus" in fused_material.extra_properties


def test_fusion_keeps_different_materials_separate():
    from app.tools.search_engine.fusion import fuse_materials

    m1 = _mat("mp", formula="Fe2O3", sg="R-3c")
    m2 = _mat("mp", formula="SiO2", sg="P3_221")
    m2.elements = ["O", "Si"]
    m2.id = "mp-2"
    fused = fuse_materials([m1, m2])
    assert len(fused) == 2


def test_fusion_selects_the_source_with_observed_agreement_not_arrival_order():
    """A concrete provider conflict keeps the loser and records both weights."""
    from app.tools.search_engine.fusion import fuse_materials

    # OPTIMADE AFLOW arrives first but disagrees with the independently agreeing
    # Materials Project and OQMD observations on a second material.
    target_from_aflow = _mat("aflow", band_gap=2.0)
    target_from_mp = _mat("mp", band_gap=2.2)
    reference_from_mp = _mat("mp", formula="SiO2", sg="P3_221", band_gap=8.9)
    reference_from_aflow = _mat(
        "aflow", formula="SiO2", sg="P3_221", band_gap=7.6
    )
    reference_from_oqmd = _mat("oqmd", formula="SiO2", sg="P3_221", band_gap=8.9)

    fused = fuse_materials(
        [
            target_from_aflow,
            target_from_mp,
            reference_from_mp,
            reference_from_aflow,
            reference_from_oqmd,
        ]
    )

    target = fused[0]
    assert target.band_gap.value == 2.2
    assert target.band_gap.source == "optimade:mp"
    assert target.extra_properties["band_gap:optimade:aflow"].value == 2.0

    decision = target.fusion_audit["band_gap"]
    assert decision.resolved is True
    assert len(decision.candidates) == 2
    winner, loser = sorted(
        decision.candidates,
        key=lambda candidate: candidate.selected,
    )
    assert winner.source == "optimade:aflow"
    assert winner.selected is False
    assert loser.source == "optimade:mp"
    assert loser.selected is True
    assert loser.source_reliability > winner.source_reliability
    assert loser.extraction_reliability == winner.extraction_reliability
    assert loser.combined_weight > winner.combined_weight


def test_polymer_repeat_unit_identity_fuses_without_colliding_with_a_different_polymer(
):
    from app.tools.search_engine.fusion import fuse_materials

    polyethylene_from_paper_a = _polymer("paper-a", "[-CH2-CH2-]", 2.3)
    polyethylene_from_paper_b = _polymer("paper-b", "[-CH2-CH2-]", 2.3)
    ptfe_from_paper_c = _polymer("paper-c", "[-CF2-CF2-]", 2.1)

    fused = fuse_materials(
        [polyethylene_from_paper_a, polyethylene_from_paper_b, ptfe_from_paper_c]
    )

    assert len(fused) == 2
    polyethylene = next(
        material
        for material in fused
        if material.identity.attributes["repeat_unit"] == "[-CH2-CH2-]"
    )
    assert set(polyethylene.sources) == {"paper-a", "paper-b"}
    assert polyethylene.extra_properties["dielectric_constant"].value == 2.3


def test_fusion_rejects_an_unknown_identity_domain():
    from app.tools.search_engine.fusion import fuse_materials

    unknown = _mat("unknown")
    unknown.identity = MaterialIdentity(
        domain="unregistered_domain",
        representation="opaque",
        attributes={"opaque": "value"},
    )

    with pytest.raises(ValueError, match="unknown material identity domain"):
        fuse_materials([unknown])


def test_fusion_empty_input():
    from app.tools.search_engine.fusion import fuse_materials

    assert fuse_materials([]) == []
