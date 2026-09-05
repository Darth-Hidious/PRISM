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
import json
import os
import re
import shutil
from pathlib import Path

from app.tools import spawn
from app.tools.base import Tool, ToolRegistry
from app.tools.evidence import EvidenceSource, stamp_evidence
from app.tools._provenance import utc_now_iso


def _compact_abstract(text, limit: int = 400) -> str:
    """Trim abstracts so a 20-result payload stays well under the agent's
    tool-result budget — a full-abstract payload got truncated to its first
    entries, making every query look like it returned the same thing."""
    text = (text or "").strip()
    return text if len(text) <= limit else text[:limit].rsplit(" ", 1)[0] + "…"


def _resolve_prism_binary() -> str | None:
    """Find the `prism` executable that hosts the Rust retrieval engine.

    Resolution order: PRISM_BINARY env (same convention as _provision.py),
    PATH, then an in-tree build. Returns None when nothing exists — the
    caller turns that into an honest error, never a fallback implementation.
    """
    env = os.environ.get("PRISM_BINARY")
    if env:
        return env
    found = shutil.which("prism")
    if found:
        return found
    repo_root = Path(__file__).resolve().parents[2]
    for candidate in ("target/release/prism", "target/debug/prism"):
        path = repo_root / candidate
        if path.exists():
            return str(path)
    return None


