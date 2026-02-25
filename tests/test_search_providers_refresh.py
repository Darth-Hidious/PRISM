"""Tests for app.search.providers.refresh — registry refresh mechanism."""
import json
from unittest.mock import AsyncMock, patch

import pytest


def test_parse_optimade_providers_response():
    from app.search.providers.refresh import parse_providers_response
    response = {
        "data": [
            {
                "id": "mp",
                "attributes": {
                    "name": "The Materials Project",
                    "base_url": "https://providers.optimade.org/index-metadbs/mp",
                    "homepage_url": "https://materialsproject.org",
                    "link_type": "external",
                },
            },
            {
                "id": "newdb",
                "attributes": {
                    "name": "Brand New Database",
                    "base_url": "https://newdb.example.com/optimade",
                    "homepage_url": "https://newdb.example.com",
                    "link_type": "external",
                },
            },
        ]
    }
    providers = parse_providers_response(response)
    assert len(providers) == 2
    assert providers[1]["id"] == "newdb"
    assert providers[1]["base_url"] == "https://newdb.example.com/optimade"


def test_parse_providers_response_skips_meta_entries():
    """Meta entries like exmpl, optimade, optimake should be filtered out."""
    from app.search.providers.refresh import parse_providers_response
    response = {
        "data": [
            {"id": "exmpl", "attributes": {"name": "Example", "base_url": "https://example.com"}},
            {"id": "optimade", "attributes": {"name": "OPTIMADE", "base_url": "https://optimade.org"}},
            {"id": "mp", "attributes": {"name": "MP", "base_url": "https://mp.org"}},
        ]
    }
    providers = parse_providers_response(response)
    assert len(providers) == 1
    assert providers[0]["id"] == "mp"


def test_merge_registries_adds_new():
    from app.search.providers.refresh import merge_registries
    existing = [{"id": "mp", "name": "MP", "tier": 1}]
    discovered = [
        {"id": "mp", "name": "MP Updated", "base_url": "https://mp.org"},
        {"id": "newdb", "name": "New DB", "base_url": "https://new.org"},
    ]
    merged, changes = merge_registries(existing, discovered)
    ids = {p["id"] for p in merged}
    assert "newdb" in ids
    assert any(c["type"] == "new_provider" for c in changes)


def test_merge_registries_updates_url():
    from app.search.providers.refresh import merge_registries
    existing = [{"id": "nmd", "name": "NOMAD", "base_url": "https://old.url"}]
    discovered = [{"id": "nmd", "name": "NOMAD", "base_url": "https://new.url"}]
    merged, changes = merge_registries(existing, discovered)
    nmd = next(p for p in merged if p["id"] == "nmd")
    assert nmd["base_url"] == "https://new.url"
    assert any(c["type"] == "url_changed" for c in changes)


def test_merge_registries_activates_namespace():
    from app.search.providers.refresh import merge_registries
    existing = [{"id": "ccdc", "name": "CCDC", "status": "namespace_reserved", "base_url": None}]
    discovered = [{"id": "ccdc", "name": "CCDC", "base_url": "https://ccdc.example.com/optimade"}]
    merged, changes = merge_registries(existing, discovered)
    ccdc = next(p for p in merged if p["id"] == "ccdc")
    assert ccdc["base_url"] == "https://ccdc.example.com/optimade"
    assert any(c["type"] == "namespace_activated" for c in changes)


def test_merge_registries_preserves_local_overrides():
    from app.search.providers.refresh import merge_registries
    existing = [{"id": "mp", "name": "MP", "tier": 1, "enabled": False, "_user_override": True}]
    discovered = [{"id": "mp", "name": "MP", "base_url": "https://mp.org"}]
    merged, _ = merge_registries(existing, discovered)
    mp = next(p for p in merged if p["id"] == "mp")
    assert mp["enabled"] is False
