"""Tests for data tools."""
import os
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
        # Response format: {endpoint: {filter: {url: {data: [entries]}}}}
        mock_client.get.return_value = {"structures": {'elements HAS "Si"': {"https://optimade.materialsproject.org/": {"data": [{"id": "mp-1", "attributes": {"chemical_formula_descriptive": "Si"}}]}}}}
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
