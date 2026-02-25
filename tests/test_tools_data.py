"""Tests for data tools."""
import os
import sys
import pytest
from unittest.mock import patch, MagicMock
from app.tools.data import create_data_tools, _query_omat24
from app.tools.base import ToolRegistry


class TestSearchMaterialsTool:
    def test_tool_registered(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "search_materials" in names

    def test_search_by_elements(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        tool = registry.get("search_materials")
        # The tool uses the federated search engine; verify it handles element args
        result = tool.execute(elements=["Si"], limit=5)
        assert "results" in result or "error" in result

    def test_search_materials_schema(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        tool = registry.get("search_materials")
        assert "elements" in tool.input_schema["properties"]


class TestQueryMPTool:
    def test_tool_registered(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "query_materials_project" in names


class TestExportResultsCSV:
    def test_tool_registered(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "export_results_csv" in names

    def test_export_creates_file(self, tmp_path):
        registry = ToolRegistry()
        create_data_tools(registry)
        tool = registry.get("export_results_csv")
        filepath = str(tmp_path / "test_export.csv")
        results = [{"id": "mp-1", "formula": "Si"}, {"id": "mp-2", "formula": "Ge"}]
        out = tool.execute(results=results, filename=filepath)
        assert out["rows"] == 2
        assert os.path.exists(filepath)
        with open(filepath) as f:
            content = f.read()
        assert "id,formula" in content
        assert "mp-1,Si" in content

    def test_export_empty_results(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        tool = registry.get("export_results_csv")
        out = tool.execute(results=[])
        assert "error" in out

    def test_export_auto_filename(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        registry = ToolRegistry()
        create_data_tools(registry)
        tool = registry.get("export_results_csv")
        out = tool.execute(results=[{"a": 1}])
        assert "filename" in out
        assert out["filename"].startswith("prism_export_")


class TestQueryOMAT24Tool:
    def test_tool_registered(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "query_omat24" in names

    def test_schema_has_elements_and_formula(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        tool = registry.get("query_omat24")
        props = tool.input_schema["properties"]
        assert "elements" in props
        assert "formula" in props
        assert "max_results" in props

    @patch("app.data.omat24_collector.OMAT24Collector")
    def test_returns_results(self, mock_cls):
        mock_collector = MagicMock()
        mock_collector.collect.return_value = [
            {"source": "omat24", "formula": "Fe2O3", "energy": -10.5},
        ]
        mock_cls.return_value = mock_collector

        result = _query_omat24(elements=["Fe", "O"], max_results=10)

        assert result["count"] == 1
        assert result["source"] == "omat24"
        assert result["results"][0]["formula"] == "Fe2O3"

    def test_handles_collector_error(self):
        """If collector raises, tool returns error dict."""
        with patch("app.data.omat24_collector.OMAT24Collector", side_effect=Exception("connection failed")):
            result = _query_omat24(elements=["Si"])
        assert "error" in result
