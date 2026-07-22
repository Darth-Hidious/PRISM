from unittest.mock import patch, MagicMock
import pytest

from app.tools.search_engine.query import MaterialSearchQuery, PropertyRange
from app.tools.search_engine.providers.endpoint import (
    ProviderEndpoint,
    AuthConfig,
    BehaviorConfig,
    CapabilitiesConfig,
)


def _make_mp_endpoint():
    return ProviderEndpoint(
        id="mp_native",
        name="Materials Project (Native)",
        base_url="https://api.materialsproject.org",
        api_type="mp_native",
        tier=1,
        enabled=True,
        auth=AuthConfig(required=True, auth_type="api_key", auth_env_var="MP_API_KEY"),
        behavior=BehaviorConfig(timeout_ms=10000),
        capabilities=CapabilitiesConfig(
            filterable_fields=["elements", "formula", "band_gap", "formation_energy"],
            returned_properties=["formula", "band_gap", "formation_energy"],
        ),
    )


def test_mp_provider_creates():
    from app.tools.search_engine.providers.materials_project import (
        MaterialsProjectProvider,
    )

    ep = _make_mp_endpoint()
    p = MaterialsProjectProvider(endpoint=ep)
    assert p.id == "mp_native"


def test_mp_provider_parse_doc():
    from app.tools.search_engine.providers.materials_project import (
        MaterialsProjectProvider,
    )

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


def test_mp_provider_keyless_unauthenticated_raises():
    """C1 honesty: no local key AND no platform auth = MP was NOT queried.
    That must surface as a provider failure (raise → breaker/query_log), never
    as an empty result set ("nothing found"). The old behavior (return []) is
    exactly the masked-outage defect."""
    from app.tools.search_engine.providers.materials_project import (
        MaterialsProjectProvider,
    )

    ep = _make_mp_endpoint()
    p = MaterialsProjectProvider(endpoint=ep)
    with (
        patch.dict("os.environ", {}, clear=True),
        patch("pathlib.Path.exists", return_value=False),
    ):
        import asyncio

        with pytest.raises(RuntimeError, match="proxy"):
            asyncio.run(p.search(MaterialSearchQuery(elements=["Fe"])))


def test_mp_provider_proxy_error_raises():
    """C1: an explicit proxy error payload must raise, not return []."""
    import asyncio

    from app.tools.search_engine.providers.materials_project import (
        MaterialsProjectProvider,
    )

    ep = _make_mp_endpoint()
    p = MaterialsProjectProvider(endpoint=ep)
    with (
        patch.dict("os.environ", {}, clear=True),
        patch(
            "app.tools.data._query_materials_project",
            return_value={"error": "MP proxy 503: upstream outage"},
        ),
    ):
        with pytest.raises(RuntimeError, match="503"):
            asyncio.run(p.search(MaterialSearchQuery(formula="Fe2O3")))


def test_mp_provider_proxy_empty_results_is_honest_empty():
    """Queried successfully + genuinely nothing found → [] (NOT an error)."""
    import asyncio

    from app.tools.search_engine.providers.materials_project import (
        MaterialsProjectProvider,
    )

    ep = _make_mp_endpoint()
    p = MaterialsProjectProvider(endpoint=ep)
    with (
        patch.dict("os.environ", {}, clear=True),
        patch(
            "app.tools.data._query_materials_project",
            return_value={"results": [], "count": 0, "source": "marc27_platform_proxy"},
        ),
    ):
        results = asyncio.run(p.search(MaterialSearchQuery(formula="Xx99Zz")))
        assert results == []


def test_mp_provider_proxy_success_parses_materials():
    import asyncio

    from app.tools.search_engine.providers.materials_project import (
        MaterialsProjectProvider,
    )

    ep = _make_mp_endpoint()
    p = MaterialsProjectProvider(endpoint=ep)
    fake = {
        "results": [
            {
                "material_id": "mp-1234",
                "formula_pretty": "Fe2O3",
                "elements": ["Fe", "O"],
                "nelements": 2,
                "band_gap": 2.2,
                "formation_energy_per_atom": -0.85,
                "energy_above_hull": 0.0,
                "symmetry": {"symbol": "R-3c"},
            }
        ],
        "count": 1,
        "source": "marc27_platform_proxy",
    }
    with (
        patch.dict("os.environ", {}, clear=True),
        patch("app.tools.data._query_materials_project", return_value=fake),
    ):
        results = asyncio.run(p.search(MaterialSearchQuery(formula="Fe2O3")))
    assert len(results) == 1
    assert results[0].id == "mp-1234"
    assert results[0].band_gap.value == 2.2
