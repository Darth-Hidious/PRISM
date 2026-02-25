import pytest
from unittest.mock import patch, MagicMock

from app.search.query import MaterialSearchQuery
from app.search.providers.endpoint import ProviderEndpoint, AuthConfig, BehaviorConfig, CapabilitiesConfig


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
    from app.search.providers.optimade import OptimadeProvider
    ep = _make_endpoint()
    p = OptimadeProvider(endpoint=ep)
    assert p.id == "mp"


def test_optimade_parse_response():
    from app.search.providers.optimade import OptimadeProvider
    ep = _make_endpoint()
    p = OptimadeProvider(endpoint=ep)

    entry = {
        "id": "mp-1234",
        "attributes": {
            "chemical_formula_descriptive": "Fe2O3",
            "elements": ["Fe", "O"],
            "nelements": 2,
            "space_group_symbol": "R-3c",
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


def test_optimade_parse_entry_missing_fields():
    from app.search.providers.optimade import OptimadeProvider
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


def test_optimade_parse_provider_specific_fields():
    from app.search.providers.optimade import OptimadeProvider
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
