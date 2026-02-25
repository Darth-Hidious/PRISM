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
  Dev:  app/plugins/catalog.json (unified plugin catalog, NOT shipped to users)
  Prod: MARC27 marketplace API (auth-gated, native API, dataset, agent plugins)
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
3. Add the provider entry in `app/plugins/catalog.json` with `type: "provider"`, `api_type`, and `base_url`

### Layer 3: MARC27 Plugin Catalog

Auth-gated, native API, dataset, and agent plugins are accessed through the MARC27
platform. These are NOT shipped in the package — `catalog.json` is a
development-time catalog only. In production, Layer 3 plugins come from the
MARC27 marketplace API.

**Current plugin catalog (`app/plugins/catalog.json`, providers only):**

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

## Caching & Resilience

### Search Cache

Query results are cached in-memory with optional disk persistence to avoid
redundant provider round-trips.

```
SearchCache
  ├── query_cache     {query_hash -> CachedResult}   # in-memory, keyed by query hash
  ├── material_index  {material_id -> Material}       # cross-query material lookup
  └── disk_dir        ~/.prism/cache/                 # optional disk persistence
```

**How it works:**
1. `SearchEngine.search()` checks the in-memory cache first via `query.query_hash()`
2. Cache hit returns immediately with `cached: true` in the result
3. Cache miss fans out to providers, fuses results, then caches the `SearchResult`
4. `flush_to_disk()` serializes each cached query to `{query_hash}.json`
5. `load_from_disk()` restores cache on startup (only loads still-fresh entries)

**Cache entry format (disk):**
```json
{
  "query": { "elements": ["Fe", "O"], "limit": 100, ... },
  "result": { "materials": [...], "total_count": 42, "query_log": [...] },
  "timestamp": 1740000000.0,
  "ttl": 86400
}
```

**Configuration:**
| Setting | Default | Description |
|---------|---------|-------------|
| TTL | 24 hours (86400s) | Time before a cached result expires |
| Disk dir | `~/.prism/cache/` | Where disk-backed cache files live |

### Circuit Breaker (Provider Health)

Each provider has independent health tracking to avoid hammering broken endpoints.

```
HealthManager
  └── provider_health  {provider_id -> ProviderHealth}
        ├── circuit_state          closed | open | half_open
        ├── consecutive_failures   int (opens circuit at 3)
        ├── avg_latency_ms         exponential moving average
        ├── success_count / failure_count
        └── last_failure           timestamp
```

**State machine:**
```
CLOSED ---(3 consecutive failures)---> OPEN
OPEN   ---(cooldown 60s expires)-----> HALF_OPEN
HALF_OPEN -(success)-----------------> CLOSED
HALF_OPEN -(failure)-----------------> OPEN
```

When a circuit is OPEN, the provider is skipped entirely — no HTTP request is made.
After the cooldown, one probe request is allowed (HALF_OPEN). Success resets the circuit.

**Persistence:** Health state is saved to `~/.prism/cache/provider_health.json` after
every search. This means circuit states survive across sessions — a provider that went
down during one search won't be hammered on the next.

**Health file format:**
```json
{
  "mp": {
    "provider_id": "mp",
    "circuit_state": "closed",
    "consecutive_failures": 0,
    "avg_latency_ms": 1250.0,
    "success_count": 14,
    "failure_count": 0,
    "last_failure": null
  },
  "aflow": {
    "provider_id": "aflow",
    "circuit_state": "open",
    "consecutive_failures": 3,
    "avg_latency_ms": 0.0,
    "success_count": 0,
    "failure_count": 3,
    "last_failure": 1740000000.0
  }
}
```

### Marketplace: Storage & Cache (Planned)

The MARC27 marketplace will provide managed infrastructure for users who don't
want to self-host cache and health state:

| Service | Self-hosted (default) | Marketplace (planned) |
|---------|----------------------|----------------------|
| Discovery cache | `~/.prism/cache/discovered_registry.json` | Platform-managed, always fresh |
| Search cache | `~/.prism/cache/{hash}.json` (local disk) | Cloud-backed, shared across team |
| Health state | `~/.prism/cache/provider_health.json` | Aggregated from all users, smarter routing |
| Dataset storage | `~/.prism/databases/` | Platform CDN, lazy download, versioned |

**How marketplace caching will work:**
- Self-hosted users keep current behavior (local files under `~/.prism/`)
- Marketplace users get a `cache_backend` config pointing to the platform API
- `SearchCache` gains a pluggable backend: `LocalDiskBackend` (current) or `MarketplaceBackend`
- Health data aggregated across all marketplace users gives better circuit-breaker
  decisions (a provider going down is detected faster when N users report failures)

**Endpoint configuration** (`~/.prism/config.yaml`, planned):
```yaml
cache:
  backend: local          # or "marketplace"
  ttl: 86400              # seconds
  disk_dir: ~/.prism/cache

# Marketplace users
cache:
  backend: marketplace
  api_url: https://api.marc27.io/cache
  team_id: my-team
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
  # docs/search.md         # this file (moved from app/search/)

  # marketplace.json moved to app/plugins/catalog.json (unified plugin catalog)

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

## Related

- [`prism data`](data.md) -- Collect and store search results
- [`prism predict`](predict.md) -- Predict properties of found materials
- [`prism sim`](sim.md) -- Simulate structures from search
- [`prism model calphad`](predict.md) -- Phase diagrams for found compositions
- [`prism labs`](labs.md) -- Premium services for deeper analysis
- [Plugins](plugins.md) -- Register custom search providers
