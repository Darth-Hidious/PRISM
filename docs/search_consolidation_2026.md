# Search Consolidation — making PRISM's data layer not feel like a "fucking weird mix"

**Status:** Research + design doc. No code change in this commit. Captures
the actual landscape (verified via codebase MCP + WebSearch) and proposes
how to collapse PRISM's many search surfaces into one tool the LLM can
reach for without picking the wrong one.

**One-line problem:** PRISM has many data sources — local KG, OPTIMADE
federation (22 providers, ~22 M structures), Materials Project,
literature (arXiv / Semantic Scholar), patents (Lens), web (Firecrawl),
and soon proprietary providers like ExoMatter. The LLM-agent today
sees them as separate point tools and has to pick correctly per turn.
That's both fragile and exposes data-source quirks the user shouldn't
care about.

---

## What PRISM has today (verified, not assumed)

### Already exposed as MCP tools the agent can call

| Tool | Source | Where |
|---|---|---|
| `literature_search` | arXiv + Semantic Scholar | `app/tools/search.py` |
| `patent_search` | Lens.org | `app/tools/search.py` |
| `web_search` / `web_read` | Firecrawl when available | `app/tools/web.py` |
| `semantic_search` | local KG vectors | `app/tools/knowledge.py` |
| `knowledge_search` / `knowledge_entity` / `knowledge_paths` / `knowledge_stats` | local KG graph traversal | `app/tools/knowledge.py` |
| `list_corpora` / `knowledge_ingest` | local KG management | `app/tools/knowledge.py` |
| `research` / `research_query` | MARC27 cloud research engine | `crates/agent/src/command_tools.rs` |

### Built but NOT yet exposed as a tool — `app/tools/search_engine/`

This is real infrastructure already in the repo:

```
app/tools/search_engine/
  engine.py          # SearchEngine — federated orchestrator
  fusion.py          # cross-provider dedup + property merge (formula + space_group keying)
  query.py           # typed MaterialSearchQuery
  translator.py      # per-provider query translation
  result.py          # Material / SearchResult / PropertyValue
  cache/             # disk-backed result cache
  resilience/        # circuit breakers per provider
  providers/
    base.py          # Provider abstraction
    materials_project.py
    optimade.py
    discovery.py     # provider auto-discovery
    endpoint.py
    registry.py      # ProviderRegistry
    refresh.py
    provider_overrides.json
```

