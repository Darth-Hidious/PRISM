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

Providers are auto-discovered from `providers.optimade.org/v1/links` via a 2-hop chain:

```
providers.optimade.org/v1/links  (hop 1: provider index)
  -> {index-metadb}/v1/links     (hop 2: child databases)
    -> actual base_urls
```

Three layers, merge order highest wins:
1. **Discovered** -- URLs from OPTIMADE consortium (cached weekly at `~/.prism/cache/discovered_registry.json`)
2. **Bundled overrides** -- `provider_overrides.json` (tiers, capabilities, quirks, native API entries)
3. **User overrides** -- `~/.prism/providers.yaml` (enable/disable, custom entries)

## Provider Status (2026-02-25)

### Tier 1 -- Primary (high quality, fast)
| Provider | ID | Status | Notes |
|----------|-----|--------|-------|
| Materials Project (OPTIMADE) | `mp` | OK | 155K structures |
| Materials Project (Native) | `mp_native` | OK | Requires `MP_API_KEY` |
| NOMAD | `nmd` | OK | 12M+ structures |
| Alexandria PBE | `alexandria.alexandria-pbe` | OK | 5M+ structures |
| Alexandria PBEsol | `alexandria.alexandria-pbesol` | OK | 5M+ structures |
| OQMD | `oqmd` | OK | 1M+ structures, slow on complex queries |
| COD | `cod` | OK | 500K+ experimental structures |
| GNoME (Google DeepMind) | `odbx.gnome` | OK | 380K ML-predicted structures |
| MPDD | `mpdd` | URL FIX | Consortium has wrong `.org`, corrected to `.com` |

### Tier 2 -- Secondary
| Provider | ID | Status | Notes |
|----------|-----|--------|-------|
| JARVIS | `jarvis` | OK | Re-enabled, NIST fixed their endpoint (2025) |
| TCOD | `tcod` | OK | Theoretical crystal structures |
| 2DMatpedia | `twodmatpedia` | OK | 2D materials, HTTP only |
| Materials Cloud MC3D | `mcloud.mc3d-pbe-v1` | OK | Via fallback URL |
| Materials Cloud MC2D | `mcloud.mc2d` | OK | Via fallback URL |

### Tier 3 -- Available but limited
| Provider | ID | Status | Notes |
|----------|-----|--------|-------|
| Matterverse | `matterverse` | OK | |
| OMDB | `omdb` | OK | HTTP only |
| odbx | `odbx.odbx_main` | OK | Small dataset |
| odbx-misc | `odbx.odbx_misc` | OK | Miscellaneous data |
| MPDS | `mpds` | DISABLED | Requires paid subscription |

### Tier 4 -- Disabled
| Provider | ID | Status | Notes |
|----------|-----|--------|-------|
| AFLOW | `aflow` | HTTP 500 | OPTIMADE endpoint broken, returns server errors |

### Offline / Dead
| Provider | ID | Status | Notes |
|----------|-----|--------|-------|
| CMR | `cmr` | 404 | GitHub Pages site gone |
| MatCloud | `matcloud` | 404 | Index proxy, not a structures endpoint |
| MPOD | `mpod` | TIMEOUT | Server unreachable |

### Not in OPTIMADE
| Dataset | Notes |
|---------|-------|
| OMAT24 (Meta) | 110M DFT calculations. HuggingFace dataset only, no OPTIMADE endpoint. Derived from Alexandria structures. |
| AFLOW (native) | Has REST API at `aflow.org/API/` but not OPTIMADE-compatible. Future native provider candidate. |

## Architecture

```
app/search/
  __init__.py              # re-exports SearchEngine, MaterialSearchQuery, etc.
  engine.py                # SearchEngine: orchestrates providers, cache, health
  query.py                 # MaterialSearchQuery, PropertyRange
  result.py                # Material, PropertyValue, SearchResult, ProviderQueryLog
  translator.py            # QueryTranslator: query -> OPTIMADE filter string
  fusion.py                # FusionEngine: dedup + merge across providers

  providers/
    discovery.py           # 2-hop OPTIMADE auto-discovery + cache + overrides
    registry.py            # ProviderRegistry + build_registry()
    endpoint.py            # ProviderEndpoint Pydantic model
    provider_overrides.json # tiers, capabilities, quirks, native API entries
    base.py                # Provider ABC, ProviderCapabilities
    optimade.py            # OptimadeProvider (wraps optimade-python-tools)
    materials_project.py   # MaterialsProjectProvider (native API)
    refresh.py             # Legacy refresh (delegates to discovery.py)

  cache/
    engine.py              # SearchCache: query -> result caching

  resilience/
    circuit_breaker.py     # HealthManager: per-provider circuit breaker
```
