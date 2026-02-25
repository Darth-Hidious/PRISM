"""OPTIMADE 2-hop provider discovery.

Walks: providers.optimade.org/v1/links -> {index}/v1/links -> child endpoints.
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
# Map provider_id -> working index URL.
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
                logger.debug("Skipping %s -- no links response", pid)
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
                # Single-endpoint provider -- no children listed
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
            # Native provider or provider not in discovery -- inject whole entry
            entry = dict(override)
            entry.setdefault("id", pid)
            for key, val in defaults.items():
                entry.setdefault(key, val)
            result_map[pid] = entry

    return list(result_map.values())
