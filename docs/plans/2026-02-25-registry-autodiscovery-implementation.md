# Registry Auto-Discovery Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the hardcoded `provider_registry.json` with auto-discovery from the OPTIMADE consortium, a slim bundled overrides file, and a user overrides layer.

**Architecture:** A `discovery.py` module walks the 2-hop OPTIMADE chain (providers.optimade.org → index-metadbs → child databases), caches results to `~/.prism/cache/discovered_registry.json`, and overlays PRISM-specific config from `provider_overrides.json`. The `ProviderRegistry.from_registry_json()` factory is replaced by `build_registry()` which merges all three layers.

**Tech Stack:** Python 3.11+, httpx (async HTTP), Pydantic v2, existing endpoint.py models

**Design doc:** `docs/plans/2026-02-25-registry-autodiscovery-design.md`

---

## Task 1: Discovery Module — Fetch and Walk the 2-Hop Chain

**Files:**
- Create: `app/search/providers/discovery.py`
- Test: `tests/test_search_discovery.py`

This is the core new module. It fetches the OPTIMADE providers index, follows each provider's `/v1/links` to find child databases, and returns a flat list of discovered endpoints.

**Step 1: Write failing tests**

```python
# tests/test_search_discovery.py
"""Tests for OPTIMADE 2-hop provider discovery."""
import json
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
```

**Step 2: Run tests to verify failure**

Run: `cd /Users/siddharthakovid/Downloads/PRISM && python3 -m pytest tests/test_search_discovery.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'app.search.providers.discovery'`

**Step 3: Implement discovery module**

