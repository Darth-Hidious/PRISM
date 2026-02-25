"""Tests for cross-provider material fusion — merge, dedup, conflict resolution."""
from app.search.result import Material, PropertyValue


def _mat(pid, formula="Fe2O3", sg="R-3c", band_gap=None, extra=None):
    m = Material(
        id=f"{pid}-1", formula=formula, elements=["Fe", "O"], n_elements=2,
        sources=[pid],
        space_group=PropertyValue(value=sg, source=f"optimade:{pid}") if sg else None,
        band_gap=PropertyValue(value=band_gap, source=f"optimade:{pid}", unit="eV") if band_gap else None,
        extra_properties=extra or {},
    )
    return m


def test_fusion_merges_same_material():
    from app.search.fusion import fuse_materials
    m1 = _mat("mp", band_gap=2.2)
    m2 = _mat("aflow", band_gap=None, extra={
        "_aflow_bulk_modulus": PropertyValue(value=220, source="optimade:aflow", unit="GPa"),
    })
    fused = fuse_materials([m1, m2])
    assert len(fused) == 1
    f = fused[0]
    assert "mp" in f.sources
    assert "aflow" in f.sources
    assert f.band_gap.value == 2.2
    assert "_aflow_bulk_modulus" in f.extra_properties


def test_fusion_keeps_different_materials_separate():
    from app.search.fusion import fuse_materials
    m1 = _mat("mp", formula="Fe2O3", sg="R-3c")
    m2 = _mat("mp", formula="SiO2", sg="P3_221")
    m2.elements = ["O", "Si"]
    m2.id = "mp-2"
    fused = fuse_materials([m1, m2])
    assert len(fused) == 2


def test_fusion_handles_conflicting_values():
    from app.search.fusion import fuse_materials
    m1 = _mat("mp", band_gap=2.2)
    m2 = _mat("aflow", band_gap=2.0)
    fused = fuse_materials([m1, m2])
    assert len(fused) == 1
    f = fused[0]
    assert f.band_gap.value == 2.2
    assert "band_gap:aflow" in f.extra_properties
    assert f.extra_properties["band_gap:aflow"].value == 2.0


def test_fusion_empty_input():
    from app.search.fusion import fuse_materials
    assert fuse_materials([]) == []
