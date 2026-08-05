"""Tests for the search tools.

After Round 6: the literature_search and patent_search Tool aliases were
removed. Both functionalities live behind prior_art_search(source=…). The
private _literature_search / _patent_search helpers are preserved for
direct testing because prior_art_search dispatches into them.
"""
import json
from unittest.mock import MagicMock, patch
from app.tools.base import ToolRegistry
from app.tools.search import create_search_tools, _literature_search, _patent_search


class TestCreateSearchTools:
    def test_registers_prior_art_search(self):
        reg = ToolRegistry()
        create_search_tools(reg)
        names = [t.name for t in reg.list_tools()]
        # Unified entry — replaced the two aliases
        assert "prior_art_search" in names
        # Old aliases must be gone (they inflated the retrieval surface
        # without adding capability)
        assert "literature_search" not in names
        assert "patent_search" not in names

    def test_prior_art_search_schema(self):
        reg = ToolRegistry()
        create_search_tools(reg)
        tool = reg.get("prior_art_search")
        assert "query" in tool.input_schema["required"]
        assert "query" in tool.input_schema["properties"]
        assert "source" in tool.input_schema["properties"]


class TestLiteratureSearchFunc:
    """`_literature_search` delegates to the Rust retrieval engine
    (`prism papers search`) — these stub the engine process, never the
    network."""

    ENGINE_OUTCOME = json.dumps({
        "papers": [
            {"source": "arxiv", "source_id": "1", "title": "Paper 1",
             "abstract_text": "A1", "url": "u1"},
            {"source": "semantic_scholar", "source_id": "2",
             "title": "Paper 2", "abstract_text": "A2", "url": "u2"},
        ],
        "duplicates_merged": 0,
        "source_status": [
            {"source": "arxiv", "status": "ok", "count": 1,
             "cache_hit": False, "error": None},
            {"source": "semantic_scholar", "status": "ok", "count": 1,
             "cache_hit": False, "error": None},
        ],
        "elapsed_ms": 10.0,
    })

    def _engine_proc(self):
        proc = MagicMock()
        proc.stdout = self.ENGINE_OUTCOME
        proc.stderr = ""
        proc.returncode = 0
        return proc

    @patch("app.tools.search.spawn.run")
    @patch("app.tools.search._resolve_prism_binary",
           return_value="/usr/local/bin/prism")
    def test_returns_results(self, _binary, mock_run):
        mock_run.return_value = self._engine_proc()
        result = _literature_search(query="tungsten alloy")
        assert result["count"] == 2
        assert result["source"] == "literature"
        assert len(result["results"]) == 2
        # Per-source outcomes must reach the caller — a thin result set with
        # a failed source is a different fact from a genuinely empty one.
        assert result["source_status"]["arxiv"] == "ok (1 results)"
        # Every record carries the literature evidence ceiling.
        assert all(r["evidence_class"] == "research" for r in result["results"])

    @patch("app.tools.search.spawn.run")
    @patch("app.tools.search._resolve_prism_binary",
           return_value="/usr/local/bin/prism")
    def test_empty_results(self, _binary, mock_run):
        proc = MagicMock()
        proc.stdout = json.dumps({"papers": [], "duplicates_merged": 0,
                                  "source_status": [], "elapsed_ms": 1.0})
        proc.stderr = ""
        proc.returncode = 0
        mock_run.return_value = proc
        result = _literature_search(query="")
        assert result["count"] == 0
        assert result["results"] == []


class TestPatentSearchFunc:
    @patch("app.tools.data_collectors.patent_collector.PatentCollector.collect")
    def test_returns_results(self, mock_collect):
        mock_collect.return_value = [
            {"source": "lens_patents", "title": "Patent 1"},
        ]
        result = _patent_search(query="alloy coating")
        assert result["count"] == 1
        assert result["source"] == "patents"

    @patch("app.tools.data_collectors.patent_collector.PatentCollector.collect")
    def test_no_token_empty(self, mock_collect):
        mock_collect.return_value = []
        result = _patent_search(query="alloy")
        assert result["count"] == 0
        assert result["results"] == []
