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


def test_mp_describe_query_keyed_path_is_mp_native_not_optimade():
    """Audit truthfulness, MPRester branch: with a local MP_API_KEY the
    provider describes its own MP-native kwargs, not the OPTIMADE filter
    string the engine used to record."""
    from app.tools.search_engine.providers.materials_project import (
        MaterialsProjectProvider,
    )
    from app.tools.search_engine.translator import QueryTranslator

    p = MaterialsProjectProvider(endpoint=_make_mp_endpoint())
    q = MaterialSearchQuery(elements=["Fe", "O"], band_gap=PropertyRange(min=1.0, max=3.0))
    with patch.dict("os.environ", {"MP_API_KEY": "local-key"}):
        described = p.describe_query(q)
    assert described == str(QueryTranslator.to_mp_kwargs(q))
    assert described != QueryTranslator.to_optimade(q)
    assert "HAS ALL" not in described  # no OPTIMADE syntax


def test_mp_describe_query_keyless_path_is_honest_about_the_proxy():
    """Audit truthfulness, proxy branch (the DEFAULT install, no MP_API_KEY):
    a formula query records the formula pull; an elements-only query records
    the refusal that search() will raise -- NOT a fabricated narrowed pull
    (the old behavior degraded elements=["Fe","O"] to formula="Fe")."""
    from app.tools.search_engine.providers.materials_project import (
        MaterialsProjectProvider,
    )
    from app.tools.search_engine.translator import QueryTranslator

    p = MaterialsProjectProvider(endpoint=_make_mp_endpoint())

    formula_q = MaterialSearchQuery(formula="Fe2O3", band_gap=PropertyRange(min=1.0, max=3.0))
    with patch.dict("os.environ", {}, clear=True):
        described = p.describe_query(formula_q)
    assert 'formula="Fe2O3"' in described
    assert "platform_proxy" in described
    assert described != str(QueryTranslator.to_mp_kwargs(formula_q))
    assert "band_gap" not in described  # never sent by the proxy path

    elements_q = MaterialSearchQuery(elements=["Fe", "O"])
    with patch.dict("os.environ", {}, clear=True):
        described = p.describe_query(elements_q)
    assert "cannot serve elements-only queries" in described
    assert 'formula="Fe"' not in described  # the old fabricated narrowing

    # A query the proxy path cannot serve at all must say so, not pretend.
    empty_q = MaterialSearchQuery(n_elements=PropertyRange(min=2, max=3))
    with patch.dict("os.environ", {}, clear=True):
        assert "no request will be issued" in p.describe_query(empty_q)


def test_mp_proxy_elements_only_raises_instead_of_substituting():
    """elements=["Ni","Al"] used to become formula="Ni" -- pure nickel
    labelled success. The proxy path must refuse honestly instead: the raise
    reaches the engine, which records a provider failure in the query log."""
    import asyncio

    from app.tools.search_engine.providers.materials_project import (
        MaterialsProjectProvider,
    )

    p = MaterialsProjectProvider(endpoint=_make_mp_endpoint())
    called = MagicMock()
    with (
        patch.dict("os.environ", {}, clear=True),
        patch("app.tools.data._query_materials_project", called),
    ):
        with pytest.raises(RuntimeError, match="elements-only"):
            asyncio.run(p.search(MaterialSearchQuery(elements=["Ni", "Al"])))
    called.assert_not_called()  # no substituted formula pull went out


def test_mp_parse_doc_identity_prefers_spacegroup_number():
    """The identity discriminator is the IT number when MP supplies it, so it
    keys identically with OPTIMADE's space_group_it_number."""
    from app.tools.search_engine.providers.materials_project import (
        MaterialsProjectProvider,
    )

    p = MaterialsProjectProvider(endpoint=_make_mp_endpoint())
    doc = {
        "material_id": "mp-2657",
        "formula_pretty": "TiO2",
        "elements": ["O", "Ti"],
        "nelements": 2,
        "symmetry": {"symbol": "P4_2/mnm", "number": 136},
    }
    material = p._parse_doc(doc)
    assert material.identity.attributes["space_group"] == "136"
    assert material.space_group.value == "P4_2/mnm"  # display keeps the symbol
    # Identity formula is canonicalised like the OPTIMADE adapter's.
    assert material.identity.attributes["formula"] == "O2Ti"
    assert material.formula == "TiO2"


def test_mp_parse_doc_missing_symmetry_omits_identity_space_group():
    """No symmetry: OMIT the attribute (no 'unknown' sentinel that merged
    every polymorph of a formula into one fabricated record)."""
    from app.tools.search_engine.providers.materials_project import (
        MaterialsProjectProvider,
    )

    p = MaterialsProjectProvider(endpoint=_make_mp_endpoint())
    doc = {
        "material_id": "mp-9999",
        "formula_pretty": "TiO2",
        "elements": ["O", "Ti"],
        "nelements": 2,
    }
    material = p._parse_doc(doc)
    assert material.space_group is None
    assert "space_group" not in material.identity.attributes
    assert "unknown" not in material.identity.attributes.values()


def test_mp_parse_doc_reads_vrh_bulk_modulus():
    """bulk_modulus is declared filterable/returned for mp_native; the parsed
    material must actually carry it (VRH average, GPa) or every hit fails the
    client-side range filter and the capability guarantees zero results."""
    from app.tools.search_engine.providers.materials_project import (
        MaterialsProjectProvider,
    )

    p = MaterialsProjectProvider(endpoint=_make_mp_endpoint())
    doc = {
        "material_id": "mp-149",
        "formula_pretty": "Si",
        "elements": ["Si"],
        "nelements": 1,
        "bulk_modulus": {"voigt": 88.9, "reuss": 87.8, "vrh": 88.4},
    }
    material = p._parse_doc(doc)
    assert material.bulk_modulus.value == 88.4
    assert material.bulk_modulus.unit == "GPa"