```python
# app/search/providers/discovery.py
"""OPTIMADE 2-hop provider discovery.

Walks: providers.optimade.org/v1/links → {index}/v1/links → child endpoints.
"""
from __future__ import annotations

import json
import logging
import time
from pathlib import Path

import httpx

logger = logging.getLogger(__name__)

PROVIDERS_INDEX_URL = "https://providers.optimade.org/v1/links"
PROVIDERS_FALLBACK_URL = (
    "https://raw.githubusercontent.com/Materials-Consortia/providers"
    "/master/src/links/v1/providers.json"
)

_SKIP_IDS = frozenset({"exmpl", "optimade", "optimake"})

# Providers whose proxy at providers.optimade.org 404s.
# Map provider_id → working index URL.
FALLBACK_INDEX_URLS: dict[str, str] = {
    "mcloud": "https://www.materialscloud.org/optimade/main/v1/links",
}

DEFAULT_CACHE_PATH = Path.home() / ".prism" / "cache" / "discovered_registry.json"
CACHE_MAX_AGE_DAYS = 7


# ------------------------------------------------------------------
# Parsers
# ------------------------------------------------------------------

def parse_index_response(response: dict) -> list[dict]:
    """Parse providers.optimade.org/v1/links into a flat list.

    Returns list of {id, name, index_url, homepage}.
    Skips meta entries and providers without a base_url.
    """
    providers: list[dict] = []
    for entry in response.get("data", []):
        pid = entry.get("id", "")
        if pid in _SKIP_IDS:
            continue
        attrs = entry.get("attributes", {})
        index_url = attrs.get("base_url")
        if not index_url:
            continue
        providers.append({
            "id": pid,
            "name": attrs.get("name", pid),
            "index_url": index_url,
            "homepage": attrs.get("homepage", attrs.get("homepage_url", "")),
        })
    return providers


def parse_links_response(response: dict) -> list[dict]:
    """Parse an index-metadb /v1/links response into child databases.

    Returns list of {id, name, base_url}.
    Only includes entries with link_type=child.
    """
    children: list[dict] = []
    for entry in response.get("data", []):
        attrs = entry.get("attributes", {})
        if attrs.get("link_type") != "child":
            continue
        base_url = attrs.get("base_url")
        if not base_url:
            continue
        children.append({
            "id": entry.get("id", ""),
            "name": attrs.get("name", ""),
            "base_url": base_url,
        })
    return children


# ------------------------------------------------------------------
# Discovery
# ------------------------------------------------------------------

async def _fetch_json(client: httpx.AsyncClient, url: str) -> dict | None:
    """Fetch JSON from URL, return None on failure."""
    try:
        resp = await client.get(url)
        resp.raise_for_status()
        return resp.json()
    except Exception as e:
        logger.debug("Fetch failed for %s: %s", url, e)
        return None


async def discover_providers(
    fallback_index_urls: dict[str, str] | None = None,
) -> list[dict]:
    """Walk the OPTIMADE 2-hop chain and return flat list of endpoints.

    Returns list of {id, name, base_url, parent, homepage, discovered_at}.
    """
    fallbacks = fallback_index_urls or FALLBACK_INDEX_URLS
    endpoints: list[dict] = []

    async with httpx.AsyncClient(timeout=10.0) as client:
        # Hop 1: fetch provider index
        data = await _fetch_json(client, PROVIDERS_INDEX_URL)
        if data is None:
            data = await _fetch_json(client, PROVIDERS_FALLBACK_URL)
        if data is None:
            logger.error("Cannot reach OPTIMADE provider index")
            return []

        providers = parse_index_response(data)

        # Hop 2: for each provider, fetch child databases
        for prov in providers:
            pid = prov["id"]
            index_url = prov["index_url"]

            # Try /v1/links on the index-metadb
            links_url = f"{index_url.rstrip('/')}/v1/links"
            links_data = await _fetch_json(client, links_url)

            # If proxy 404s, try fallback
            if links_data is None and pid in fallbacks:
                links_data = await _fetch_json(client, fallbacks[pid])

            if links_data is None:
                logger.debug("Skipping %s — no links response", pid)
                continue

            children = parse_links_response(links_data)
            if children:
                for child in children:
                    endpoints.append({
                        "id": f"{pid}.{child['id']}" if len(children) > 1 else pid,
                        "name": child["name"] or prov["name"],
                        "base_url": child["base_url"],
                        "parent": pid,
                        "homepage": prov.get("homepage", ""),
                        "discovered_at": time.time(),
                    })
            else:
                # Single-endpoint provider — no children listed
                # The index itself might serve structures
                endpoints.append({
                    "id": pid,
                    "name": prov["name"],
                    "base_url": index_url,
                    "parent": pid,
                    "homepage": prov.get("homepage", ""),
                    "discovered_at": time.time(),
                })

    return endpoints


# ------------------------------------------------------------------
# Cache
# ------------------------------------------------------------------

def load_cache(path: Path | None = None) -> dict | None:
    """Load discovery cache. Returns None if missing or corrupt."""
    p = path or DEFAULT_CACHE_PATH
    if not p.exists():
        return None
    try:
        return json.loads(p.read_text())
    except Exception:
        return None


def save_cache(endpoints: list[dict], path: Path | None = None) -> None:
    """Save discovered endpoints to cache."""
    p = path or DEFAULT_CACHE_PATH
    p.parent.mkdir(parents=True, exist_ok=True)
    data = {
        "version": "2.0.0",
        "cached_at": time.time(),
        "endpoints": endpoints,
    }
    p.write_text(json.dumps(data, indent=2))


def is_cache_fresh(cache: dict, max_age_days: float = CACHE_MAX_AGE_DAYS) -> bool:
    """Check if cache is within max_age_days."""
    cached_at = cache.get("cached_at", 0)
    age_days = (time.time() - cached_at) / 86400
    return age_days < max_age_days
```

**Step 4: Run tests to verify they pass**

Run: `cd /Users/siddharthakovid/Downloads/PRISM && python3 -m pytest tests/test_search_discovery.py -v`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add app/search/providers/discovery.py tests/test_search_discovery.py
git commit -m "feat(search): add OPTIMADE 2-hop discovery module with cache"
```

---

## Task 2: Discovery Cache Tests — Load, Save, Freshness

**Files:**
- Modify: `tests/test_search_discovery.py` (add tests)
- (Implementation already in Task 1's `discovery.py`)

**Step 1: Add cache tests**

Append to `tests/test_search_discovery.py`:

```python
import time


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
```

**Step 2: Run tests**

Run: `cd /Users/siddharthakovid/Downloads/PRISM && python3 -m pytest tests/test_search_discovery.py -v`
Expected: ALL PASS (8 total)

**Step 3: Commit**

```bash
git add tests/test_search_discovery.py
git commit -m "test(search): add cache load/save/freshness tests"
```

---

## Task 3: Bundled Overrides File

**Files:**
- Create: `app/search/providers/provider_overrides.json`
- Test: `tests/test_search_overrides.py`

This replaces the 1113-line `provider_registry.json`. Only PRISM-specific metadata — NO base_url entries except for native API providers.

**Step 1: Write failing tests**

```python
# tests/test_search_overrides.py
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


