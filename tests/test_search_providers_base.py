import pytest


def test_provider_capabilities():
    from app.search.providers.base import ProviderCapabilities
    cap = ProviderCapabilities(
        filterable_fields={"elements", "formula"},
        returned_properties={"formula", "space_group"},
    )
    assert "elements" in cap.filterable_fields


def test_provider_capabilities_can_handle():
    from app.search.providers.base import ProviderCapabilities
    from app.search.query import MaterialSearchQuery, PropertyRange
    cap = ProviderCapabilities(
        filterable_fields={"elements", "formula", "nelements"},
        returned_properties={"formula"},
    )
    q1 = MaterialSearchQuery(elements=["Fe", "O"])
    assert cap.can_handle(q1) is True
    q2 = MaterialSearchQuery(band_gap=PropertyRange(min=1.0))
    assert cap.can_handle(q2) is False


def test_provider_endpoint_from_registry_json():
    from app.search.providers.endpoint import ProviderEndpoint, load_registry
    endpoints = load_registry()
    assert len(endpoints) > 0
    mp = next(e for e in endpoints if e.id == "mp")
    assert "materialsproject" in mp.base_url
    assert mp.auth.required is False


def test_provider_endpoint_mpds_requires_auth():
    from app.search.providers.endpoint import load_registry
    endpoints = load_registry()
    mpds = next(e for e in endpoints if e.id == "mpds")
    assert mpds.auth.required is True
    assert mpds.auth.auth_header == "Key"


def test_provider_endpoint_namespace_placeholders():
    from app.search.providers.endpoint import load_registry
    endpoints = load_registry()
    ids = {e.id for e in endpoints}
    assert "ccdc" in ids
    assert "aiida" in ids
    assert "pcod" in ids
    ccdc = next(e for e in endpoints if e.id == "ccdc")
    assert ccdc.enabled is False
    assert ccdc.status == "namespace_reserved"


def test_load_registry_includes_all_tiers():
    from app.search.providers.endpoint import load_registry
    endpoints = load_registry()
    tiers = {e.tier for e in endpoints}
    assert 1 in tiers
    assert 2 in tiers
