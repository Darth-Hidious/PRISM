# Search & ingestion tools — consolidated inventory

Every search / fetch / ingest surface in PRISM, top to bottom. Compiled 2026-08-08 by walking
`app/tools/**` and `crates/**` — **118 registered tool names total, 14 in the search family**, plus
the Rust paper-fetch path.

Status is what I could establish from the source. Nothing here is certified as *working* yet —
that is the next step, and it needs live runs.

---

## A. Literature — papers

| # | Tool | Source it hits | Needs | Status |
|---|---|---|---|---|
| A1 | Rust paper fetch (`crates/retrieval`) | **Europe PMC** `ebi.ac.uk/europepmc/webservices/rest` · **NCBI OA** `ncbi.nlm.nih.gov/pmc/utils/oa` | — | Real. This is the main path. Feeds the claims engine. |
| A2 | `eastern_literature` | **J-STAGE** `api.jstage.jst.go.jp/searchapi` · **archive.org** advancedsearch · OAI-PMH | — | Real HTTP. **4 tests skip unless `PRISM_LIVE_SOURCES=1`** — so it is untested in CI. |
| A3 | `licensed_sources` | delegates → platform client | — | Delegating layer, no direct endpoint. |
| A4 | `prior_art_search` | delegates → shells to `PRISM_BINARY` | `PRISM_BINARY` | Indirect: spawns the Rust CLI. |

## B. Web

| # | Tool | Source | Needs | Status |
|---|---|---|---|---|
| B1 | `web` (fetch) | **Firecrawl** `api.firecrawl.dev/v1` or self-hosted | `FIRECRAWL_API_KEY` **or** `FIRECRAWL_API_URL` / `FIRECRAWL_LOCAL_URL` | **All three unset in this shell.** Self-hosted stack exists (5-service Railway). |
| B2 | `web` (search) | **DuckDuckGo** `html.duckduckgo.com` via `ddgs` | — | Real, keyless. Scraping endpoint — brittle by nature. |

## C. Materials databases

| # | Tool | Source | Needs | Status |
|---|---|---|---|---|
| C1 | `materials_search` | delegates → `search_engine/engine.py` → provider registry | — | Orchestrator over C2–C4. |
| C2 | OPTIMADE provider | `optimade.materialsproject.org` | — | Real HTTP, keyless. |
| C3 | OPTIMADE **discovery** | `providers.optimade.org/v1/links` · `materialscloud.org` · GitHub raw | — | Federation index. ⚠ **MPDS is in that registry and is paid** — a naive federated query hits paid data. |
| C4 | `query_materials_project` / MP provider | Materials Project API | **`MP_API_KEY`** | **Unset.** Cannot run. |
| C5 | `lookup_structure` | `materials/screening.py` | — | Local screening. |
| C6 | `omat24` collector | HuggingFace `datasets` | — | Bulk dataset pull, no HTTP client of its own. |

## D. Patents

| # | Tool | Source | Needs | Status |
|---|---|---|---|---|
| D1 | `patents` | **Lens.org** `api.lens.org/patent/search` | **`LENS_API_TOKEN`** | **Unset.** Cannot run. 68 lines — thinnest collector in the tree. |

## E. Internal / memory

| # | Tool | Source | Status |
|---|---|---|---|
| E1 | `search_artifacts`, `fetch_artifact` | local provenance store | Local, no network. |
| E2 | `search_existing` | `tool_reasoning.py` | Local. |
| E3 | `start/check/cancel/list_background_research` | `agent_runs.py` | Orchestration, not a source. |

---

## What this inventory already tells us

**1. Three of the highest-value sources cannot run right now — all for the same reason.**
`MP_API_KEY`, `LENS_API_TOKEN`, and the Firecrawl trio are unset. Materials Project, patents, and
web fetch are the three that most directly serve "which material" questions. This is configuration,
not code.

**2. The one path that is fully keyless and real is the paper path** — Europe PMC + NCBI OA, in
Rust, feeding the claims engine that was hardened over the last 139 commits. That is the strongest
link in the chain today.

**3. Eastern literature is real but unexercised.** Four tests skip unless `PRISM_LIVE_SOURCES=1`, so
J-STAGE and archive.org have no CI coverage at all. Real code, zero proof.

**4. Document understanding is down, and it degrades everything above.** The VLM Space returns 404
(paused) and MinerU's health check was lying. **Every PDF currently ingests as flat text** — no
figures, no tables. Numbers living in tables are simply not seen, whichever search tool found the
paper.

**5. The federation has a licensing trap.** OPTIMADE discovery pulls a provider list that includes
**MPDS (paid)**, and Materials Project's own collections mix **CC BY 4.0 core with CC BY-NC GNoME
data** under `batch_id: "gnome_r2scan_statics"`. A naive bulk pull silently takes non-commercial
data into a commercial product.

---

## Certification plan — what "it works" has to mean

For each tool, in this order:

1. **Runs at all** — invoke it live, capture real output or the real error. No mocks.
2. **Returns real data** — the result traces to a URL that can be opened and matches.
3. **Fails honestly** — with the key removed, it says which key and how to set it. It does not
   return an empty list that reads as "no results".
4. **Is reachable by the agent** — registered in the catalog and callable, not just importable.
5. **Provenance survives** — what it returns lands in the graph with the source attached.

Order to certify, by value:

1. **A1 paper fetch** — the spine. Prove one paper end-to-end: fetch → parse → typed fact → graph,
   with the fact tracing back to its source block.
2. **B1/B2 web** — set Firecrawl, or make the DDG-only path explicit rather than a silent fallback.
3. **C2/C3 OPTIMADE** — keyless, so it should work today. If it does not, that is a code defect.
4. **C4 Materials Project** — needs the key.
5. **D1 patents** — needs the token.
6. **A2 eastern literature** — run with `PRISM_LIVE_SOURCES=1` and find out.

**Blocking everything: the VLM Space.** Until figure/table reading is restored, every source above
delivers text-only papers, and the tables where the numbers live are invisible.
