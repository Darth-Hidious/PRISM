"""A search must say which databases it actually asked.

Measured 2026-09-03 on a live run: `prior_art_search` returned fourteen
papers and the UI's source table showed

    SOURCE NOT REPORTED BY prior_art_search

The tool knows perfectly well where the results came from — the retrieval
engine hands it a per-source status list with a name, a state, a count, a
cache flag and an error — and it collapsed that into a display string and
emitted nothing the source table could read. So the reader saw fourteen
papers with no way to tell whether they came from one database or five,
which of them timed out, or how much of the literature was never asked.

That is the difference between a result and a citable result.

A blank query keeps every branch off the network: the literature impl
returns early, the patent branch refuses before building a client, and the
eastern collector gets nothing to fetch.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from app.tools.search import _literature_search_impl, _prior_art_search


def _sources(out):
    assert isinstance(out.get("sources"), list), (
        "the tool must declare a sources array the source table can read; "
        f"got {out.get('sources')!r}"
    )
    return out["sources"]


def test_every_declared_source_names_itself_and_what_it_returned():
    """Each source it asked is named, with what it gave back."""
    outcome = {
        "source_status": [
            {"source": "arxiv", "status": "ok", "count": 8, "cache_hit": False},
            {"source": "semantic_scholar", "status": "ok", "count": 6, "cache_hit": True},
            {"source": "chemrxiv", "status": "timeout", "error": "deadline exceeded"},
            {"source": "crossref", "status": "error", "error": "503 from upstream"},
        ]
    }
    declared = _literature_search_impl.declare_sources(outcome)

    by_name = {row["source"]: row for row in declared}
    assert set(by_name) == {"arxiv", "semantic_scholar", "chemrxiv", "crossref"}

    arxiv = by_name["arxiv"]
    assert arxiv["count"] == 8
    assert arxiv["status"] == "ok"
    assert arxiv["kind"], "a source must say what KIND of data it returned"
    assert arxiv["fetched"], "a source must say when it was fetched"

    # A cache hit is a different fact from a live read, and the reader is
    # entitled to know which one they are looking at.
    assert by_name["semantic_scholar"]["record"]["cache_hit"] is True

    # A source that failed is DECLARED, with its reason. Dropping it would
    # turn "four databases, two answered" into "two databases".
    assert by_name["chemrxiv"]["status"] == "timeout"
    assert "deadline" in by_name["chemrxiv"]["record"]["error"]
    assert by_name["crossref"]["status"] == "error"
    assert "503" in by_name["crossref"]["record"]["error"]

    # A source that failed returned nothing, and must not read as zero hits.
    assert by_name["chemrxiv"]["count"] is None, (
        "a source that timed out did not return zero results — it returned "
        "no answer, and 0 would be read as 'searched, found nothing'"
    )


def test_a_branch_that_was_never_searched_declares_no_sources():
    """Absence of evidence is not evidence of absence, in the table too."""
    out = _prior_art_search(query="", source="papers", max_results=1)
    named = {row["source"] for row in _sources(out)}
    assert not any("patent" in name.lower() for name in named), (
        "the patent branch was never consulted, so it must not appear in the "
        f"source table as though it had been: {named}"
    )


def test_the_literature_branch_carries_its_sources_up(monkeypatch):
    """Declaring them inside the impl is no use if the tool drops them.

    Without this, removing the line that lifts `sources` out of the
    literature branch left every test green while the source table went
    blank again — which is exactly the bug this file exists to prevent.
    """
    import app.tools.search as search

    declared = [
        {
            "source": "arxiv",
            "kind": "peer-reviewed literature metadata",
            "count": 3,
            "fetched": "2026-09-03T10:00:00+00:00",
            "status": "ok",
            "record": {"status": "ok"},
        }
    ]
    monkeypatch.setattr(
        search,
        "_literature_search_impl",
        lambda **kwargs: {
            "results": [],
            "count": 0,
            "sources": declared,
            "source_status": {},
            "relevance": None,
        },
    )

    out = search._prior_art_search(query="anything", source="papers", max_results=1)

    assert out["sources"] == declared, (
        "the tool must carry the branch's declared sources into its result; "
        f"got {out['sources']!r}"
    )


def test_the_search_result_itself_carries_an_evidence_class(monkeypatch):
    """Each record was stamped; the result was not, so the card read [unclassified].

    Bibliographic records from named databases are literature evidence:
    the producer ceiling for literature extraction is "research".
    """
    import app.tools.search as search

    monkeypatch.setattr(
        search,
        "_literature_search_impl",
        lambda **kwargs: {"results": [], "count": 0, "sources": [], "source_status": {}, "relevance": None},
    )
    out = search._prior_art_search(query="anything", source="papers", max_results=1)
    assert out.get("evidence_class") == "research", out.get("evidence_class")
    assert out.get("evidence_color") == "orange"


def test_the_eastern_branch_declares_its_sources(monkeypatch):
    """The eastern branch put its per-source outcome in `eastern_source_status`
    only, so the source table showed nothing for a Russian/Chinese search — the
    same blank the papers branch had. Every eastern source consulted is a row:
    ok with a count; blocked/skipped/timeout/error with the reason and no count."""
    import app.tools.search as search

    monkeypatch.setattr(search, "_eastern_search_impl", lambda **kw: {
        "results": [], "count": 0, "source": "eastern_literature",
        "source_status": {
            "openalex:zh": "ok (5 of 2123; language:zh; query in zh)",
            "cyberleninka": "skipped: needs a Russian query — pass queries={'ru': …}",
            "cnki": "blocked: CNKI (中国知网): licence required. Not collected.",
            "jstage": "timeout: no answer within 45s",
            "internet_archive": "error: HTTPError: 503",
        },
    })
    out = search._prior_art_search(query="高温合金 涂层", source="eastern", max_results=5)
    rows = {r["source"]: r for r in out["sources"]}
    assert set(rows) == {"openalex:zh", "cyberleninka", "cnki", "jstage", "internet_archive"}
    assert rows["openalex:zh"]["status"] == "ok" and rows["openalex:zh"]["count"] == 5
    for name, state in (("cyberleninka", "skipped"), ("cnki", "blocked"),
                        ("jstage", "timeout"), ("internet_archive", "error")):
        assert rows[name]["status"] == state, rows[name]
        assert rows[name]["count"] is None, "no answer is not zero results"
        assert rows[name]["record"]["error"], rows[name]
    assert all(r["kind"] for r in out["sources"])
    # The model's translations are passed through to the collector.
    seen = {}
    monkeypatch.setattr(search, "_eastern_search_impl",
                        lambda **kw: seen.update(kw) or {"results": [], "count": 0, "source_status": {}})
    search._prior_art_search(query="x", source="eastern", queries={"zh": "高温合金"})
    assert seen["queries"] == {"zh": "高温合金"}


def test_the_eastern_branch_surfaces_what_a_human_must_do(monkeypatch):
    """A licence wall the collector reports must reach the tool result as
    `needs_human` — that is what the agent loop announces to the human."""
    import app.tools.search as search

    task = {"source": "cnki", "what": "an institutional CNKI licence",
            "url": "https://oversea.cnki.net/", "reason": "robots.txt disallows /; licence required"}
    monkeypatch.setattr(search, "_eastern_search_impl", lambda **kw: {
        "results": [], "count": 0, "source": "eastern_literature",
        "source_status": {"cnki": "blocked: licence required. Not collected."},
        "needs_human": [task],
    })
    out = search._prior_art_search(query="高温合金", source="eastern")
    assert out["needs_human"] == [task]