def test_native_providers_have_base_url():
    """Native API providers (mp_native etc) MUST have a base_url."""
    path = Path(__file__).parent.parent / "app" / "search" / "providers" / "provider_overrides.json"
    data = json.loads(path.read_text())
    for pid, override in data["overrides"].items():
        api_type = override.get("api_type", "optimade")
        if api_type != "optimade":
            assert "base_url" in override, f"{pid} is native but missing base_url"


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
```

**Step 2: Run tests to verify failure**

Run: `cd /Users/siddharthakovid/Downloads/PRISM && python3 -m pytest tests/test_search_overrides.py -v`
Expected: FAIL

**Step 3: Create the overrides file**

Create `app/search/providers/provider_overrides.json` — extracted from current `provider_registry.json`, keeping only tiers, capabilities, auth, quirks. NO base_url on OPTIMADE entries.

```json
{
  "_meta": {
    "version": "2.0.0",
    "description": "PRISM-specific overrides. OPTIMADE base_urls come from auto-discovery, not here."
  },

  "fallback_index_urls": {
    "mcloud": "https://www.materialscloud.org/optimade/main/v1/links"
  },

  "defaults": {
    "api_type": "optimade",
    "api_version": "v1",
    "tier": 3,
    "enabled": true,
    "behavior": {
      "timeout_ms": 10000,
      "max_results": 1000,
      "use_https": true
    },
    "capabilities": {
      "filterable_fields": ["elements", "formula", "nelements", "space_group"],
      "returned_properties": ["formula", "elements", "nelements", "space_group", "lattice_vectors", "cartesian_site_positions", "species", "species_at_sites"],
      "supports_pagination": true
    }
  },

  "overrides": {
    "nmd": {
      "tier": 1,
      "reliability": { "known_quirks": [] }
    },
    "mpdd": {
      "tier": 1,
      "behavior": { "use_https": false },
      "capabilities": {
        "provider_specific_fields": [
          "_mpdd_crystal_system", "_mpdd_sipfenn_formation_energy", "_mpdd_sipfenn_stability"
        ]
      },
      "reliability": {
        "validation_score": "41/46",
        "known_quirks": ["Uses HTTP not HTTPS", "Timeout issues on large queries"]
      }
    },
    "alexandria.alexandria-pbe": {
      "tier": 1,
      "capabilities": {
        "provider_specific_fields": [
          "_alexandria_formation_energy_per_atom", "_alexandria_band_gap",
          "_alexandria_hull_distance"
        ]
      },
      "reliability": { "validation_score": "57/57" }
    },
    "alexandria.alexandria-pbesol": {
      "tier": 1,
      "capabilities": {
        "provider_specific_fields": [
          "_alexandria_formation_energy_per_atom", "_alexandria_band_gap",
          "_alexandria_scan_total_energy"
        ]
      },
      "reliability": { "validation_score": "57/57" }
    },
    "oqmd": {
      "tier": 1,
      "behavior": { "timeout_ms": 15000, "max_results": 500 },
      "capabilities": {
        "provider_specific_fields": [
          "_oqmd_band_gap", "_oqmd_formation_energy", "_oqmd_stability"
        ]
      },
      "reliability": {
        "validation_score": "28/37",
        "known_quirks": ["Returns IDs as integers", "Very slow on complex queries"]
      }
    },
    "cod": {
      "tier": 1,
      "data_type": "experimental",
      "reliability": {
        "validation_score": "57/61",
        "known_quirks": ["Mixed occupancy species handling"]
      }
    },
    "mp": {
      "tier": 1,
      "capabilities": {
        "provider_specific_fields": ["_mp_chemical_system", "_mp_stability"]
      },
      "reliability": {
        "validation_score": "55/56",
        "known_quirks": ["Returns species_at_sites only"]
      }
    },
    "mp_native": {
      "api_type": "mp_native",
      "name": "Materials Project (Native API)",
      "base_url": "https://api.materialsproject.org",
      "tier": 1,
      "enabled": true,
      "auth": {
        "required": true,
        "auth_type": "api_key",
        "auth_header": "X-API-KEY",
        "auth_env_var": "MP_API_KEY",
        "obtain_url": "https://materialsproject.org/dashboard"
      },
      "capabilities": {
        "filterable_fields": ["elements", "formula", "nelements", "space_group", "band_gap", "formation_energy", "energy_above_hull", "bulk_modulus", "is_metal"],
        "returned_properties": ["formula", "elements", "band_gap", "formation_energy", "energy_above_hull", "bulk_modulus", "is_metal", "is_stable"]
      }
    },
    "odbx.odbx-gnome": {
      "tier": 1,
      "data_type": "ml_predicted",
      "capabilities": {
        "provider_specific_fields": [
          "_gnome_bandgap", "_gnome_formation_energy_per_atom"
        ]
      },
      "reliability": { "validation_score": "44/45" }
    },
    "mcloud.mc3d-pbe-v1": {
      "tier": 2,
      "reliability": { "validation_score": "44/50" }
    },
    "mcloud.mc2d": {
      "tier": 2,
      "reliability": { "validation_score": "44/50" }
    },
    "tcod": {
      "tier": 2,
      "reliability": { "validation_score": "46/50" }
    },
    "twodmatpedia": {
      "tier": 2,
      "behavior": { "use_https": false },
      "reliability": { "validation_score": "57/57" }
    },
    "aflow": {
      "tier": 4,
      "enabled": false,
      "reliability": {
        "validation_score": "7/13",
        "known_quirks": ["OPTIMADE endpoint barely functional", "Frequent HTTP 500"]
      }
    },
    "jarvis": {
      "tier": 4,
      "enabled": false,
      "reliability": {
        "known_quirks": ["OPTIMADE endpoint returns 0 structures"]
      }
    },
    "mpds": {
      "tier": 3,
      "enabled": false,
      "auth": {
        "required": true,
        "auth_type": "api_key",
        "auth_env_var": "MPDS_API_KEY"
      },
      "reliability": {
        "known_quirks": ["Subscription required", "Rate limited"]
      }
    }
  }
}
```

**Step 4: Add `apply_overrides` to `discovery.py`**

Append to `app/search/providers/discovery.py`:

```python
# ------------------------------------------------------------------
# Overrides
# ------------------------------------------------------------------