def _literature_search_impl(**kwargs) -> dict:
    """Delegate to the Rust retrieval engine (`prism papers search`).

    The old Python arXiv/Semantic Scholar fetcher was deleted when the Rust
    engine replaced it: there is exactly one literature retrieval engine, and
    this adapter keeps the agent-visible contract stable. Every record comes
    back stamped with the literature evidence class (research/orange) — a
    retrieval record is never stronger than that.
    """
    query = kwargs.get("query", "")
    max_results = int(kwargs.get("max_results", 20))
    sources = kwargs.get("sources")
    if not query:
        return {"results": [], "count": 0, "source": "literature",
                "source_status": {}}

    binary = _resolve_prism_binary()
    if not binary:
        return {
            "results": [],
            "count": 0,
            "source": "literature",
            "source_status": {},
            "error": (
                "prism binary not found — the papers backend is the Rust "
                "retrieval engine (`prism papers search`); no Python fallback "
                "exists by design. Set PRISM_BINARY or install prism."
            ),
        }

    argv = [binary, "papers", "search", "--query", query,
            "--limit", str(max_results)]
    if sources:
        argv += ["--sources", ",".join(sources)]
    try:
        proc = spawn.run(
            argv, capture_output=True, text=True, timeout=180, check=False,
        )
    except Exception as exc:
        return {
            "results": [], "count": 0, "source": "literature",
            "source_status": {},
            "error": f"papers engine failed to run: {type(exc).__name__}: {exc}",
        }
    if proc.returncode != 0:
        stderr = (proc.stderr or "").strip()[:400]
        return {
            "results": [], "count": 0, "source": "literature",
            "source_status": {},
            "error": f"papers engine exited {proc.returncode}: {stderr}",
        }
    try:
        outcome = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return {
            "results": [], "count": 0, "source": "literature",
            "source_status": {},
            "error": "papers engine produced unparseable output",
        }

    results = []
    for paper in outcome.get("papers", []):
        record = dict(paper)
        # Backward-compatible field aliases for the pre-engine contract.
        record["abstract"] = _compact_abstract(record.pop("abstract_text", None))
        record["type"] = "paper"
        stamp_evidence(record, EvidenceSource.LITERATURE_EXTRACTION)
        results.append(record)

    # Engine status list -> legacy per-source dict. Count ok sources by the
    # engine's state, not by the rendered string: a fully-cached zero-hit
    # success renders "cache (0 results)" and a startswith("ok") check would
    # report it as a failure.
    declared_sources = _literature_search_impl.declare_sources(outcome)
    source_status = {}
    ok_sources = 0
    for status in outcome.get("source_status", []):
        name = status.get("source", "unknown")
        state = status.get("status", "error")
        if state == "ok":
            ok_sources += 1
            note = "cache" if status.get("cache_hit") else "ok"
            source_status[name] = f"{note} ({status.get('count', 0)} results)"
        elif state == "timeout":
            source_status[name] = f"timeout: {status.get('error') or 'deadline'}"
        else:
            source_status[name] = f"error: {status.get('error') or 'unknown'}"

    out = {
        "results": results,
        # Which databases were actually asked, and what each one gave back.
        # The UI's source table reads this; without it a reader sees results
        # with no way to tell one database from five, or which never answered.
        "sources": declared_sources,
        "count": len(results),
        "source": "literature",
        "source_status": source_status,
        # Preserve the engine's complete relevance accounting verbatim. In
        # particular, `unavailable`/`failed` means these papers were returned
        # unfiltered, while `applied` can carry an exact dropped count plus
        # bounded examples. Losing this at the adapter boundary would turn an
        # honest Rust outcome back into a misleadingly clean agent result.
        "relevance": outcome.get("relevance"),
        "duplicates_merged": outcome.get("duplicates_merged", 0),
        "engine_elapsed_ms": outcome.get("elapsed_ms"),
    }
    # Nothing was retrieved AND every source failed: that is a fault, not an
    # empty result. Surface it; keep the results list honest (empty). An
    # honest zero-hit success (cached or not) must never trip this.
    if not results and source_status and ok_sources == 0:
        out["error"] = (
            "no papers retrieved because every source failed; see source_status"
        )
    # Bibliographic records from named databases are literature evidence.
    stamp_evidence(out, EvidenceSource.LITERATURE_EXTRACTION)
    return out


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
        queries=kwargs.get("queries"),
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
    and the legacy `patent_search` alias.

    Everything that can go wrong in the collector — no backend configured, an
    extract that could not be built, a byte ceiling that refused the job —
    raises `CollectorConfigError`. It is deliberately NOT caught here: the
    caller turns it into `patents_error`, because an empty list would be read
    as "nobody has patented this".
    """
    from app.tools.data_collectors.patent_collector import PatentCollector
    collector = PatentCollector()
    results = collector.collect(**kwargs)
    return {
        "results": results,
        "count": len(results),
        "source": "patents",
        # Which corpus produced this. A zero has to be attributable — an
        # unattributed 0 is indistinguishable from a source that never ran.
        "backend": getattr(collector, "last_backend", None),
    }


def _declare_literature_sources(outcome: dict) -> list[dict]:
    """The engine's per-source status, in the shape the source table reads.

    Every source consulted is named — including the ones that failed, because
    dropping them turns "four databases, two answered" into "two databases".
    A source that timed out or errored reports `count: None`: it did not
    return zero results, it returned no answer, and a plain 0 reads as
    "searched, found nothing".
    """
    fetched = utc_now_iso()
    declared = []
    for status in outcome.get("source_status", []) or []:
        state = status.get("status", "error")
        answered = state == "ok"
        record = {"status": state}
        if status.get("error"):
            record["error"] = status["error"]
        if "cache_hit" in status:
            record["cache_hit"] = bool(status.get("cache_hit"))
        if status.get("endpoint"):
            record["endpoint"] = status["endpoint"]
        declared.append(
            {
                "source": status.get("source", "unknown"),
                "kind": "peer-reviewed literature metadata",
                "count": status.get("count", 0) if answered else None,
                "fetched": fetched,
                "status": state,
                "record": record,
            }
        )
    return declared


_literature_search_impl.declare_sources = _declare_literature_sources


def _declare_eastern_sources(status: dict) -> list[dict]:
    """The eastern collector's per-source lines, in the shape the source table
    reads. `ok (N …)` is an answer with a count; blocked / skipped / timeout /
    error carry the reason and `count: None` — no answer is not zero results.
    Without this the table showed nothing for a Russian or Chinese search."""
    fetched = utc_now_iso()
    rows = []
    for name, line in (status or {}).items():
        line = str(line or "")
        if line.startswith("ok"):
            m = re.match(r"ok \((\d+)", line)
            rows.append({
                "source": name,
                "kind": "non-Western literature metadata",
                "count": int(m.group(1)) if m else 0,
                "fetched": fetched,
                "status": "ok",
                "record": {"status": "ok", "detail": line},
            })
            continue
        state = line.split(":", 1)[0].strip().lower()
        if state not in ("blocked", "skipped", "timeout", "error"):
            state = "error"
        rows.append({
            "source": name,
            "kind": "non-Western literature metadata",
            "count": None,
            "fetched": fetched,
            "status": state,
            "record": {"status": state, "error": line or "no status reported"},
        })
    return rows


def _prior_art_search(**kwargs) -> dict:
    """Federated prior-art lookup.

    `source`: "papers" (default), "patents", "eastern", or "both".

    For "both", we run the backends in sequence (sequential is fine here —
    no call is heavy enough to need parallelism, and keeping the
    implementations independent means a Lens API failure doesn't prevent
    literature results from flowing).

    Result shape stays uniform regardless of source:
      { "papers": [...], "patents": [...], "eastern": [...],
        "counts": {"papers": N, "patents": M, "eastern": K},
        "searched": ["papers"] }
    Empty arrays for unrequested sources so consumers don't have to
    null-check.

    A count of `0` means SEARCHED AND FOUND NOTHING. A source that was not
    consulted counts `null`, and `searched` lists the ones that were.

    Measured 2026-08-20: called with `source="papers"`, this returned
    `counts: {papers: 39, patents: 0, eastern: 0}`. Both zeros were for
    backends that were never contacted, and nothing in the payload said so —
    so the honest reading ("I did not look for patents") and the false one
    ("there are no relevant patents") are the same bytes. The model had just
    been told the patent backend was unconfigured, and a plain `0` invites it
    to conclude the literature is settled. Absence of evidence must not be
    encoded as evidence of absence.
    """
    query = kwargs.get("query", "")
    max_results = kwargs.get("max_results", 20)
    source = (kwargs.get("source") or "papers").lower()

    out: dict = {
        "papers": [],
        "patents": [],
        "eastern": [],
        # Only branches that were actually consulted add entries here, so the
        # table never shows a database that was never asked.
        "sources": [],
        # `None` until a backend actually answers; each branch below sets its
        # own count, and any left `None` were not searched.
        "counts": {"papers": None, "patents": None, "eastern": None},
        "searched": [],
        "query": query,
    }

    if source in ("eastern", "both"):
        out["searched"].append("eastern")
        out["counts"]["eastern"] = 0
        try:
            east = _eastern_search_impl(
                query=query,
                max_results=max_results,
                sources=kwargs.get("eastern_sources"),
                queries=kwargs.get("queries"),
            )
            out["eastern"] = east.get("results", [])
            out["counts"]["eastern"] = east.get("count", 0)
            # Gated sources (CNKI, eLIBRARY, Wanfang) report here by name so
            # "no Chinese results" is never mistaken for "nothing published".
            out["eastern_source_status"] = east.get("source_status", {})
            out["sources"].extend(_declare_eastern_sources(out["eastern_source_status"]))
        except Exception as exc:
            out["eastern_error"] = str(exc)

    if source in ("papers", "both"):
        out["searched"].append("papers")
        out["counts"]["papers"] = 0
        try:
            lit = _literature_search_impl(
                query=query,
                max_results=max_results,
                sources=kwargs.get("sources"),  # arxiv / semantic_scholar override
            )
            out["papers"] = lit.get("results", [])
            out["sources"].extend(lit.get("sources", []) or [])
            out["counts"]["papers"] = lit.get("count", 0)
            out["source_status"] = lit.get("source_status", {})
            # Relevance applies only to the literature branch of this
            # federated result, so keep the provenance explicit in the key.
            # This is the same report emitted by `prism papers search`.
            out["papers_relevance"] = lit.get("relevance")
            # A retrieval fault (engine missing, every source down) is not an
            # empty result — keep it visible to the agent.
            if lit.get("error"):
                out["papers_error"] = lit["error"]
        except Exception as exc:
            out["papers_error"] = str(exc)

    if source in ("patents", "both"):
        out["searched"].append("patents")
        out["counts"]["patents"] = 0
        if not (query or "").strip():
            # The collector answers a blank query with [], which at this level
            # is a silent zero: measured once as counts.patents = 0 with no
            # patents_error at all. Name it instead.
            out["patents_error"] = (
                "prior_art_search(source='patents') needs a non-empty query"
            )
        else:
            try:
                pat = _patent_search_impl(query=query, max_results=max_results)
                out["patents"] = pat.get("results", [])
                out["counts"]["patents"] = pat.get("count", 0)
                # Say which corpus answered, so a zero is attributable rather
                # than merely absent.
                out["patents_backend"] = pat.get("backend")
            except Exception as exc:
                # No backend configured, an extract that could not be built, a
                # byte ceiling that refused the job, an expired Lens token —
                # every one of them is a FAILED search, never an empty one.
                # Surfaced per-source so literature results still flow.
                out["patents_error"] = str(exc)

    stamp_evidence(out, EvidenceSource.LITERATURE_EXTRACTION)
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
        # Reads a federated/local source and returns what it found. Spends
        # nothing, writes nothing, so the human is not asked. Declared
        # explicitly: an undeclared tool is gated.
        requires_approval=False,
        description=(
            "Federated prior-art search across scientific literature "
            "(arXiv, Semantic Scholar), patents (whichever patent backend the "
            "operator configured), AND non-Western "
            "sources (OpenAlex filtered to Chinese/Russian/Japanese-language "
            "literature, CyberLeninka's Russian aerospace-materials journals, "
            "NASA Technical Translations of Soviet work, J-STAGE Japanese "
            "metallurgy, scanned handbooks on Internet Archive). For 'eastern' "
            "or 'both', TRANSLATE the query yourself and pass `queries` "
            "({'ru': …, 'zh': …, 'ja': …}): each source only receives a "
            "language it indexes, and a source with no usable query is "
            "reported as skipped with the language it needs. Use "
            "this for any 'what has been published / patented about X?' "
            "question. The `source` flag selects 'papers' (default), "
            "'patents', 'eastern', or 'both'. Returns a uniform shape "
            "{ papers, patents, eastern, counts } — empty arrays for "
            "unrequested sources. Per-source failures (e.g. Lens auth "
            "missing, CNKI licence required) are reported in `papers_error` "
            "/ `patents_error` / `eastern_source_status` without failing the "
            "whole call, so the agent gets partial results and knows which "
            "source was skipped rather than genuinely empty. The "
            "`papers_relevance` report also says whether literature results "
            "were filtered, how many off-topic papers were dropped (with "
            "examples), or whether embeddings were unavailable and the "
            "papers therefore came back unfiltered. This is also the "
            "literature cross-check for candidates that came out of "
            "materials_search or predict: cite what comes back (DB id + "
            "paper DOI), never propose a composition without a traceable "
            "source."
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
                        "'patents' = the configured patent backend (a hosted "
                        "service, the caller's own BigQuery project, or "
                        "Lens.org — it is selected from whichever credentials "
                        "are set, and says so in `patents_backend`; with none "
                        "set it reports the routes in `patents_error` rather "
                        "than returning an empty list); "
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
                        "(default: openalex, cyberleninka, ntrs_translations, "
                        "jstage, internet_archive). Naming a gated source "
                        "(elibrary, cnki, wanfang) returns the credential it "
                        "needs rather than an empty list."
                    ),
                },
                "queries": {
                    "type": "object",
                    "additionalProperties": {"type": "string"},
                    "description": (
                        "The query translated by you into the languages of the "
                        "eastern sources, keyed by ISO code: {'ru': 'жаропрочный "
                        "сплав покрытие', 'zh': '高温合金 涂层', 'ja': '耐熱合金 "
                        "コーティング'}. Russian reaches CyberLeninka and "
                        "OpenAlex(ru); Chinese reaches OpenAlex(zh) and archive "
                        "scans; Japanese reaches J-STAGE and OpenAlex(ja). "
                        "Without a translation the source is skipped and says "
                        "so — supply it and search again."
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
