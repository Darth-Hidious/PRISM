"""Tests for OPTIMADE 2-hop provider discovery."""
import json
import time
from unittest.mock import AsyncMock, patch

import pytest


def _make_providers_response(providers):
    """Build a providers.optimade.org/v1/links style response."""
    return {
        "data": [
            {
                "id": pid,
                "attributes": {
                    "name": name,
                    "base_url": index_url,
                    "homepage": f"https://{pid}.example.com",
                    "link_type": "external",
                },
            }
            for pid, name, index_url in providers
        ]
    }


def _make_links_response(children):
    """Build an index-metadb /v1/links style response."""
    return {
        "data": [
            {
                "id": cid,
                "attributes": {
                    "name": name,
                    "base_url": base_url,
                    "link_type": "child",
                },
            }
            for cid, name, base_url in children
        ]
    }


# ------------------------------------------------------------------
# parse_index_response
# ------------------------------------------------------------------

def test_parse_index_response_extracts_providers():
    from app.search.providers.discovery import parse_index_response
    resp = _make_providers_response([
        ("mp", "Materials Project", "https://index.mp.org"),
        ("cod", "COD", "https://index.cod.org"),
    ])
    providers = parse_index_response(resp)
    assert len(providers) == 2
    assert providers[0]["id"] == "mp"
    assert providers[0]["index_url"] == "https://index.mp.org"


def test_parse_index_response_skips_meta():
    from app.search.providers.discovery import parse_index_response
    resp = _make_providers_response([
        ("exmpl", "Example", "https://example.com"),
        ("optimade", "OPTIMADE", "https://optimade.org"),
        ("mp", "MP", "https://index.mp.org"),
    ])
    providers = parse_index_response(resp)
    assert len(providers) == 1
    assert providers[0]["id"] == "mp"


def test_parse_index_response_skips_null_url():
    from app.search.providers.discovery import parse_index_response
    resp = {
        "data": [
            {"id": "aiida", "attributes": {"name": "AiiDA", "base_url": None, "link_type": "external"}},
        ]
    }
    providers = parse_index_response(resp)
    assert len(providers) == 0


# ------------------------------------------------------------------
# parse_links_response
# ------------------------------------------------------------------

def test_parse_links_response_extracts_children():
    from app.search.providers.discovery import parse_links_response
    resp = _make_links_response([
        ("pbe", "Alexandria PBE", "https://alexandria.rub.de/pbe"),
        ("pbesol", "Alexandria PBEsol", "https://alexandria.rub.de/pbesol"),
    ])
    children = parse_links_response(resp)
    assert len(children) == 2
    assert children[0]["id"] == "pbe"
    assert children[0]["base_url"] == "https://alexandria.rub.de/pbe"


def test_parse_links_response_ignores_non_child():
    """Only link_type=child entries are real databases."""
    resp = {
        "data": [
            {"id": "idx", "attributes": {"name": "Index", "base_url": "https://idx.org", "link_type": "root"}},
            {"id": "db1", "attributes": {"name": "DB1", "base_url": "https://db1.org", "link_type": "child"}},
        ]
    }
    from app.search.providers.discovery import parse_links_response
    children = parse_links_response(resp)
    assert len(children) == 1
    assert children[0]["id"] == "db1"


# ------------------------------------------------------------------
# Cache: save, load, freshness
# ------------------------------------------------------------------

def test_save_and_load_cache(tmp_path):
    from app.search.providers.discovery import save_cache, load_cache
    endpoints = [{"id": "mp", "name": "MP", "base_url": "https://mp.org"}]
    cache_path = tmp_path / "cache.json"
    save_cache(endpoints, path=cache_path)
    loaded = load_cache(path=cache_path)
    assert loaded is not None
    assert loaded["endpoints"] == endpoints
    assert "cached_at" in loaded


def test_load_cache_returns_none_if_missing(tmp_path):
    from app.search.providers.discovery import load_cache
    assert load_cache(path=tmp_path / "nope.json") is None


def test_is_cache_fresh():
    from app.search.providers.discovery import is_cache_fresh
    fresh = {"cached_at": time.time()}
    assert is_cache_fresh(fresh) is True
    stale = {"cached_at": time.time() - 86400 * 10}
    assert is_cache_fresh(stale) is False
