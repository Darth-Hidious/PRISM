"""Prior-art search tool: papers + patents in one federated call.

Previously two separate tools (`literature_search`, `patent_search`).
Today we collapse them into a single `prior_art_search` with a
`source` flag that picks "papers", "patents", or "both" — federated
across arXiv / Semantic Scholar / Lens, results unified in one shape.

Why collapse:
  - The agent doesn't need to pick correctly between two tools when
    both answer the same question class ("what's been written about X?").
  - Reducing tool count tightens the Stage 2.1 retrieval prompt
    budget — every tool description goes into the embedder; fewer
    tools = sharper top-K matches.
  - Future "research_local" iterative loops want one prior-art entry
    point, not two.

The two original tools stay registered as aliases for backward
compatibility — anything that calls `literature_search` directly
keeps working. The agent's catalog, however, sees the unified one
first because it has a richer description.
"""
from app.tools.base import Tool, ToolRegistry


def _compact_abstract(text, limit: int = 400) -> str:
    """Trim abstracts so a 20-result payload stays well under the agent's
    tool-result budget — a full-abstract payload got truncated to its first
    entries, making every query look like it returned the same thing."""
    text = (text or "").strip()
    return text if len(text) <= limit else text[:limit].rsplit(" ", 1)[0] + "…"


def _literature_search_impl(**kwargs) -> dict:
    """Run the LiteratureCollector. Internal helper for both the unified
    `prior_art_search` and the legacy `literature_search` alias."""
    from app.tools.data_collectors.literature_collector import LiteratureCollector
    collector = LiteratureCollector()
    out = collector.collect_with_status(
        query=kwargs.get("query", ""),
        max_results=kwargs.get("max_results", 20),
        sources=kwargs.get("sources"),
    )
    results = out["results"]
    for r in results:
        r["abstract"] = _compact_abstract(r.get("abstract"))
    return {
        "results": results,
        "count": len(results),
        "source": "literature",
        "source_status": out["source_status"],
    }


def _eastern_search_impl(**kwargs) -> dict:
    """Run the EasternLiteratureCollector — Soviet/Russian, Chinese and
    Japanese sources that arXiv/Semantic Scholar do not index."""
    from app.tools.data_collectors.eastern_literature_collector import (
        EasternLiteratureCollector,
    )
    collector = EasternLiteratureCollector()
    out = collector.collect_with_status(
        query=kwargs.get("query", ""),
        max_results=kwargs.get("max_results", 20),
        sources=kwargs.get("sources"),
    )
    results = out["results"]
    for r in results:
        r["abstract"] = _compact_abstract(r.get("abstract"))
    return {
        "results": results,
        "count": len(results),
        "source": "eastern_literature",
        "source_status": out["source_status"],
    }


def _patent_search_impl(**kwargs) -> dict:
    """Run the PatentCollector. Internal helper for both the unified tool
    and the legacy `patent_search` alias."""
    from app.tools.data_collectors.patent_collector import PatentCollector
    collector = PatentCollector()
    results = collector.collect(**kwargs)
    return {
        "results": results,
        "count": len(results),
        "source": "patents",
    }


def _prior_art_search(**kwargs) -> dict:
    """Federated prior-art lookup.

    `source`: "papers" (default), "patents", "eastern", or "both".

    For "both", we run the backends in sequence (sequential is fine here —
    no call is heavy enough to need parallelism, and keeping the
    implementations independent means a Lens API failure doesn't prevent
    literature results from flowing).

    Result shape stays uniform regardless of source:
      { "papers": [...], "patents": [...], "eastern": [...],
        "counts": {"papers": N, "patents": M, "eastern": K} }
    Empty arrays for unrequested sources so consumers don't have to
    null-check.
    """
    query = kwargs.get("query", "")
    max_results = kwargs.get("max_results", 20)
    source = (kwargs.get("source") or "papers").lower()

    out: dict = {
        "papers": [],
        "patents": [],
        "eastern": [],
        "counts": {"papers": 0, "patents": 0, "eastern": 0},
        "query": query,
    }

    if source in ("eastern", "both"):
        try:
            east = _eastern_search_impl(
                query=query,
                max_results=max_results,
                sources=kwargs.get("eastern_sources"),
            )
            out["eastern"] = east.get("results", [])
            out["counts"]["eastern"] = east.get("count", 0)
            # Gated sources (CNKI, eLIBRARY, Wanfang) report here by name so
            # "no Chinese results" is never mistaken for "nothing published".
            out["eastern_source_status"] = east.get("source_status", {})
        except Exception as exc:
            out["eastern_error"] = str(exc)

    if source in ("papers", "both"):
        try:
            lit = _literature_search_impl(
                query=query,
                max_results=max_results,
                sources=kwargs.get("sources"),  # arxiv / semantic_scholar override
            )
            out["papers"] = lit.get("results", [])
            out["counts"]["papers"] = lit.get("count", 0)
            out["source_status"] = lit.get("source_status", {})
        except Exception as exc:
            out["papers_error"] = str(exc)

    if source in ("patents", "both"):
        try:
            pat = _patent_search_impl(query=query, max_results=max_results)
            out["patents"] = pat.get("results", [])
            out["counts"]["patents"] = pat.get("count", 0)
        except Exception as exc:
            # Lens commonly fails for users without LENS_API_TOKEN —
            # surface as a per-source error instead of failing the
            # whole call. The agent can decide to retry with
            # source="papers" if it cares.
            out["patents_error"] = str(exc)

    return out


