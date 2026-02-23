"""Tests for data tools."""
import sys
import pytest
from unittest.mock import patch, MagicMock
from app.tools.data import create_data_tools
from app.tools.base import ToolRegistry


class TestSearchOPTIMADETool:
    def test_tool_registered(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "search_optimade" in names

    def test_search_by_elements(self):
        mock_client_cls = MagicMock()
        mock_client = mock_client_cls.return_value
        mock_client.get.return_value = {"mp": {"data": [{"id": "mp-1", "attributes": {"chemical_formula_descriptive": "Si"}}]}}
        mock_optimade = MagicMock()
        mock_optimade.client.OptimadeClient = mock_client_cls
        with patch.dict(sys.modules, {"optimade": mock_optimade, "optimade.client": mock_optimade.client}):
            registry = ToolRegistry()
            create_data_tools(registry)
            tool = registry.get("search_optimade")
            result = tool.execute(filter_string='elements HAS "Si"', providers=["mp"], max_results=5)
            assert "results" in result or "error" in result

    def test_search_optimade_schema(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        tool = registry.get("search_optimade")
        assert "filter_string" in tool.input_schema["properties"]


class TestQueryMPTool:
    def test_tool_registered(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "query_materials_project" in names
