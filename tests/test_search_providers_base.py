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


def test_build_registry_returns_providers(tmp_path):
    """build_registry returns working providers from cache + overrides."""
    from app.search.providers.registry import build_registry
    from app.search.providers.discovery import save_cache

    endpoints = [
        {"id": "mp", "name": "MP", "base_url": "https://optimade.materialsproject.org", "parent": "mp"},
    ]
    cache_path = tmp_path / "cache.json"
    save_cache(endpoints, path=cache_path)

    reg = build_registry(cache_path=cache_path, skip_network=True)
    providers = reg.get_all()
    assert len(providers) > 0
    ids = {p.id for p in providers}
    assert "mp" in ids


def test_marketplace_mpds_auth_config():
    """MPDS auth config is present in catalog.json."""
    import json
    from pathlib import Path
    catalog_path = Path(__file__).parent.parent / "app" / "plugins" / "catalog.json"
    data = json.loads(catalog_path.read_text())
    mpds = data["plugins"]["mpds"]
    assert mpds["auth"]["required"] is True
    assert mpds["auth"]["auth_type"] == "api_key"


def test_marketplace_mp_native_has_auth():
    """MP native provider has auth config in catalog.json."""
    import json
    from pathlib import Path
    catalog_path = Path(__file__).parent.parent / "app" / "plugins" / "catalog.json"
    data = json.loads(catalog_path.read_text())
    mp_native = data["plugins"]["mp_native"]
    assert mp_native["auth"]["required"] is True
    assert mp_native["base_url"] == "https://api.materialsproject.org"


def test_provider_endpoint_model():
    """ProviderEndpoint Pydantic model validates correctly."""
    from app.search.providers.endpoint import ProviderEndpoint
    ep = ProviderEndpoint(
        id="test", name="Test Provider", base_url="https://test.org",
        tier=2, enabled=True,
    )
    assert ep.id == "test"
    assert ep.api_type == "optimade"
    assert ep.behavior.timeout_ms == 5000
