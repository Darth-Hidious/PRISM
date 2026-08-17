# Making literature search actually work

Everything below is measured, not assumed. Each source was probed live on
2026-08-17 with a real query (`Ti-6Al-4V fatigue`).

## Where we are

| source | probe | state |
|---|---|---|
| Crossref | 200, 15 KB | works |
| OpenAlex | 200, 56 KB | works |
| Europe PMC | 200, 2.7 KB | works |
| NTRS (NASA) | 200, 55 KB | works |
| DOAJ | 200, 8.0 KB | works |
| **arXiv** | **301, 0 B** | **BROKEN — fixed below** |
| **ChemRxiv** | **403** | **blocked, cause unknown** |
| **Semantic Scholar** | **429** | **rate-limited, no key** |
| PubMed | not probed | unknown |
| Patents (Lens.org) | needs `LENS_API_TOKEN` | **unconfigured** |
| **internal graph** | 0 entities / 0 assertions | **was empty — merged, see §3** |

Two failures are the whole story of why the Ti-6Al-4V answer came from a
public SAE page instead of our own corpus.

## 1. arXiv was returning nothing — FIXED

`sources/arxiv.rs:10` declared `http://export.arxiv.org/api/query`. arXiv
answers plain HTTP with a bare `301` and an **empty body**; the client does
not follow redirects. So every arXiv search returned zero results, silently,
on the most important source in the domain — 63 references to `arxiv` across
the tree. Changed to `https://`, which returns 200 and real entries.

**This is the shape of defect to hunt for.** It never errored. It never
logged. The agent just concluded arXiv had nothing on the subject.

## 2. The remaining broken sources

- **ChemRxiv 403** — not a User-Agent block (tested with a real UA, still
  403). Either the public API moved or it is behind a bot filter. Decide:
  fix it, or REMOVE it from the source list. A source that always 403s is
  worse than an absent one, because it pollutes every result set with a
  failure row and teaches the model the search "partly failed".
- **Semantic Scholar 429** — unauthenticated rate limit. Needs an API key
  (free) or a backoff+cache. Until then it should be marked degraded, not
  presented as a live source.
- **PubMed** — implemented (`sources/pubmed.rs`) but never probed. Verify.
- **Patents** — `PatentCollector` requires `LENS_API_TOKEN`, which is not
  set, so patent search is dead. It fails honestly (raises rather than
  returning "no patents"), which is right. Getting a Lens.org token is an
  owner action; alternatives that need no key are EPO OPS (registration) and
  Google Patents via scraping (fragile, discouraged).

## 3. The internal graph was empty — MERGED

The agent's local store had **0 entities, 0 assertions**. Meanwhile the
91-paper corpus run had produced **21,227 assertions across 91 SEPARATE
stores**, one per paper — forced, because PRISM takes an exclusive lock and
concurrent writers to one store fail on open.

Merged into `~/Downloads/prism-experiment/corpus_merged.db`:

- 34,897 entities · 21,218 assertions · 20,984 edges
- 21,227 evidence rows · **34,897 embeddings** (the vector-search substrate)
- 9 assertions (0.04%) dropped on duplicate ids

**PRISM has no import/merge capability at all** — this was hand-rolled SQL.
For a product whose thesis is a knowledge graph that accumulates across
ingests, "no way to combine two stores" is an architectural gap, and it is
exactly why 21k facts were stranded.

## 4. What a proper literature search must do

Today `prior_art_search` fans out and returns a flat list. For real research
that is not enough. The target behaviour, in order:

1. **Search our own graph FIRST.** 34,897 entities with embeddings are
   sitting there. Semantic search over them costs nothing and is the only
   part nobody else has. If the answer is already extracted, cite our own
   provenance — with the paper, the line range, and the extraction status.
2. **Then external, in parallel, with per-source honesty.** Each source
   reports `ok(n)` / `empty` / `error(reason)` / `degraded(rate-limited)`.
   Never a bare aggregate. A 403 is not "no results".
3. **Deduplicate by DOI, then by normalised title.** The same paper arrives
   from Crossref, OpenAlex and Europe PMC; presenting it three times fakes
   corroboration, which is the one thing this product must not do.
4. **Rank by evidence, not by recency.** Peer-reviewed > preprint; a
   specification (SAE/ASTM/ISO) outranks both for a normative value.
5. **Patents are a distinct question** and must be labelled as such: a
   patent is evidence of a *claim*, not of a measurement.
6. **Every returned item carries its provenance**: source, id/DOI, date,
   type (journal / preprint / spec / patent / dataset), and whether we hold
   the full text.

## 5. Artefacts the user can see

Right now a search is a wall of tool JSON that scrolls past. A literature
search should leave a durable, inspectable artefact:

- **A search record**, stored, with: the query, every source queried, each
  source's outcome, counts, timings, dedup decisions, and the final ranked
  list. Reproducible and auditable after the fact.
- **A rendered report** the user can open — the ranked hits with title,
  authors, year, venue, DOI, type, and why each was ranked where it was.
- **Reachable both ways** (the standing rule): a tool the agent calls, and
  a TUI surface the user opens — not a CLI-only path.

PRISM already has an artifact plane (`crates/tui/src/artifact.rs`,
`list_artifacts`, `fetch_artifact`); the literature search should write into
it rather than inventing a parallel mechanism.

## 6. Questions worth asking

Generic lookups ("what is the yield strength of Ti-6Al-4V") are answerable
from a spec sheet and prove little. Questions that exercise the system:

- **Cross-corpus synthesis** — "Across our 91 ingested papers, which report
  tunnel magnetoresistance, what values, and how do they disagree?"
  (Needs: internal graph + embeddings.)
- **Contradiction hunting** — "Find two papers in our graph that report
  different values for the same quantity on the same material."
  (This is the corroboration thesis, tested directly.)
- **Prior-art / novelty** — "Has anyone patented laser powder-bed fusion of
  a refractory high-entropy alloy with in-situ pyrometry feedback?"
  (Needs: patents, and the distinction between claim and measurement.)
- **Provenance challenge** — "You said 825 MPa. Which line of which document
  says so, and what is the extraction status of that fact?"
  (Tests re-verification — the half of annotate-don't-refuse that is still
  unwired.)
- **Negative result** — "What does our corpus say about X?" where X is
  absent, to confirm it says "nothing" rather than inventing.

## Order of work

1. ~~arXiv scheme~~ — done.
2. Verify every source end-to-end THROUGH PRISM (not curl), and make each
   report its own honest status.
3. Decide ChemRxiv: fix or remove. Get a Semantic Scholar key.
4. Wire the internal graph + embeddings as the FIRST search step.
5. Dedup by DOI/title; rank by evidence class.
6. Write the search artefact; surface it in the TUI.
7. Patents: owner needs to supply `LENS_API_TOKEN`, or we pick another
   provider.