_OVERRIDES_PATH = Path(__file__).parent / "provider_overrides.json"


def load_overrides(path: Path | None = None) -> dict:
    """Load bundled provider overrides."""
    p = path or _OVERRIDES_PATH
    return json.loads(p.read_text())


def apply_overrides(
    discovered: list[dict],
    overrides: dict[str, dict],
    defaults: dict,
) -> list[dict]:
    """Apply PRISM overrides + defaults onto discovered endpoints.

    - Discovered entries get defaults applied first, then per-provider overrides.
    - Native API entries in overrides that aren't in discovered are injected.
    - base_url from discovery is NEVER overwritten by overrides (except native providers).
    """
    result_map: dict[str, dict] = {}

    for ep in discovered:
        merged = {}
        # Apply defaults
        for key, val in defaults.items():
            if isinstance(val, dict):
                merged[key] = dict(val)
            else:
                merged[key] = val
        # Apply discovered fields (overwrite defaults)
        for key, val in ep.items():
            if isinstance(val, dict) and isinstance(merged.get(key), dict):
                merged[key].update(val)
            else:
                merged[key] = val
        result_map[ep["id"]] = merged

    # Apply per-provider overrides
    for pid, override in overrides.items():
        if pid in result_map:
            ep = result_map[pid]
            for key, val in override.items():
                if key == "base_url":
                    continue  # never overwrite discovered URL
                if isinstance(val, dict) and isinstance(ep.get(key), dict):
                    ep[key].update(val)
                else:
                    ep[key] = val
        else:
            # Native provider or provider not in discovery — inject whole entry
            entry = dict(override)
            entry.setdefault("id", pid)
            for key, val in defaults.items():
                entry.setdefault(key, val)
            result_map[pid] = entry

    return list(result_map.values())
