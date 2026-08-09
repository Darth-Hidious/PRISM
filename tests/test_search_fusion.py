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


def _optimade_material(provider_id, attributes, entry_id="x-1"):
    """Parse a raw OPTIMADE entry through the REAL adapter, then fuse it."""
    from app.tools.search_engine.providers.endpoint import (
        BehaviorConfig,
        CapabilitiesConfig,
        ProviderEndpoint,
    )
    from app.tools.search_engine.providers.optimade import OptimadeProvider

    provider = OptimadeProvider(
        endpoint=ProviderEndpoint(
            id=provider_id, name=provider_id, base_url="https://example.org",
            api_type="optimade", enabled=True,
            behavior=BehaviorConfig(), capabilities=CapabilitiesConfig(),
        )
    )
    return provider._parse_entry({"id": entry_id, "attributes": attributes})


def test_tio2_polymorphs_from_three_providers_stay_three_materials():
    """Anatase, rutile and brookite share a formula, not an identity.

    Before: no provider returns the non-spec `space_group_symbol`, every
    identity recorded space_group="unknown", all polymorphs landed in ONE
    bucket, and truth-discovery averaged their genuinely different band gaps
    as competing claims about one material -- manufactured data."""
    from app.tools.search_engine.fusion import fuse_materials

    anatase = _optimade_material("mp", {
        "chemical_formula_reduced": "O2Ti", "elements": ["O", "Ti"],
        "nelements": 2, "space_group_it_number": 141,
    }, entry_id="mp-anatase")
    rutile = _optimade_material("oqmd", {
        "chemical_formula_descriptive": "TiO2", "elements": ["O", "Ti"],
        "nelements": 2, "space_group_it_number": 136,
    }, entry_id="oqmd-rutile")
    brookite = _optimade_material("cod", {
        "chemical_formula_reduced": "O2Ti", "elements": ["O", "Ti"],
        "nelements": 2, "space_group_symbol_hermann_mauguin": "Pbca",
    }, entry_id="cod-brookite")

    fused = fuse_materials([anatase, rutile, brookite])

    assert len(fused) == 3
    # Each polymorph keeps its own symmetry; none was excluded from fusion --
    # they are fusable identities that are genuinely DIFFERENT.
    assert {m.identity.attributes["space_group"] for m in fused} == {
        "141", "136", "Pbca",
    }
    assert all(m.fusion_exclusion is None for m in fused)
    # And the formula spellings ("TiO2" vs "O2Ti") did key identically.
    assert {m.identity.attributes["formula"] for m in fused} == {"O2Ti"}


def test_same_polymorph_from_optimade_and_mp_native_still_merges():
    """Entries that DO carry symmetry keep merging -- across adapters: the
    OPTIMADE it_number and MP's symmetry number produce the same key."""
    from app.tools.search_engine.fusion import fuse_materials
    from app.tools.search_engine.providers.endpoint import ProviderEndpoint
    from app.tools.search_engine.providers.materials_project import (
        MaterialsProjectProvider,
    )

    via_optimade = _optimade_material("oqmd", {
        "chemical_formula_reduced": "O2Ti", "elements": ["O", "Ti"],
        "nelements": 2, "space_group_it_number": 136,
        "space_group_symbol_hermann_mauguin": "P4_2/mnm",
    }, entry_id="oqmd-136")

    mp = MaterialsProjectProvider(
        endpoint=ProviderEndpoint(
            id="mp_native", name="MP", base_url="https://api.materialsproject.org",
            api_type="mp_native", enabled=True,
        )
    )
    via_mp_native = mp._parse_doc({
        "material_id": "mp-2657", "formula_pretty": "TiO2",
        "elements": ["O", "Ti"], "nelements": 2,
        "symmetry": {"symbol": "P4_2/mnm", "number": 136},
    })

    fused = fuse_materials([via_optimade, via_mp_native])
    assert len(fused) == 1
    assert set(fused[0].sources) == {"oqmd", "mp_native"}


def test_record_without_symmetry_is_not_fused_with_anything():
    """An absent discriminator is never a value to merge on: two same-formula
    records with no symmetry stay separate (from each other AND from the
    record that has symmetry), each carrying an honest audit note."""
    from app.tools.search_engine.fusion import fuse_materials

    with_symmetry = _optimade_material("mp", {
        "chemical_formula_reduced": "O2Ti", "elements": ["O", "Ti"],
        "nelements": 2, "space_group_it_number": 136,
    }, entry_id="mp-rutile")
    bare_a = _optimade_material("cod", {
        "chemical_formula_reduced": "O2Ti", "elements": ["O", "Ti"],
        "nelements": 2,
    }, entry_id="cod-bare")
    bare_b = _optimade_material("nmd", {
        "chemical_formula_reduced": "O2Ti", "elements": ["O", "Ti"],
        "nelements": 2,
    }, entry_id="nmd-bare")

    fused = fuse_materials([with_symmetry, bare_a, bare_b])

    assert len(fused) == 3
    notes = [m.fusion_exclusion for m in fused if m.fusion_exclusion]
    assert len(notes) == 2
    assert all("no symmetry data" in note for note in notes)
    keyed = [m for m in fused if m.fusion_exclusion is None]
    assert len(keyed) == 1 and keyed[0].sources == ["mp"]


def test_legacy_unknown_sentinel_is_not_a_mergeable_space_group():
    """Contract enforcement against FUTURE adapters: an identity carrying the
    old space_group="unknown" sentinel is treated as having no discriminator,
    not as a value shared by every polymorph of the formula."""
    from app.tools.search_engine.fusion import fuse_materials

    a = _mat("prov-a", formula="TiO2", sg="unknown")
    b = _mat("prov-b", formula="TiO2", sg="unknown")
    fused = fuse_materials([a, b])
    assert len(fused) == 2
    assert all("no symmetry data" in m.fusion_exclusion for m in fused)


def test_legacy_record_without_identity_carries_an_honest_note():
    from app.tools.search_engine.fusion import fuse_materials

    legacy = _mat("mp")
    legacy.identity = None
    fused = fuse_materials([legacy])
    assert len(fused) == 1
    assert fused[0].fusion_exclusion == "not mergeable: no domain identity"


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
