"""Registry refresh -- discover new OPTIMADE providers and update the local registry."""
from __future__ import annotations

import json
import logging
from pathlib import Path

import httpx

logger = logging.getLogger(__name__)

PROVIDERS_INDEX_URL = "https://providers.optimade.org/v1/links"
PROVIDERS_FALLBACK_URL = (
    "https://raw.githubusercontent.com/Materials-Consortia/providers"
    "/master/src/links/v1/providers.json"
)

# Meta/example entries that should never appear in a real registry.
_SKIP_IDS = frozenset({"exmpl", "optimade", "optimake"})


def parse_providers_response(response: dict) -> list[dict]:
    """Parse the OPTIMADE providers index response into a flat list.

    Each entry in the returned list has keys: id, name, base_url, homepage.
    Meta entries (exmpl, optimade, optimake) are silently skipped.
    """
    providers: list[dict] = []
    for entry in response.get("data", []):
        pid = entry.get("id", "")
        if pid in _SKIP_IDS:
            continue
        attrs = entry.get("attributes", {})
        providers.append({
            "id": pid,
            "name": attrs.get("name", pid),
            "base_url": attrs.get("base_url"),
            "homepage": attrs.get("homepage_url", ""),
        })
    return providers


def merge_registries(
    existing: list[dict],
    discovered: list[dict],
) -> tuple[list[dict], list[dict]]:
    """Merge discovered providers into existing registry.

    Returns:
        (merged_providers, changes) where *changes* is a list of dicts
        describing what was added or modified.

    Rules:
    - New provider ids are appended with tier=3, enabled=False, status="discovered".
    - Namespace-reserved entries that now have a base_url get activated.
    - URL changes are applied unless the entry has ``_user_override=True``.
    - Entries flagged ``_user_override`` are never mutated (preserves local overrides).
    """
    existing_map = {p["id"]: p for p in existing}
    changes: list[dict] = []

    for disc in discovered:
        pid = disc["id"]

        if pid not in existing_map:
            # Brand-new provider
            changes.append({"type": "new_provider", "id": pid, "name": disc["name"]})
            existing_map[pid] = {
                "id": pid,
                "name": disc["name"],
                "base_url": disc.get("base_url"),
                "homepage": disc.get("homepage", ""),
                "api_type": "optimade",
                "tier": 3,
                "enabled": False,
                "status": "discovered",
            }
        else:
            cur = existing_map[pid]

            # Never touch user-overridden entries.
            if cur.get("_user_override"):
                continue

            # Namespace activation: was reserved, now has a URL.
            if (
                cur.get("status") == "namespace_reserved"
                and disc.get("base_url")
            ):
                changes.append({"type": "namespace_activated", "id": pid})
                cur["base_url"] = disc["base_url"]
                cur["status"] = "discovered"

            # URL change on an already-known provider.
            elif (
                disc.get("base_url")
                and cur.get("base_url") != disc.get("base_url")
            ):
                changes.append({
                    "type": "url_changed",
                    "id": pid,
                    "old": cur.get("base_url"),
                    "new": disc["base_url"],
                })
                cur["base_url"] = disc["base_url"]

    return list(existing_map.values()), changes


async def refresh_registry(registry_path: Path | None = None) -> list[dict]:
    """Fetch latest providers from OPTIMADE consortium, merge with local registry.

    Returns the list of changes applied (empty list if nothing changed or on
    network failure).
    """
    from app.search.providers.endpoint import _REGISTRY_PATH

    path = registry_path or _REGISTRY_PATH

    # Fetch from primary, fall back to GitHub mirror.
    async with httpx.AsyncClient(timeout=10.0) as client:
        try:
            resp = await client.get(PROVIDERS_INDEX_URL)
            resp.raise_for_status()
            discovered = parse_providers_response(resp.json())
        except Exception:
            logger.warning("Failed to fetch from primary URL, trying fallback")
            try:
                resp = await client.get(PROVIDERS_FALLBACK_URL)
                resp.raise_for_status()
                discovered = parse_providers_response(resp.json())
            except Exception:
                logger.error(
                    "Failed to refresh provider registry from any source"
                )
                return []

    data = json.loads(path.read_text())
    existing = data["providers"]
    merged, changes = merge_registries(existing, discovered)

    if changes:
        data["providers"] = merged
        path.write_text(json.dumps(data, indent=2))
        logger.info("Registry updated: %d changes", len(changes))

    return changes