```

**Step 5: Run tests**

Run: `cd /Users/siddharthakovid/Downloads/PRISM && python3 -m pytest tests/test_search_overrides.py tests/test_search_discovery.py -v`
Expected: ALL PASS

**Step 6: Commit**

```bash
git add app/search/providers/provider_overrides.json app/search/providers/discovery.py tests/test_search_overrides.py
git commit -m "feat(search): add provider_overrides.json + apply_overrides logic"
```

---

## Task 4: Rewrite `build_registry()` — The New Entry Point

**Files:**
- Modify: `app/search/providers/registry.py`
- Modify: `app/search/providers/endpoint.py` (keep `ProviderEndpoint` model, adapt `load_registry`)
- Test: `tests/test_search_registry.py` (update existing tests)

**Step 1: Update tests**

Rewrite `tests/test_search_registry.py` to test the new `build_registry()`:

```python
# tests/test_search_registry.py
"""Tests for ProviderRegistry — build_registry, routing, custom registration."""
from unittest.mock import patch


def test_build_registry_from_cache(tmp_path):
    """build_registry loads from discovery cache + overrides."""
    import json
    from app.search.providers.registry import build_registry
    from app.search.providers.discovery import save_cache

    # Seed a fake cache
    endpoints = [
        {"id": "mp", "name": "MP", "base_url": "https://mp.org", "parent": "mp"},
        {"id": "cod", "name": "COD", "base_url": "https://cod.org", "parent": "cod"},
    ]
    cache_path = tmp_path / "cache.json"
    save_cache(endpoints, path=cache_path)

    reg = build_registry(cache_path=cache_path, skip_network=True)
    providers = reg.get_all()
    assert len(providers) >= 2  # mp + cod + mp_native from overrides


def test_build_registry_includes_native_providers(tmp_path):
    """Native API providers from overrides are included even if not discovered."""
    import json
    from app.search.providers.registry import build_registry
    from app.search.providers.discovery import save_cache

    endpoints = [{"id": "mp", "name": "MP", "base_url": "https://mp.org", "parent": "mp"}]
    cache_path = tmp_path / "cache.json"
    save_cache(endpoints, path=cache_path)

    reg = build_registry(cache_path=cache_path, skip_network=True)
    ids = {p.id for p in reg.get_all()}
    assert "mp_native" in ids


def test_registry_get_capable():
    from app.search.providers.registry import ProviderRegistry
    from app.search.providers.base import Provider, ProviderCapabilities
    from app.search.query import MaterialSearchQuery

    class FakeProvider(Provider):
        id = "fake"
        name = "Fake"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})
        async def search(self, query): return []

    reg = ProviderRegistry()
    reg.register(FakeProvider())
    q = MaterialSearchQuery(elements=["Fe"])
    assert len(reg.get_capable(q)) == 1


def test_registry_register_custom():
    from app.search.providers.registry import ProviderRegistry
    from app.search.providers.base import Provider, ProviderCapabilities

    class FakeProvider(Provider):
        id = "fake"
        name = "Fake"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})
        async def search(self, query): return []

    reg = ProviderRegistry()
    reg.register(FakeProvider())
    assert "fake" in {p.id for p in reg.get_all()}
```

**Step 2: Run tests to verify failure**

Run: `cd /Users/siddharthakovid/Downloads/PRISM && python3 -m pytest tests/test_search_registry.py -v`
Expected: FAIL — `ImportError: cannot import name 'build_registry'`

**Step 3: Rewrite `registry.py`**

```python
# app/search/providers/registry.py
"""Provider registry -- discover, build, route queries."""
from __future__ import annotations

import logging

from app.search.providers.base import Provider, ProviderCapabilities
from app.search.providers.endpoint import ProviderEndpoint
from app.search.providers.optimade import OptimadeProvider
from app.search.providers.materials_project import MaterialsProjectProvider
from app.search.query import MaterialSearchQuery

logger = logging.getLogger(__name__)


