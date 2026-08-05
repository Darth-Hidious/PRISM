"""Delegation tests: `prior_art_search(source='papers')` must run through
the Rust retrieval engine (`prism papers search`) and every returned record
must carry the literature evidence class.

These fail against the old Python fetch implementation (no evidence stamp,
no delegation) and pass with the new adapter.
"""
import json
from unittest.mock import MagicMock, patch

from app.tools.search import _literature_search_impl, _prior_art_search

ENGINE_JSON = json.dumps({
    "papers": [
        {
            "source": "arxiv",
            "source_id": "2401.12345v1",
            "title": "High entropy alloy phase stability",
            "authors": ["A. Researcher"],
            "year": 2024,
            "published": "2024-01-22T18:00:00Z",
            "doi": "10.1234/hea",
            "external_ids": {"arxiv": "2401.12345v1"},
            "abstract_text": "We study phase stability.",
            "url": "https://arxiv.org/abs/2401.12345v1",
            "fulltext_url": "https://arxiv.org/pdf/2401.12345v1",
            "fulltext_format": "pdf",
            "journal": None,
        }
    ],
    "duplicates_merged": 0,
    "source_status": [
        {"source": "arxiv", "status": "ok", "count": 1, "latency_ms": 900.0,
         "cache_hit": False, "error": None},
        {"source": "semantic_scholar", "status": "error", "count": 0,
         "latency_ms": 400.0, "cache_hit": False,
         "error": "HTTP 429 Too Many Requests"},
    ],
    "elapsed_ms": 950.0,
})


def _mock_spawn(stdout=ENGINE_JSON, returncode=0):
    proc = MagicMock()
    proc.stdout = stdout
    proc.stderr = ""
    proc.returncode = returncode
    return proc


class TestEvidenceStamps:
    @patch("app.tools.search.spawn.run")
    @patch("app.tools.search._resolve_prism_binary", return_value="/usr/local/bin/prism")
    def test_every_paper_carries_research_evidence(self, _bin, mock_run):
        mock_run.return_value = _mock_spawn()
        out = _prior_art_search(query="high entropy alloy", source="papers")
        assert out["counts"]["papers"] == 1
        paper = out["papers"][0]
        # Literature extraction is ORANGE/research and nothing stronger.
        assert paper["evidence_class"] == "research"
        assert paper["evidence_color"] == "orange"

    @patch("app.tools.search.spawn.run")
    @patch("app.tools.search._resolve_prism_binary", return_value="/usr/local/bin/prism")
    def test_paper_shape_is_additive_not_lossy(self, _bin, mock_run):
        mock_run.return_value = _mock_spawn()
        out = _prior_art_search(query="x", source="papers")
        paper = out["papers"][0]
        # Old contract fields survive...
        assert paper["title"] == "High entropy alloy phase stability"
        assert paper["source"] == "arxiv"
        assert paper["abstract"] == "We study phase stability."
        # ...and the richer engine fields are visible too.
        assert paper["doi"] == "10.1234/hea"
        assert paper["fulltext_format"] == "pdf"


class TestHonestSourceStatus:
    @patch("app.tools.search.spawn.run")
    @patch("app.tools.search._resolve_prism_binary", return_value="/usr/local/bin/prism")
    def test_failed_source_is_named_not_dropped(self, _bin, mock_run):
        mock_run.return_value = _mock_spawn()
        out = _prior_art_search(query="x", source="papers")
        status = out["source_status"]
        assert "ok" in status["arxiv"]
        assert "error" in status["semantic_scholar"]
        assert "429" in status["semantic_scholar"]


class TestFailureModes:
    @patch("app.tools.search._resolve_prism_binary", return_value=None)
    def test_missing_binary_is_an_error_not_empty_results(self, _bin):
        out = _literature_search_impl(query="tungsten alloy")
        assert "error" in out
        assert "prism" in out["error"].lower()
        # No fabricated success shape.
        assert out.get("count", 0) == 0

    @patch("app.tools.search.spawn.run")
    @patch("app.tools.search._resolve_prism_binary", return_value="/usr/local/bin/prism")
    def test_engine_nonzero_exit_is_surfaced(self, _bin, mock_run):
        mock_run.return_value = _mock_spawn(stdout="", returncode=2)
        out = _literature_search_impl(query="tungsten alloy")
        assert "error" in out
        assert out.get("count", 0) == 0

    @patch("app.tools.search.spawn.run")
    @patch("app.tools.search._resolve_prism_binary", return_value="/usr/local/bin/prism")
    def test_empty_query_does_not_spawn_the_engine(self, _bin, mock_run):
        out = _literature_search_impl(query="")
        assert out["results"] == []
        mock_run.assert_not_called()

    @patch("app.tools.search.spawn.run")
    @patch("app.tools.search._resolve_prism_binary", return_value="/usr/local/bin/prism")
    def test_sources_override_reaches_the_engine(self, _bin, mock_run):
        mock_run.return_value = _mock_spawn()
        _literature_search_impl(query="x", sources=["arxiv", "doaj"])
        argv = mock_run.call_args[0][0]
        assert "--sources" in argv
        assert argv[argv.index("--sources") + 1] == "arxiv,doaj"
