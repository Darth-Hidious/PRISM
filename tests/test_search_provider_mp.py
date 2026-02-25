from unittest.mock import patch, MagicMock
import pytest

from app.search.query import MaterialSearchQuery, PropertyRange
from app.search.providers.endpoint import ProviderEndpoint, AuthConfig, BehaviorConfig, CapabilitiesConfig


def _make_mp_endpoint():
    return ProviderEndpoint(
        id="mp_native", name="Materials Project (Native)",
        base_url="https://api.materialsproject.org",
        api_type="mp_native", tier=1, enabled=True,
        auth=AuthConfig(required=True, auth_type="api_key", auth_env_var="MP_API_KEY"),
        behavior=BehaviorConfig(timeout_ms=10000),
        capabilities=CapabilitiesConfig(
            filterable_fields=["elements", "formula", "band_gap", "formation_energy"],
            returned_properties=["formula", "band_gap", "formation_energy"],
        ),
    )


def test_mp_provider_creates():
    from app.search.providers.materials_project import MaterialsProjectProvider
    ep = _make_mp_endpoint()
    p = MaterialsProjectProvider(endpoint=ep)
    assert p.id == "mp_native"


def test_mp_provider_parse_doc():
    from app.search.providers.materials_project import MaterialsProjectProvider
    ep = _make_mp_endpoint()
    p = MaterialsProjectProvider(endpoint=ep)
    doc = {
        "material_id": "mp-1234",
        "formula_pretty": "Fe2O3",
        "elements": ["Fe", "O"],
        "nelements": 2,
        "band_gap": 2.2,
        "formation_energy_per_atom": -0.85,
        "energy_above_hull": 0.0,
        "symmetry": {"symbol": "R-3c"},
    }
    material = p._parse_doc(doc)
    assert material.id == "mp-1234"
    assert material.band_gap.value == 2.2
    assert material.band_gap.source == "mp_native"
    assert material.formation_energy.value == -0.85


def test_mp_provider_skips_without_api_key():
    from app.search.providers.materials_project import MaterialsProjectProvider
    ep = _make_mp_endpoint()
    p = MaterialsProjectProvider(endpoint=ep)
    with patch.dict("os.environ", {}, clear=True):
        import asyncio
        results = asyncio.run(p.search(MaterialSearchQuery(elements=["Fe"])))
        assert results == []
