"""Tests for literature_search and patent_search tools."""
import pytest
from unittest.mock import patch, MagicMock
from app.tools.base import ToolRegistry
from app.tools.search import create_search_tools, _literature_search, _patent_search


class TestCreateSearchTools:
    def test_registers_both_tools(self):
        reg = ToolRegistry()
        create_search_tools(reg)
        names = [t.name for t in reg.list_tools()]
        assert "literature_search" in names
        assert "patent_search" in names

    def test_literature_search_tool_schema(self):
        reg = ToolRegistry()
        create_search_tools(reg)
        tool = reg.get("literature_search")
        assert tool.input_schema["required"] == ["query"]
        assert "query" in tool.input_schema["properties"]
        assert "max_results" in tool.input_schema["properties"]
        assert "sources" in tool.input_schema["properties"]

    def test_patent_search_tool_schema(self):
        reg = ToolRegistry()
        create_search_tools(reg)
        tool = reg.get("patent_search")
        assert tool.input_schema["required"] == ["query"]
        assert "query" in tool.input_schema["properties"]
        assert "max_results" in tool.input_schema["properties"]


class TestLiteratureSearchFunc:
    @patch("app.data.literature_collector.LiteratureCollector.collect")
    def test_returns_results(self, mock_collect):
        mock_collect.return_value = [
            {"source": "arxiv", "title": "Paper 1"},
            {"source": "semantic_scholar", "title": "Paper 2"},
        ]
        result = _literature_search(query="tungsten alloy")
        assert result["count"] == 2
        assert result["query"] == "tungsten alloy"
        assert len(result["results"]) == 2

    @patch("app.data.literature_collector.LiteratureCollector.collect")
    def test_empty_results(self, mock_collect):
        mock_collect.return_value = []
        result = _literature_search(query="")
        assert result["count"] == 0
        assert result["results"] == []


class TestPatentSearchFunc:
    @patch("app.data.patent_collector.PatentCollector.collect")
    def test_returns_results(self, mock_collect):
        mock_collect.return_value = [
            {"source": "lens_patents", "title": "Patent 1"},
        ]
        result = _patent_search(query="alloy coating")
        assert result["count"] == 1
        assert result["query"] == "alloy coating"

    @patch("app.data.patent_collector.PatentCollector.collect")
    def test_no_token_empty(self, mock_collect):
        mock_collect.return_value = []
        result = _patent_search(query="alloy")
        assert result["count"] == 0
        assert result["results"] == []