class ProviderRegistry:
    """Manages all registered providers."""

    def __init__(self):
        self._providers: dict[str, Provider] = {}

    def register(self, provider: Provider) -> None:
        self._providers[provider.id] = provider

    def get_all(self) -> list[Provider]:
        return list(self._providers.values())

    def get_capable(self, query: MaterialSearchQuery) -> list[Provider]:
        """Return only providers that can handle this query's filters."""
        capable = []
        for p in self._providers.values():
            if query.providers and p.id not in query.providers:
                continue
            if p.capabilities.can_handle(query):
                capable.append(p)
        return capable

    @classmethod
    def from_endpoints(cls, endpoints: list[dict]) -> ProviderRegistry:
        """Build registry from resolved endpoint dicts."""
        reg = cls()
        for ep_data in endpoints:
            if not ep_data.get("enabled", True):
                continue
            if not ep_data.get("base_url"):
                continue
            try:
                ep = ProviderEndpoint.model_validate(ep_data)
                if ep.api_type == "optimade":
                    reg.register(OptimadeProvider(endpoint=ep))
                elif ep.api_type == "mp_native":
                    reg.register(MaterialsProjectProvider(endpoint=ep))
            except Exception as e:
                logger.debug("Skipping provider %s: %s", ep_data.get("id"), e)
        return reg

    # Keep backward compat — delegates to build_registry
    @classmethod
    def from_registry_json(cls) -> ProviderRegistry:
        """Legacy entry point — calls build_registry()."""
        return build_registry()


def build_registry(
    cache_path=None,
    overrides_path=None,
    skip_network: bool = False,
) -> ProviderRegistry:
    """Build the provider registry from discovery cache + overrides.

    1. Load discovery cache (or run discovery if missing/stale)
    2. Apply bundled overrides
    3. Return ProviderRegistry
    """
    from pathlib import Path
    from app.search.providers.discovery import (
        load_cache, save_cache, is_cache_fresh, discover_providers,
        load_overrides, apply_overrides, DEFAULT_CACHE_PATH,
    )
    import asyncio

    c_path = cache_path or DEFAULT_CACHE_PATH
    cache = load_cache(c_path)

    if cache and is_cache_fresh(cache):
        endpoints = cache["endpoints"]
    elif not skip_network:
        # Discovery needed
        try:
            overrides_data = load_overrides(overrides_path)
            fallbacks = overrides_data.get("fallback_index_urls", {})
            endpoints = asyncio.run(discover_providers(fallback_index_urls=fallbacks))
            if endpoints:
                save_cache(endpoints, c_path)
            elif cache:
                # Discovery failed but stale cache exists — use it
                logger.warning("Discovery failed, using stale cache")
                endpoints = cache["endpoints"]
            else:
                endpoints = []
        except Exception as e:
            logger.error("Discovery error: %s", e)
            endpoints = cache["endpoints"] if cache else []
    else:
        endpoints = cache["endpoints"] if cache else []

    # Apply overrides
    overrides_data = load_overrides(overrides_path)
    overrides = overrides_data.get("overrides", {})
    defaults = overrides_data.get("defaults", {})
    resolved = apply_overrides(endpoints, overrides, defaults)

    return ProviderRegistry.from_endpoints(resolved)
```

**Step 4: Run tests**

Run: `cd /Users/siddharthakovid/Downloads/PRISM && python3 -m pytest tests/test_search_registry.py tests/test_search_discovery.py tests/test_search_overrides.py -v`
Expected: ALL PASS

**Step 5: Run full test suite**

Run: `cd /Users/siddharthakovid/Downloads/PRISM && python3 -m pytest tests/ -x -q`
Expected: ALL PASS (636+). The existing `from_registry_json()` calls still work via the backward compat shim.

**Step 6: Commit**

```bash
git add app/search/providers/registry.py tests/test_search_registry.py
git commit -m "feat(search): rewrite registry to use discovery cache + overrides"
```

---

## Task 5: Add `--refresh` Flag to Search Command

**Files:**
- Modify: `app/commands/search.py`
- Test: `tests/test_search_cli_refresh.py`

**Step 1: Write failing test**

```python
# tests/test_search_cli_refresh.py
"""Tests for prism search --refresh CLI flag."""
from unittest.mock import patch, AsyncMock
from click.testing import CliRunner


