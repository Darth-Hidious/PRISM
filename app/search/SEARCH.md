# PRISM Federated Search Engine

## How It Works

PRISM queries 20+ materials databases simultaneously using the [OPTIMADE](https://www.optimade.org/) protocol, fuses results across providers, and returns unified materials with full provenance tracking.

```
User Query (elements, formula, band_gap, ...)
    |
    v
MaterialSearchQuery (Pydantic model, validated)
    |
    v
QueryTranslator.to_optimade() --> OPTIMADE filter string
    |
    v
SearchEngine.search()
    |-- ProviderRegistry.get_capable(query) --> list of providers
    |-- asyncio.gather(*[provider.search(query) for provider in capable])
    |-- FusionEngine.fuse(raw_results) --> deduplicated, merged materials
    |-- SearchCache.put(query, result)
    v
SearchResult (materials + audit trail + warnings)
```

## Query Schema

```python
MaterialSearchQuery:
    # Composition
    elements: ["Fe", "O"]           # all of these elements must be present
    elements_any: ["Fe", "Ni"]      # any of these elements
    exclude_elements: ["Pb"]        # none of these elements
    formula: "SiO2"                 # exact formula
    n_elements: {min: 2, max: 4}    # element count range

    # Properties (PropertyRange: {min, max})
    band_gap: {min: 1.0, max: 3.0}             # eV
    formation_energy: {min: -2.0, max: 0.0}     # eV/atom
    energy_above_hull: {min: 0.0, max: 0.05}    # eV/atom
    bulk_modulus: {min: 100}                     # GPa
    debye_temperature: {min: 300}                # K

    # Structural
    space_group: "Fm-3m"            # Hermann-Mauguin symbol or int
    crystal_system: "cubic"         # cubic|hexagonal|tetragonal|orthorhombic|monoclinic|triclinic|trigonal

    # Control
    providers: ["mp", "cod"]        # restrict to specific providers
    limit: 100                      # max results (1-10000)
```

## Result Schema

```python
SearchResult:
    materials: [Material]       # fused, deduplicated
    total_count: int            # before limit
    query: MaterialSearchQuery  # echo
    query_log: [ProviderQueryLog]  # per-provider audit
    warnings: [str]
    cached: bool
    search_time_ms: float

Material:
    id: str                     # stable ID
    formula: str                # chemical formula
    elements: ["Fe", "O"]       # sorted
    n_elements: int
    sources: ["mp", "cod"]      # which providers contributed

    # Properties with provenance (PropertyValue: {value, source, method, unit})
    space_group: {value: "Fm-3m", source: "optimade:mp"}
    band_gap: {value: 1.5, source: "optimade:mp", unit: "eV"}
    formation_energy: {value: -1.2, source: "optimade:oqmd", unit: "eV/atom"}
    lattice_vectors: {value: [[a1,a2,a3],[b1,b2,b3],[c1,c2,c3]], source: "optimade:cod"}
    extra_properties: {"_mp_stability": {value: ..., source: "optimade:mp"}}

ProviderQueryLog:
    provider_id: str            # e.g. "mp"
    status: success|timeout|http_error|parse_error|circuit_open|skipped
    result_count: int
    latency_ms: float
    error_message: str|null
```

## CLI Usage

```bash
# Basic search
prism search --elements Fe,O --limit 20

# Property filters
prism search --elements Si --band-gap-min 1.0 --band-gap-max 3.0

# Specific providers
prism search --formula SiO2 --providers mp,cod,oqmd

# Refresh provider registry from OPTIMADE consortium
prism search --refresh

# Crystal system
prism search --elements Ti,O --crystal-system tetragonal --space-group "I4/mmm"
```

## Provider Registry

### Three-Layer Architecture

```
Layer 1 -- Discovered (OPTIMADE consortium, auto-refreshed weekly)
  Source: providers.optimade.org/v1/links -> {index}/v1/links chain
  Cache:  ~/.prism/cache/discovered_registry.json
  Contains: id, name, base_url, homepage, parent_provider

Layer 2 -- Bundled overrides (shipped with PRISM)
  File: app/search/providers/provider_overrides.json
  Contains: tiers, capabilities, auth config, native API entries, known quirks
  Also: fallback_index_urls (proxies that 404), url_corrections (consortium typos)

Layer 3 -- Platform & user sources
  Dev:  app/search/marketplace.json (MARC27 catalog, NOT shipped to users)
  Prod: MARC27 marketplace API (auth-gated, native API, dataset providers)
  User: ~/.prism/providers.yaml (personal overrides, optional)
```

Merge order: discovered -> bundled overrides -> user overrides. Higher layer wins per field.

### Discovery: 2-Hop OPTIMADE Chain

```
providers.optimade.org/v1/links  (hop 1: provider index, ~28 entries)
  -> {index-metadb}/v1/links     (hop 2: child databases per provider)
    -> actual base_urls           (flattened into discovery cache)
```

Single-child providers (COD, MP, OQMD) produce one endpoint.
Multi-child providers (Alexandria, Materials Cloud, odbx) produce multiple endpoints
with IDs like `alexandria.alexandria-pbe`, `mcloud.mc3d-pbe-v1`.

### Layer 2 Corrections

Some consortium URLs are wrong. Layer 2 handles two types of corrections:

- **fallback_index_urls**: For providers whose proxy at `providers.optimade.org` 404s
  (e.g. Materials Cloud). Maps `provider_id -> working /v1/links URL`.
- **url_corrections**: For providers where the consortium lists a wrong base_url
  (e.g. MPDD `.org` vs `.com`). Maps `provider_id -> correct base_url`. Applied
  after discovery, before overrides.

These are consortium data errors, not server issues. When fixed upstream, our
corrections become harmless no-ops.

## Provider Status (2026-02-25)

### Tier 1 -- Primary (high quality, fast)
| Provider | ID | Status | Notes |
|----------|-----|--------|-------|
| Materials Project (OPTIMADE) | `mp` | OK | 155K structures |
| NOMAD | `nmd` | OK | 12M+ structures |
| Alexandria PBE | `alexandria.alexandria-pbe` | OK | 5M+ structures |
| Alexandria PBEsol | `alexandria.alexandria-pbesol` | OK | 5M+ structures |
| OQMD | `oqmd` | OK | 1M+ structures, slow on complex queries |
| COD | `cod` | OK | 500K+ experimental structures |
| GNoME (Google DeepMind) | `odbx.gnome` | OK | 380K ML-predicted structures |
| MPDD | `mpdd` | OK | URL corrected: consortium lists `.org`, actual is `.com` |

### Tier 2 -- Secondary
| Provider | ID | Status | Notes |
|----------|-----|--------|-------|
| JARVIS (NIST) | `jarvis` | OK | Re-enabled 2025, NIST fixed their endpoint |
| TCOD | `tcod` | OK | Theoretical crystal structures |
| 2DMatpedia | `twodmatpedia` | OK | 2D materials, HTTP only |
| Materials Cloud MC3D | `mcloud.mc3d-pbe-v1` | OK | Via fallback URL (proxy 404s) |
| Materials Cloud MC2D | `mcloud.mc2d` | OK | Via fallback URL (proxy 404s) |

### Tier 3 -- Available but limited
| Provider | ID | Status | Notes |
|----------|-----|--------|-------|
| Matterverse | `matterverse` | OK | |
| OMDB | `omdb` | OK | HTTP only |
| odbx | `odbx.odbx_main` | OK | Small dataset |
| odbx-misc | `odbx.odbx_misc` | OK | Miscellaneous data |
| MPDS | `mpds` | DISABLED | Requires paid subscription |

### Tier 4 -- Disabled (broken OPTIMADE endpoints)
| Provider | ID | Status | Issue | Last checked |
|----------|-----|--------|-------|-------------|
| AFLOW | `aflow` | HTTP 500 | OPTIMADE wrapper broken (`/v1/structures` returns 500). Native AFLUX API works (3.5M+ compounds) — available as `aflow_native` in Layer 3 marketplace. | 2026-02-25 |

### Offline / Dead
| Provider | ID | Status | Issue | Last checked |
|----------|-----|--------|-------|-------------|
| CMR | `cmr` | 404 | GitHub Pages site gone entirely | 2026-02-25 |
| MatCloud | `matcloud` | 404 | Index proxy at providers.optimade.org, not a structures endpoint | 2026-02-25 |
| MPOD | `mpod` | TIMEOUT | Server unreachable, connection refused | 2026-02-25 |

## Extending Search

### Current Provider Types

Search currently supports two provider types, both registered in `ProviderRegistry`:

| Type | Base Class | How it queries | Example |
|------|-----------|---------------|---------|
| `optimade` | `OptimadeProvider` | OPTIMADE filter via `optimade-python-tools` | mp, cod, oqmd |
| `mp_native` | `MaterialsProjectProvider` | Materials Project REST API | mp_native |

### Adding a New Provider Type

To add a new provider type (e.g. `aflow_native`, `jarvis_native`):

1. Create `app/search/providers/<name>.py` implementing `Provider` ABC
2. Add the type string to `ProviderRegistry.from_endpoints()` dispatch
3. Add the provider entry in `marketplace.json` with `api_type` and `base_url`

### Layer 3: MARC27 Marketplace Providers

Auth-gated, native API, and dataset providers are accessed through the MARC27
platform. These are NOT shipped in the package — `marketplace.json` is a
development-time catalog only. In production, Layer 3 providers come from the
MARC27 marketplace API.

**Current marketplace catalog (`app/search/marketplace.json`):**

| Provider | API Type | Status | Notes |
|----------|----------|--------|-------|
| Materials Project (Native) | `mp_native` | available | Requires `MP_API_KEY` |
| AFLOW (AFLUX API) | `aflow_native` | coming_soon | 3.5M+ compounds, native API confirmed working |
| MPDS | `mpds_native` | coming_soon | Paid subscription required |
| OMAT24 (Meta FAIR) | `dataset` | coming_soon | 110M DFT calculations on HuggingFace |

**User overrides** (`~/.prism/providers.yaml`, optional):
Users can override any marketplace field (enable/disable, custom auth, etc.)
via a local YAML file. User entries win over marketplace defaults.

**Planned architecture for datasets:**

```python
class DatasetProvider:
    """Queryable local/cached dataset."""
    def search(self, query: MaterialSearchQuery) -> list[Material]: ...
    def info(self) -> DatasetInfo: ...

# Marketplace sources are NOT part of the OPTIMADE discovery pipeline.
# They are served by the MARC27 platform at runtime.
```

## Architecture

```
app/search/
  __init__.py              # re-exports SearchEngine, MaterialSearchQuery, etc.
  engine.py                # SearchEngine: orchestrates providers, cache, health
  query.py                 # MaterialSearchQuery, PropertyRange
  result.py                # Material, PropertyValue, SearchResult, ProviderQueryLog
  translator.py            # QueryTranslator: query -> OPTIMADE filter string
  fusion.py                # FusionEngine: dedup + merge across providers
  SEARCH.md                # this file

  marketplace.json         # Layer 3 dev catalog (NOT shipped, replaced by platform API)

  providers/
    discovery.py           # 2-hop OPTIMADE auto-discovery + cache + overrides + Layer 3
    registry.py            # ProviderRegistry + build_registry()
    endpoint.py            # ProviderEndpoint Pydantic model
    provider_overrides.json # Layer 2: tiers, capabilities, quirks, URL corrections
    base.py                # Provider ABC, ProviderCapabilities
    optimade.py            # OptimadeProvider (wraps optimade-python-tools)
    materials_project.py   # MaterialsProjectProvider (native API)
    refresh.py             # Legacy refresh (delegates to discovery.py)

  cache/
    engine.py              # SearchCache: query -> result caching

  resilience/
    circuit_breaker.py     # HealthManager: per-provider circuit breaker

~/.prism/                  # User data directory
  cache/
    discovered_registry.json  # Layer 1: auto-discovery cache (refreshed weekly)
    provider_health.json      # circuit breaker state
  databases/                  # Future: Layer 3 dataset storage
  providers.yaml              # Future: Layer 3 user overrides
```
