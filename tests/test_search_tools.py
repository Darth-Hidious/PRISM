"""Tests for the search tools.

After Round 6: the literature_search and patent_search Tool aliases were
removed. Both functionalities live behind prior_art_search(source=…). The
private _literature_search / _patent_search helpers are preserved for
direct testing because prior_art_search dispatches into them.
"""
import pytest
from unittest.mock import patch, MagicMock
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
    @patch("app.tools.data_collectors.literature_collector.LiteratureCollector.collect")
    def test_returns_results(self, mock_collect):
        mock_collect.return_value = [
            {"source": "arxiv", "title": "Paper 1"},
            {"source": "semantic_scholar", "title": "Paper 2"},
        ]
        result = _literature_search(query="tungsten alloy")
        assert result["count"] == 2
        assert result["source"] == "literature"
        assert len(result["results"]) == 2

    @patch("app.tools.data_collectors.literature_collector.LiteratureCollector.collect")
    def test_empty_results(self, mock_collect):
        mock_collect.return_value = []
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