def test_refresh_flag_triggers_discovery():
    from app.commands.search import search
    runner = CliRunner()

    mock_endpoints = [
        {"id": "mp", "name": "MP", "base_url": "https://mp.org", "parent": "mp"},
    ]

    with patch("app.commands.search.discover_providers", new_callable=AsyncMock, return_value=mock_endpoints) as mock_discover:
        with patch("app.commands.search.save_cache") as mock_save:
            with patch("app.commands.search.load_overrides", return_value={
                "fallback_index_urls": {},
                "overrides": {},
                "defaults": {},
            }):
                result = runner.invoke(search, ["--refresh"])
                assert mock_discover.called
                assert mock_save.called
                assert result.exit_code == 0


def test_refresh_flag_shows_provider_count():
    from app.commands.search import search
    runner = CliRunner()

    mock_endpoints = [
        {"id": "mp", "name": "MP", "base_url": "https://mp.org", "parent": "mp"},
        {"id": "cod", "name": "COD", "base_url": "https://cod.org", "parent": "cod"},
    ]

    with patch("app.commands.search.discover_providers", new_callable=AsyncMock, return_value=mock_endpoints):
        with patch("app.commands.search.save_cache"):
            with patch("app.commands.search.load_overrides", return_value={
                "fallback_index_urls": {},
                "overrides": {},
                "defaults": {},
            }):
                result = runner.invoke(search, ["--refresh"])
                assert "2" in result.output  # should mention count
```

**Step 2: Run to verify failure**

Run: `cd /Users/siddharthakovid/Downloads/PRISM && python3 -m pytest tests/test_search_cli_refresh.py -v`
Expected: FAIL

**Step 3: Add `--refresh` flag to `app/commands/search.py`**

Add the import and the flag at the top of the file. The flag triggers discovery + prints diff, then exits (does not run a search).

Add to the top of `app/commands/search.py` (after existing imports):

```python
from app.search.providers.discovery import discover_providers, save_cache, load_overrides
```

Add `--refresh` to the `@click.option` list:

```python
@click.option('--refresh', is_flag=True, help='Refresh the provider registry from OPTIMADE consortium and exit.')
```

Add `refresh` to the function signature. At the start of the function body, before the "at least one criterion" check:

```python
    if refresh:
        import asyncio
        console.print("[bold green]Refreshing provider registry from OPTIMADE consortium...[/bold green]")
        try:
            overrides_data = load_overrides()
            fallbacks = overrides_data.get("fallback_index_urls", {})
            endpoints = asyncio.run(discover_providers(fallback_index_urls=fallbacks))
            if endpoints:
                save_cache(endpoints)
                console.print(f"[green]Discovered {len(endpoints)} provider endpoints.[/green]")
                from rich.table import Table
                table = Table(show_header=True, header_style="bold dim")
                table.add_column("ID")
                table.add_column("Name")
                table.add_column("Base URL")
                for ep in sorted(endpoints, key=lambda e: e["id"]):
                    table.add_row(ep["id"], ep["name"], ep.get("base_url", "N/A"))
                console.print(table)
            else:
                console.print("[red]Discovery failed — no endpoints found.[/red]")
        except Exception as e:
            console.print(f"[red]Refresh error: {e}[/red]")
        return
```

**Step 4: Run tests**

Run: `cd /Users/siddharthakovid/Downloads/PRISM && python3 -m pytest tests/test_search_cli_refresh.py -v`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add app/commands/search.py tests/test_search_cli_refresh.py
git commit -m "feat(search): add --refresh flag to search command for provider discovery"
```

---

## Task 6: Delete `provider_registry.json` + Fix All References

**Files:**
- Delete: `app/search/providers/provider_registry.json`
- Modify: `app/search/providers/endpoint.py` — remove `_REGISTRY_PATH` and old `load_registry`
- Modify: `app/commands/optimade.py` — use new registry
- Modify: `tests/test_search_providers_refresh.py` — adapt to new discovery module
- Modify: any other files referencing old `provider_registry.json`

**Step 1: Find all references**

Run: `cd /Users/siddharthakovid/Downloads/PRISM && grep -r "provider_registry.json\|_REGISTRY_PATH\|load_registry\|from_registry_json" app/ tests/ --include="*.py" -l`

**Step 2: Update `endpoint.py`**

Remove the `_REGISTRY_PATH` constant and the old `load_registry` function. Keep all the Pydantic models (`ProviderEndpoint`, `AuthConfig`, etc.) — they're still used everywhere.

Replace `load_registry` with a thin wrapper that calls `build_registry`:

