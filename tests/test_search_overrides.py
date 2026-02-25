"""Tests for loading and applying provider overrides."""
import json
from pathlib import Path


def test_overrides_file_is_valid_json():
    path = Path(__file__).parent.parent / "app" / "search" / "providers" / "provider_overrides.json"
    data = json.loads(path.read_text())
    assert "overrides" in data
    assert "defaults" in data
    assert "fallback_index_urls" in data


def test_overrides_have_no_base_url_for_optimade_providers():
    """OPTIMADE providers get their base_url from discovery, not overrides."""
    path = Path(__file__).parent.parent / "app" / "search" / "providers" / "provider_overrides.json"
    data = json.loads(path.read_text())
    for pid, override in data["overrides"].items():
        api_type = override.get("api_type", "optimade")
        if api_type == "optimade":
            assert "base_url" not in override, f"{pid} is optimade but has base_url in overrides"


def test_layer2_overrides_are_optimade_only():
    """Layer 2 overrides should only contain OPTIMADE providers.
    Native/auth-gated providers belong in Layer 3 (marketplace.json)."""
    path = Path(__file__).parent.parent / "app" / "search" / "providers" / "provider_overrides.json"
    data = json.loads(path.read_text())
    for pid, override in data["overrides"].items():
        api_type = override.get("api_type", "optimade")
        assert api_type == "optimade", f"{pid} has api_type={api_type} — should be in marketplace.json"


def test_marketplace_native_providers_have_base_url():
    """Native API providers in marketplace.json MUST have a base_url."""
    path = Path(__file__).parent.parent / "app" / "search" / "marketplace.json"
    data = json.loads(path.read_text())
    for pid, entry in data["providers"].items():
        assert "base_url" in entry, f"marketplace {pid} missing base_url"


def test_apply_overrides_merges_fields():
    from app.search.providers.discovery import apply_overrides
    discovered = [
        {"id": "mp", "name": "Materials Project", "base_url": "https://mp.org"},
        {"id": "cod", "name": "COD", "base_url": "https://cod.org"},
    ]
    overrides = {
        "mp": {"tier": 1, "enabled": True},
    }
    defaults = {"behavior": {"timeout_ms": 10000}}
    result = apply_overrides(discovered, overrides, defaults)
    mp = next(e for e in result if e["id"] == "mp")
    cod = next(e for e in result if e["id"] == "cod")
    assert mp["tier"] == 1
    assert mp["enabled"] is True
    assert mp["base_url"] == "https://mp.org"  # preserved from discovery
    assert cod["behavior"]["timeout_ms"] == 10000  # defaults applied


def test_apply_overrides_adds_native_providers():
    """Native API entries in overrides are injected even if not discovered."""
    from app.search.providers.discovery import apply_overrides
    discovered = [{"id": "mp", "name": "MP", "base_url": "https://mp.org"}]
    overrides = {
        "mp_native": {
            "api_type": "mp_native",
            "name": "Materials Project (Native)",
            "base_url": "https://api.materialsproject.org",
            "tier": 1,
            "enabled": True,
        },
    }
    result = apply_overrides(discovered, overrides, {})
    ids = {e["id"] for e in result}
    assert "mp_native" in ids