**This is exactly the federated-search engine the user asked for.** It
already handles:
- Pluggable providers (Materials Project + OPTIMADE federation)
- Per-provider query translation (translates one `MaterialSearchQuery`
  into each provider's native query format)
- Result fusion (deduplicate same material across providers; merge
  conflicting property values into an `extra` field with provider tag)
- Caching + circuit breakers (so a flaky provider doesn't tank the
  whole call)

What it lacks: **registration as an MCP tool**. The `SearchEngine`
class exists but `__init__.py` only exports types; no `create_search_tools`
function in this folder, no entry in the agent's tool catalog. So the
LLM can't pick this when the user says "find me materials with X."

---

## The landscape — what we're actually federating across (verified May 2026)

### OPTIMADE federation
- **Specification**: REST API for materials databases, [Materials-Consortia/OPTIMADE](https://github.com/Materials-Consortia/OPTIMADE)
- **Scale**: 22 registered providers, 25 interoperable databases, **22 million crystal structures with associated properties** ([NIST 2024 review](https://pubs.rsc.org/en/content/articlehtml/2024/dd/d4dd00039k), [arXiv 2402.00572](https://arxiv.org/abs/2402.00572))
- **Strength**: same query syntax across all 22 providers; one query → 22 backend results
- **Weakness called out in the OPTIMADE 2024 review**: *"if the database backend cannot support the type of query the user represents (for example, a database containing molecular dynamics calculations of proteins cannot sensibly be searched on the chemical formula in the simulation cell), it means that there exists no way to make that query interoperable across databases without altering the backend."* That's the heterogeneity tax — query *what* differs per provider.

### Proprietary / commercial providers outside OPTIMADE

| Provider | What | API status |
|---|---|---|
| **ExoMatter** ([exomatter.ai](https://www.exomatter.ai/)) | German Aerospace Center spin-off, focus on inorganic crystalline (ceramic oxides, semiconductors). Customers: Audi, Infineon, Airbus. Has their own conversational agent **Exira**. Raised €1.7M seed late 2024. | Not visibly in the OPTIMADE federation as of search. Public API status unclear; likely BYO-key / partnership-shaped |
| **Citrine** | Materials informatics, ML platform | Public API, separate auth |
| **MaterialsZone** | Industrial materials database | Separate API |
| **Nomad** | OPTIMADE-compatible (so it's already in the federation) | OPTIMADE provider |

**The pattern**: a few big federations (OPTIMADE, Materials Project) +
a long tail of proprietary providers each with their own API and auth.
ExoMatter is one of those long-tail providers — and for PRISM it joins
the same provider abstraction as Materials Project / OPTIMADE.

### Non-materials sources (different data type, different shape)

- Literature: arXiv, Semantic Scholar (already wired as `literature_search`)
- Patents: Lens.org (already wired as `patent_search`)
- Web: Firecrawl (`web_search`, `web_read`)
- User's own corpora: local KG (`semantic_search`, `knowledge_search`)
- Persisted research sessions: local KG hits with `[research_session]` tag

These are different *kinds* of search — not "candidate materials" but
"papers", "patents", "web pages", "prior research notes". They don't
fuse with materials hits — they're complementary, not competing.

---

## Proposed consolidation — three tools, not nine

The right LLM-facing surface is **three** tools, each backed by a
federated implementation:

### 1. `materials_search`
Single tool that wraps `app/tools/search_engine/SearchEngine`. The LLM
passes a typed `MaterialSearchQuery` (formula, property ranges, space
group, …); the engine fans out to every healthy provider, fuses the
results, returns one unified `SearchResult`. The LLM doesn't know or
care which databases were hit.

```yaml
materials_search:
  query:
    formula: "Ni3Al"             # optional
    property_ranges:             # optional
      bulk_modulus: [180, 220]   # GPa
    space_group: "Pm-3m"         # optional
    element_filter: ["Ni", "Al", "+W"]  # optional
    max_results: 100
    providers:                   # optional override
      - materials_project
      - alexandria
      - oqma
      - cod
      - exomatter                # added when wired
```

Returns:
- merged `Material` records
- per-provider provenance for every property (so the LLM can cite
  which DB said what)
- circuit-breaker state for transparency ("OQMD timed out, used 19/20
  providers")

### 2. `literature_and_patents_search`
Wraps the existing `literature_search` + `patent_search` into one tool
with a `source` flag (`papers`, `patents`, `both`). Returns same
unified record shape (`title`, `authors`, `year`, `doi/url`, `abstract`,
`provider`).

The LLM only has to pick this when the question is about prior art /
papers / IP, not about candidate materials.

### 3. `knowledge_search` (the one we already have)
Stays as it is. This is the local KG — different role: "what do *we*
already know from prior sessions / ingested user data?" Results carry
the `[research_session]`, `[corpus:<name>]`, etc. tags so the LLM
knows where the hit came from.

`web_search` and `web_read` stay as their own tools — they're a
different action (fetching arbitrary URLs / a free-text query against
the open web), not a federation of materials databases.

---

## Provider abstraction — Exomatter and friends slot in here

The existing `app/tools/search_engine/providers/base.py` already has
the `Provider` abstraction. Adding a new provider means:

1. Implement `Provider` with three methods:
   - `translate_query(query: MaterialSearchQuery) → ProviderNativeQuery`
   - `execute(native_query) → list[Material]`
   - `capabilities() → ProviderCapabilities`
2. Add an entry in `provider_overrides.json` (rate limits, timeouts,
   auth env-var name)
3. Drop the file in `providers/`; auto-discovery picks it up

`capabilities()` is the lever for the OPTIMADE-flagged limitation —
each provider declares what it can do (formula filtering yes, property
ranges yes, structure search no, …). The translator skips providers
that can't answer a given query, so users don't get spurious empty
results from incapable backends.

Adding ExoMatter:

```python
# app/tools/search_engine/providers/exomatter.py
class ExomatterProvider(Provider):
    name = "exomatter"
    requires_env = "EXOMATTER_API_KEY"  # BYO-key model
    base_url = "https://api.exomatter.ai/v1"   # placeholder, confirm with vendor

    def capabilities(self) -> ProviderCapabilities:
        return ProviderCapabilities(
            formula=True,
            property_ranges=True,
            structure=True,
            inorganic_crystalline_only=True,  # narrow domain per their pitch
        )

    def translate_query(self, q): ...
    def execute(self, native_q): ...
```

Same shape as `materials_project.py` and `optimade.py`. Done.

---

## The OPTIMADE limitation — what to do about it

The cited limitation: *"a protein DB can't be searched by formula
sensibly."* The fix is **per-provider capability declarations**, which
the existing translator can already act on. Nothing about the data is
mis-interoperated; the engine just routes the query to providers whose
capabilities cover the query's required axes.

For queries that can't be answered well by any single provider, the
fusion layer can (in a follow-up commit) compose results: one
provider answers part A, another answers part B, results are joined
on the common identity key (formula + space group). That's a real
search-fusion pattern, and the bones of it are already in
`fusion.py`.

---

## What this consolidation gets us

1. **LLM tool surface shrinks** from 9 search tools to 3 — easier for
   the agent to pick right, less prompt budget per turn.
2. **New providers slot in without changing the agent** — the LLM never
   sees that ExoMatter joined; it just gets richer `materials_search`
   results.
3. **Provenance is preserved** — every property has a provider tag, so
   when the chat says "Inconel 718 has bulk modulus X", the source DB
   is in the citation.
4. **Failure isolation** — circuit breaker takes a flaky provider out
   of rotation without affecting others.
5. **Caching is shared** — one disk cache across providers via
   `cache/engine.py`.

---

## What this doc is NOT

- Not building it. The `SearchEngine` exists; wrapping it as an MCP tool
  + adding the ExoMatter provider is application-layer code, not part
  of the architecture refactor. Belongs to a fresh small commit/PR
  *after* Phase 1 lands.
- Not replacing OPTIMADE. OPTIMADE *is* one of the providers. The
  consolidation just hides the per-provider differences from the LLM.
- Not a critique of OPTIMADE — its 22-DB / 22M-structure scale is
  exactly why we want it in the federation. The "limitation" is just
  a feature of heterogeneous data, addressable via capability flags.

---

## Concrete next steps (small PR after Phase 1)

1. Add `app/tools/search_engine/tools.py` exposing
   `create_search_engine_tools(registry)` that registers
   `materials_search` as an MCP tool wrapping `SearchEngine.search()`.
2. Collapse `literature_search` + `patent_search` into one
   `literature_and_patents_search` tool with a `source` flag.
3. Add `ProviderCapabilities` typed declaration and have
   `QueryTranslator` consult it before fanning out.
4. Wire up an `exomatter.py` provider scaffold (real API integration
   gated behind whatever access the user negotiates with them).
5. Update the materials-science-tools doc to reference this layer.

Each step is a separate commit; the whole thing is ~1-2 days of work
once you green-light it.

---

## Sources

- [OPTIMADE specification](https://github.com/Materials-Consortia/OPTIMADE)
- [OPTIMADE 2024 review — Developments and applications, Andersen et al. RSC Digital Discovery 2024](https://pubs.rsc.org/en/content/articlehtml/2024/dd/d4dd00039k) — 22 providers / 25 DBs / 22 M structures, capability heterogeneity discussion
- [OPTIMADE on arXiv — 2402.00572](https://arxiv.org/abs/2402.00572)
- [Materials Cloud OPTIMADE APIs](https://optimade.materialscloud.org/)
- [ExoMatter — AI-powered materials R&D](https://www.exomatter.ai/) — German company, inorganic crystalline focus, customers include Audi/Infineon/Airbus
- [ExoMatter Materials Screening Engine](https://www.exomatter.ai/materials-screening-engine/)
- [ExoMatter — features](https://www.exomatter.ai/features/) — Exira conversational agent for materials screening