```python
def load_registry(path=None):
    """Legacy compat — returns list of ProviderEndpoint from the new registry.

    Prefer build_registry() directly.
    """
    from app.search.providers.registry import build_registry
    reg = build_registry()
    return [p._endpoint for p in reg.get_all() if hasattr(p, "_endpoint")]
```

Remove `_REGISTRY_PATH`.

**Step 3: Update `app/commands/optimade.py`**

The `list-dbs` command currently calls `load_registry()` from endpoint.py. Update it to use `build_registry()`:

```python
from app.search.providers.registry import build_registry

@optimade.command("list-dbs")
def list_databases():
    """Lists all available OPTIMADE provider databases."""
    console = Console(force_terminal=True, width=120)
    try:
        reg = build_registry()
        providers = reg.get_all()
        # ... table rendering using provider.id, provider.name, etc.
```

**Step 4: Update `tests/test_search_providers_refresh.py`**

The existing tests for `parse_providers_response` and `merge_registries` still apply to the old refresh.py. Since we're replacing refresh.py's role with discovery.py, either:
- Keep the old tests passing (refresh.py still exists but is secondary)
- Or migrate the tests to use discovery.py functions

Simplest: keep the old tests passing since `refresh.py` still has valid `parse_providers_response` and `merge_registries` functions. They work with the old format and could be useful as a fallback.

**Step 5: Delete `provider_registry.json`**

```bash
git rm app/search/providers/provider_registry.json
```

**Step 6: Run full test suite**

Run: `cd /Users/siddharthakovid/Downloads/PRISM && python3 -m pytest tests/ -x -q`
Expected: ALL PASS

**Step 7: Commit**

```bash
git add -A
git commit -m "refactor(search): delete provider_registry.json, all URLs now from auto-discovery"
```

---

## Task 7: Full Test Suite + Integration Smoke Test

**Files:**
- All test files

**Step 1: Run all new + existing tests**

Run: `cd /Users/siddharthakovid/Downloads/PRISM && python3 -m pytest tests/ -v`
Expected: ALL PASS

**Step 2: Manual smoke test (requires network)**

```python
import asyncio
from app.search.providers.discovery import discover_providers, save_cache, load_overrides

async def smoke():
    overrides = load_overrides()
    fallbacks = overrides.get("fallback_index_urls", {})
    endpoints = await discover_providers(fallback_index_urls=fallbacks)
    print(f"Discovered {len(endpoints)} endpoints:")
    for ep in sorted(endpoints, key=lambda e: e["id"]):
        print(f"  {ep['id']:30s} {ep['base_url']}")
    save_cache(endpoints)

asyncio.run(smoke())
```

Then run a search using the discovered registry:

```python
import asyncio
from app.search import SearchEngine, MaterialSearchQuery
from app.search.providers.registry import build_registry

async def search_test():
    reg = build_registry()
    engine = SearchEngine(registry=reg)
    result = await engine.search(MaterialSearchQuery(elements=["Fe", "O"], limit=5))
    print(f"Found {result.total_count} materials from {len(result.query_log)} providers")
    for log in result.query_log:
        print(f"  {log.provider_id}: {log.status} ({log.result_count} results)")

asyncio.run(search_test())
```

**Step 3: Final commit**

```bash
git add -A
git commit -m "feat(search): complete registry auto-discovery — OPTIMADE URLs from source of truth"
```

---

## Summary: File Map

| Task | Files Created/Modified | Tests |
|---|---|---|
| 1. Discovery module | Create: `discovery.py` | `test_search_discovery.py` (5 tests) |
| 2. Cache tests | — | `test_search_discovery.py` (+3 tests) |
| 3. Overrides file | Create: `provider_overrides.json`, Modify: `discovery.py` | `test_search_overrides.py` (5 tests) |
| 4. build_registry | Modify: `registry.py` | `test_search_registry.py` (4 tests) |
| 5. --refresh flag | Modify: `app/commands/search.py` | `test_search_cli_refresh.py` (2 tests) |
| 6. Delete old JSON | Delete: `provider_registry.json`, Modify: `endpoint.py`, `optimade.py` | regression |
| 7. Integration | — | smoke test |

**Total: 2 new files, 1 new JSON, 1 deleted JSON, 4 modified files, 3 new test files, 7 commits.**
