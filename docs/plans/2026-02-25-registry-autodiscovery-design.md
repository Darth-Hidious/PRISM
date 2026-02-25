# Registry Auto-Discovery Design

## Problem

The provider registry (`provider_registry.json`) hardcodes 30 provider entries including base URLs manually copy-pasted from OPTIMADE documentation. These URLs go stale — we just fixed 3 wrong ones (mpdd wrong domain, Materials Cloud wrong subdomain path). Meanwhile, the OPTIMADE consortium maintains the canonical registry at `providers.optimade.org` with live, correct URLs.

We're duplicating work that the consortium already does, and doing it worse.

## Solution

Split the registry into three layers:

```
Layer 1 — Discovered (from OPTIMADE consortium)
  Source: providers.optimade.org/v1/links → {index}/v1/links chain
  Cache:  ~/.prism/cache/discovered_registry.json
  Contains: id, name, base_url, description, homepage, parent_provider
  Refresh: weekly auto, or `prism search --refresh`

Layer 2 — Bundled overrides (shipped with PRISM)
  File: app/search/providers/provider_overrides.json
  Contains: tiers, capabilities, auth config, native API entries, known quirks
  NO base_url entries (except native API providers like mp_native, future aflow_native)

Layer 3 — User overrides
  File: ~/.prism/providers.yaml
  Contains: user's enable/disable, custom DB entries, personal auth config
  _user_override: true makes an entry immune to discovery updates
```

Merge order: discovered → bundled overrides → user overrides. Higher layer wins per field.

## Discovery Algorithm

```
fetch providers.optimade.org/v1/links
  → 28 providers, each with an index-metadb URL

for each provider with index URL:
    fetch {index_url}/v1/links
      → child databases with actual base_urls
    if 404: check FALLBACK_INDEX_URLS map
    if timeout: skip, use cached entry

flatten into: [{id, name, base_url, parent, description, homepage}, ...]
write to ~/.prism/cache/discovered_registry.json with timestamp
```

**FALLBACK_INDEX_URLS** handles providers whose proxy 404s:

```json
{
  "mcloud": "https://www.materialscloud.org/optimade/main/v1/links"
}
```

This is a small, rarely-changing map bundled in the overrides file.

## Discovery Chain Detail

The OPTIMADE ecosystem has a 2-hop structure:

```
providers.optimade.org/v1/links
  ├─ aflow  → providers.optimade.org/index-metadbs/aflow/v1/links
  │            └─ child: aflow → https://aflow.org/API/optimade/
  ├─ mp     → providers.optimade.org/index-metadbs/mp/v1/links
  │            └─ child: mp → https://optimade.materialsproject.org
  ├─ alexandria → .../index-metadbs/alexandria/v1/links
  │               ├─ child: alexandria-pbe → https://alexandria.icams.rub.de/pbe
  │               └─ child: alexandria-pbesol → https://alexandria.icams.rub.de/pbesol
  ├─ mcloud → PROXY 404, use fallback → materialscloud.org/.../v1/links
  │            ├─ child: mc3d-pbe-v1 → optimade.materialscloud.org/main/mc3d-pbe-v1
  │            ├─ child: mc2d → optimade.materialscloud.org/main/mc2d
  │            └─ child: curated-cofs, pyrene-mofs, 2dtopo, ...
  └─ aiida  → base_url=null (namespace only, no endpoint)
```

Single-child providers (COD, MP, OQMD) → one endpoint.
Multi-child providers (Alexandria, Materials Cloud) → multiple endpoints, each a separate Provider.

## Startup Sequence

```python
def build_registry() -> ProviderRegistry:
    # 1. Load or discover
    cache = load_discovery_cache()   # ~/.prism/cache/discovered_registry.json
    if cache is None or cache.age_days > 7:
        if cache is None:
            # First run: blocking discovery
            discovered = run_discovery()
            save_cache(discovered)
        else:
            # Stale: use cache now, refresh in background
            schedule_background_refresh()
        endpoints = cache.entries if cache else discovered
    else:
        endpoints = cache.entries

    # 2. Apply bundled overrides
    overrides = load_bundled_overrides()  # app/search/providers/provider_overrides.json
    endpoints = apply_overrides(endpoints, overrides)

    # 3. Apply user overrides
    user = load_user_overrides()          # ~/.prism/providers.yaml
    endpoints = apply_overrides(endpoints, user)

    # 4. Build registry
    return ProviderRegistry.from_endpoints(endpoints)
```

## What `provider_overrides.json` Looks Like

```json
{
  "_meta": {
    "version": "1.0.0",
    "description": "PRISM-specific overrides. URLs come from OPTIMADE auto-discovery."
  },

  "fallback_index_urls": {
    "mcloud": "https://www.materialscloud.org/optimade/main/v1/links"
  },

  "defaults": {
    "behavior": { "timeout_ms": 10000, "max_results": 1000 },
    "capabilities": {
      "filterable_fields": ["elements", "formula", "nelements", "space_group"]
    }
  },

  "overrides": {
    "nmd": {
      "tier": 1,
      "enabled": true
    },
    "mp": {
      "tier": 1,
      "enabled": true,
      "reliability": { "known_quirks": ["Returns species_at_sites only"] }
    },
    "mp_native": {
      "api_type": "mp_native",
      "base_url": "https://api.materialsproject.org",
      "tier": 1,
      "enabled": true,
      "auth": {
        "required": true,
        "auth_type": "api_key",
        "auth_env_var": "MP_API_KEY"
      },
      "capabilities": {
        "filterable_fields": ["elements", "formula", "band_gap", "formation_energy"]
      }
    },
    "oqmd": {
      "tier": 1,
      "enabled": true,
      "behavior": { "timeout_ms": 15000 },
      "reliability": { "known_quirks": ["Very slow on complex queries"] }
    },
    "aflow": {
      "tier": 4,
      "enabled": false,
      "reliability": { "known_quirks": ["OPTIMADE endpoint barely functional"] }
    }
  }
}
```

Only ~15-20 providers need overrides. The rest use defaults.

## `--refresh` Flag

Lives on `app/commands/search.py` (not the main CLI monolith):

```
prism search --refresh              # force re-discovery, print diff, exit
prism search --elements Fe,O        # normal search (uses cached registry)
```

The `--refresh` flag triggers blocking discovery, prints a diff table showing new/changed/offline providers, saves cache, and exits. It does not run a search.

## Error Handling

| Scenario | Behavior |
|----------|----------|
| `providers.optimade.org` down | Fall back to GitHub raw mirror |
| Individual `/v1/links` 404 | Check `fallback_index_urls`, else skip |
| Individual `/v1/links` timeout | Skip, use cached entry if available |
| All discovery fails | Use stale cache (any age). Never zero providers. |
| No cache + all discovery fails | Fall back to bundled `provider_overrides.json` native entries only |
| Background refresh finds changes | Hot-swap into running registry, save cache |

## What Gets Deleted

- `app/search/providers/provider_registry.json` (1113 lines) — replaced by auto-discovery + overrides
- The hardcoded URL maintenance burden

## What Gets Created

- `app/search/providers/provider_overrides.json` (~150 lines) — tiers, capabilities, quirks
- `app/search/providers/discovery.py` — the 2-hop discovery + cache logic
- Modified: `app/search/providers/registry.py` — new `build_registry()` entry point
- Modified: `app/search/providers/refresh.py` — rewritten for 2-hop chain
- Modified: `app/commands/search.py` — `--refresh` flag
