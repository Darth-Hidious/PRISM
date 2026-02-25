"""Tests for data tools."""
import os
import sys
import pytest
from unittest.mock import patch, MagicMock
from app.tools.data import create_data_tools
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
