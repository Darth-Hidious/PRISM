# Fast fetch — replace Firecrawl with a millisecond-first fetcher

Owner direction (2026-08-08): *"Firecrawl is fucking slow. Firecrawl takes too much space.
Firecrawl uses Chromium. We need a different browser. Millisecond-first."*

## Why Firecrawl is slow and heavy — confirmed

The deployed stack is **five Railway services**: `marc27-firecrawl`, `firecrawl-playwright`,
`firecrawl-redis`, `firecrawl-rabbitmq`, `firecrawl-nuq-postgres`. `firecrawl-playwright` runs
**full Chromium per fetch**. That is the slow, heavy, disk-hungry part: a browser process, a
render, a queue, a DB — to read text off a page.

## The core insight

**A browser is the wrong default, not a browser that's too slow.** Chromium renders JavaScript.
Almost no scientific/reference source needs that — the data is in a machine-readable form behind the
page. The paper engine already proves this: 8 sources, 3.6s, **zero rendering**. The fetcher should
render only when it has proven the data cannot be reached any other way.

## `marc27-fetch` — one static Rust binary, three tiers, no Chromium

Ships as a single binary. Replaces the 5-service stack. Same in-process crate the retrieval engine
already lives in.

### Tier 0 — HTTP + reader. THE MILLISECOND PATH. (build first)
Plain `reqwest` GET, then extract in-process:
1. **JSON-LD** (`<script type="application/ld+json">`) — most publishers emit full structured
   metadata here. Zero parsing of prose.
2. **Hydration state** — `__NEXT_DATA__`, `__NUXT__`, `window.__INITIAL_STATE__`. Server-rendered
   frameworks ship the whole page's data as JSON in the HTML. No JS execution needed to read it.
3. **Readability** — a `<article>`/main-content extractor (`readability`-style DOM scoring via the
   `scraper` crate) for plain prose pages.

Target: **sub-100 ms** for static and server-rendered pages. This is 80%+ of real URLs.

### Tier 1 — structured-source shortcut. NEVER RENDER A KNOWN SOURCE.
Before Tier 0 even runs, a domain registry maps known hosts to their API / OAI-PMH / dump endpoint
(the retrieval engine already does this for arXiv, OpenAlex, Crossref, PubMed, Europe PMC, DOAJ —
generalize that registry). `doi.org` → Crossref/Unpaywall; `*.mdpi.com`, `sciencedirect`, etc →
their content API. **Zero rendering, machine-readable payload.**

### Tier 2 — JS execution, last resort, explicitly slow.
When Tier 0 returns a shell with no data (a genuine SPA), do NOT spawn Chromium per fetch. Options,
in order of preference:
1. **A single long-lived headless engine over CDP** — one process, reused, not one-per-request.
2. **A lightweight JS runtime** (`boa`/`deno_core`) that executes the page's inline scripts without
   a full browser, when the page only needs its own JS run to hydrate.
3. Real headless browser only if 1–2 fail — flagged in the result as `tier: 2, rendered: true` so
   slowness is visible, never hidden.

## What this buys

- **Speed:** the common case drops from "queue → Chromium render → scrape" (seconds) to one HTTP
  round-trip + parse (milliseconds).
- **Space:** one binary vs Chromium + 4 support services. The Railway Firecrawl stack can be torn
  down once Tier 0/1 cover the callers.
- **Honesty:** every result says which tier served it and whether anything was rendered.

## Build order

1. **Tier 0 crate** — GET + JSON-LD + hydration + readability, returning a typed
   `FetchedDoc { url, title, text, structured: Option<Value>, tier, rendered }`. Benchmarked
   against 20 real URLs, real numbers, no fabrication.
2. **Wire it behind the existing `web` tool** as the default, Firecrawl demoted to a fallback flag.
3. **Tier 1 registry**, seeded from the retrieval engine's source list.
4. **Tier 2** only if measured need remains after 0+1.
5. Tear down the Railway Firecrawl services once callers are migrated.

## Non-negotiables

- **Never fabricate.** A fetch that gets nothing returns nothing, with the reason. No placeholder
  text, no invented content. (`fallback_proposals` was deleted from this repo for exactly this.)
- **No Chromium in the default path.** If a fetch renders, it is Tier 2 and it says so.
- Benchmarks are measured and pasted, never estimated.