# Backward-compat wrappers — keep the original tool shape for callers
# that hard-coded `literature_search` / `patent_search`.
def _literature_search(**kwargs) -> dict:
    return _literature_search_impl(**kwargs)


def _patent_search(**kwargs) -> dict:
    return _patent_search_impl(**kwargs)


def create_search_tools(registry: ToolRegistry) -> None:
    """Register the unified prior-art tool plus the legacy aliases."""

    # Unified tool — preferred path. Larger, richer description so the
    # Stage 2.1 retriever picks this one over the legacy aliases for
    # most prior-art queries.
    registry.register(Tool(
        name="prior_art_search",
        description=(
            "Federated prior-art search across scientific literature "
            "(arXiv, Semantic Scholar), patents (Lens.org), AND non-Western "
            "sources (CyberLeninka's Russian aerospace-materials journals, "
            "NASA Technical Translations of Soviet work, J-STAGE Japanese "
            "metallurgy, scanned Soviet handbooks on Internet Archive). Use "
            "this for any 'what has been published / patented about X?' "
            "question. The `source` flag selects 'papers' (default), "
            "'patents', 'eastern', or 'both'. Returns a uniform shape "
            "{ papers, patents, eastern, counts } — empty arrays for "
            "unrequested sources. Per-source failures (e.g. Lens auth "
            "missing, CNKI licence required) are reported in `papers_error` "
            "/ `patents_error` / `eastern_source_status` without failing the "
            "whole call, so the agent gets partial results and knows which "
            "source was skipped rather than genuinely empty."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": (
                        "Search query, e.g. 'tungsten rhenium alloy "
                        "phase stability' or 'high entropy alloy coating'."
                    ),
                },
                "source": {
                    "type": "string",
                    "enum": ["papers", "patents", "eastern", "both"],
                    "default": "papers",
                    "description": (
                        "What to search. 'papers' = arXiv + Semantic Scholar; "
                        "'patents' = Lens.org (needs LENS_API_TOKEN env); "
                        "'eastern' = Soviet/Russian, Japanese and scanned-"
                        "handbook sources (CyberLeninka OAI, NASA Technical "
                        "Translations, J-STAGE, Internet Archive) that the "
                        "Western indexes do not cover — use it for Soviet-era "
                        "alloy, rocket-engine and qualification literature; "
                        "'both' = run all of them sequentially and merge."
                    ),
                },
                "eastern_sources": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": (
                        "Optional override for the `eastern` backend list "
                        "(default: cyberleninka, ntrs_translations, jstage, "
                        "internet_archive). Naming a gated source (elibrary, "
                        "cnki, wanfang) returns the credential it needs rather "
                        "than an empty list."
                    ),
                },
                "max_results": {
                    "type": "integer",
                    "description": "Max items per source (default 20).",
                    "default": 20,
                },
                "sources": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": (
                        "Optional override for the `papers` backend list "
                        "(default: ['arxiv', 'semantic_scholar']). Ignored "
                        "when source='patents'."
                    ),
                },
            },
            "required": ["query"],
            "additionalProperties": False,
        },
        func=_prior_art_search,
    ))

    # Backward-compat aliases — same behaviour as before, narrower
    # NOTE: literature_search and patent_search aliases were removed in
    # Round 6 cleanup. Both functionalities are accessible via
    # prior_art_search(source='papers'|'patents'). The aliases existed
    # only to ease migration; keeping them inflated the embedding
    # retrieval surface (Stage 2.1) without adding capability. Any
    # callers should switch to prior_art_search.
