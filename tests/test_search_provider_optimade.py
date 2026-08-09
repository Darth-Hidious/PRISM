import pytest
from unittest.mock import patch, MagicMock

from app.tools.search_engine.query import MaterialSearchQuery
from app.tools.search_engine.providers.endpoint import ProviderEndpoint, AuthConfig, BehaviorConfig, CapabilitiesConfig


def _make_endpoint(pid="mp", url="https://optimade.materialsproject.org"):
    return ProviderEndpoint(
        id=pid, name="Test", base_url=url,
        api_type="optimade", tier=1, enabled=True,
        behavior=BehaviorConfig(timeout_ms=5000),
        capabilities=CapabilitiesConfig(
            filterable_fields=["elements", "formula", "nelements", "space_group"],
        ),
    )


def test_optimade_provider_creates():
    from app.tools.search_engine.providers.optimade import OptimadeProvider
    ep = _make_endpoint()
    p = OptimadeProvider(endpoint=ep)
    assert p.id == "mp"


def test_optimade_parse_response():
    """Symmetry comes from the fields the OPTIMADE spec actually defines.

    `space_group_symbol` is NOT in the specification and no live provider
    returns it (MP's OPTIMADE endpoint returns space_group_it_number /
    space_group_symbol_hall / space_group_symbol_hermann_mauguin), so reading
    it made symmetry None for essentially every federation hit."""
    from app.tools.search_engine.providers.optimade import OptimadeProvider
    ep = _make_endpoint()
    p = OptimadeProvider(endpoint=ep)

    entry = {
        "id": "mp-1234",
        "attributes": {
            "chemical_formula_descriptive": "Fe2O3",
            "chemical_formula_reduced": "Fe2O3",
            "elements": ["Fe", "O"],
            "nelements": 2,
            "space_group_it_number": 167,
            "space_group_symbol_hermann_mauguin": "R-3c",
            "lattice_vectors": [[5.0, 0, 0], [0, 5.0, 0], [0, 0, 13.7]],
        },
    }
    material = p._parse_entry(entry)
    assert material.id == "mp-1234"
    assert material.formula == "Fe2O3"
    assert material.elements == ["Fe", "O"]
    assert material.n_elements == 2
    assert material.space_group.value == "R-3c"
    assert material.sources == ["mp"]
    # Identity keys on the notation-free IT number, not the display symbol.
    assert material.identity.attributes["space_group"] == "167"


def test_optimade_nonspec_space_group_symbol_field_is_ignored():
    """The made-up field must not silently come back as a symmetry source."""
    from app.tools.search_engine.providers.optimade import OptimadeProvider
    p = OptimadeProvider(endpoint=_make_endpoint())
    entry = {
        "id": "x-1",
        "attributes": {
            "chemical_formula_reduced": "Fe2O3",
            "elements": ["Fe", "O"],
            "nelements": 2,
            "space_group_symbol": "R-3c",  # not an OPTIMADE field
        },
    }
    material = p._parse_entry(entry)
    assert material.space_group is None
    assert "space_group" not in material.identity.attributes


def test_optimade_parse_entry_missing_fields():
    """No symmetry data: the identity must OMIT space_group (no 'unknown'
    sentinel -- that sentinel merged every polymorph of a formula into one
    fabricated record)."""
    from app.tools.search_engine.providers.optimade import OptimadeProvider
    ep = _make_endpoint()
    p = OptimadeProvider(endpoint=ep)
    entry = {
        "id": "cod-12345",
        "attributes": {
            "chemical_formula_descriptive": "SiO2",
            "elements": ["O", "Si"],
            "nelements": 2,
        },
    }
    material = p._parse_entry(entry)
    assert material.formula == "SiO2"
    assert material.space_group is None
    assert "space_group" not in material.identity.attributes
    assert "unknown" not in material.identity.attributes.values()


def test_optimade_identity_formula_is_canonical_reduced():
    """Identity prefers chemical_formula_reduced, canonicalised: a provider
    sending descriptive "TiO2" and one sending reduced "O2Ti" must produce
    the SAME identity formula."""
    from app.tools.search_engine.providers.optimade import OptimadeProvider
    p = OptimadeProvider(endpoint=_make_endpoint())

    descriptive_only = p._parse_entry({
        "id": "a-1",
        "attributes": {
            "chemical_formula_descriptive": "TiO2",
            "elements": ["O", "Ti"],
            "nelements": 2,
        },
    })
    reduced_form = p._parse_entry({
        "id": "b-1",
        "attributes": {
            "chemical_formula_reduced": "O2Ti",
            "elements": ["O", "Ti"],
            "nelements": 2,
        },
    })
    assert descriptive_only.identity.attributes["formula"] == "O2Ti"
    assert reduced_form.identity.attributes["formula"] == "O2Ti"
    # Display formula keeps the provider's spelling.
    assert descriptive_only.formula == "TiO2"


def test_optimade_parse_provider_specific_fields():
    from app.tools.search_engine.providers.optimade import OptimadeProvider
    ep = _make_endpoint(pid="oqmd")
    p = OptimadeProvider(endpoint=ep)
    entry = {
        "id": "12345",
        "attributes": {
            "chemical_formula_descriptive": "Fe2O3",
            "elements": ["Fe", "O"],
            "nelements": 2,
            "_oqmd_band_gap": 2.1,
            "_oqmd_formation_energy": -0.85,
        },
    }
    material = p._parse_entry(entry)
    assert material.id == "12345"
    assert "_oqmd_band_gap" in material.extra_properties
    assert material.extra_properties["_oqmd_band_gap"].value == 2.1


def test_optimade_describe_query_is_the_wire_filter():
    """describe_query reports exactly the OPTIMADE filter search() sends."""
    from app.tools.search_engine.providers.optimade import OptimadeProvider
    from app.tools.search_engine.translator import QueryTranslator

    p = OptimadeProvider(endpoint=_make_endpoint())
    q = MaterialSearchQuery(elements=["Fe", "O"])
    assert p.describe_query(q) == QueryTranslator.to_optimade(q)
    assert 'elements HAS ALL "Fe","O"' in p.describe_query(q)
