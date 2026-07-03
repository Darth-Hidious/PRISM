"""Tests for data tools.

After Round 7:
  - search_materials Tool registration removed (duplicate of materials_search
    in app/tools/search_engine/tools.py). The _search_materials private
    helper is still importable for direct unit testing if needed.
  - import_dataset / export_results_csv Tool registrations removed; both
    folded into the unified `dataset` Tool as `dataset(action='import')` /
    `dataset(action='export')`.
  - query_materials_project remains — different scope (MP-specific deep
    property query, not a federated catalog search).
"""
import os
import sys
import pytest
from unittest.mock import patch, MagicMock
from app.tools.data import create_data_tools, _search_materials, _export_results_csv, _import_dataset
from app.tools.base import ToolRegistry


class TestRegistration:
    def test_only_query_mp_registered(self):
        """Round 7: data.py now registers ONLY query_materials_project.
        The other former data tools moved (search_materials → search_engine,
        import/export → dataset Tool actions)."""
        registry = ToolRegistry()
        create_data_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "query_materials_project" in names
        # Removed in Round 7
        assert "search_materials" not in names
        assert "import_dataset" not in names
        assert "export_results_csv" not in names


class TestQueryMPTool:
    def test_tool_registered(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "query_materials_project" in names

    def test_schema(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        tool = registry.get("query_materials_project")
        # MP-specific schema fields
        assert "formula" in tool.input_schema["properties"]
        assert "material_id" in tool.input_schema["properties"]


class TestSearchMaterialsImpl:
    """The private _search_materials function is still used internally
    (by Skills, by the dataset acquisition workflow). Verify it's
    importable and has the expected signature."""

    def test_callable_with_elements(self):
        # Real call would hit the federated search engine; we just verify
        # the function accepts the expected kwargs without error
        # (it'll return either results or a network error).
        result = _search_materials(elements=["Si"], limit=1)
        assert isinstance(result, dict)
        assert "results" in result or "error" in result


class TestExportResultsCSVImpl:
    """_export_results_csv is now dispatched via dataset(action='export'),
    but the underlying impl remains testable directly."""

    def test_creates_file(self, tmp_path):
        filepath = str(tmp_path / "test_export.csv")
        results = [{"id": "mp-1", "formula": "Si"}, {"id": "mp-2", "formula": "Ge"}]
        out = _export_results_csv(results=results, filename=filepath)
        assert out["rows"] == 2
        assert os.path.exists(filepath)
        with open(filepath) as f:
            content = f.read()
        assert "id,formula" in content
        assert "mp-1,Si" in content

    def test_empty_results(self):
        out = _export_results_csv(results=[])
        assert "error" in out

    def test_auto_filename(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        out = _export_results_csv(results=[{"a": 1}])
        assert "filename" in out
        assert out["filename"].startswith("prism_export_")


class TestDatasetActionImportExport:
    """Verify the new dataset(action='import') and dataset(action='export')
    actions wire correctly through the dataset Tool."""

    def test_import_via_dataset_tool(self, tmp_path):
        from app.tools.dataset import create_dataset_tool

        # Write a small CSV
        csv_path = tmp_path / "alloys.csv"
        csv_path.write_text("formula,band_gap\nSi,1.1\nGe,0.67\n")

        registry = ToolRegistry()
        create_dataset_tool(registry)
        tool = registry.get("dataset")
        result = tool.execute(action="import", file_path=str(csv_path))
        assert "error" not in result, f"unexpected error: {result}"
        assert result.get("rows") == 2 or "dataset_name" in result

    def test_import_requires_file_path(self):
        from app.tools.dataset import create_dataset_tool
        registry = ToolRegistry()
        create_dataset_tool(registry)
        tool = registry.get("dataset")
        result = tool.execute(action="import")
        assert "error" in result
        assert "file_path" in result["error"]

    def test_export_via_dataset_tool(self, tmp_path):
        from app.tools.dataset import create_dataset_tool

        registry = ToolRegistry()
        create_dataset_tool(registry)
        tool = registry.get("dataset")
        out_path = str(tmp_path / "export.csv")
        result = tool.execute(
            action="export",
            results=[{"x": 1, "y": 2}, {"x": 3, "y": 4}],
            filename=out_path,
        )
        assert "error" not in result
        assert os.path.exists(out_path)

    def test_export_requires_results(self):
        from app.tools.dataset import create_dataset_tool
        registry = ToolRegistry()
        create_dataset_tool(registry)
        tool = registry.get("dataset")
        result = tool.execute(action="export")
        assert "error" in result
        assert "results" in result["error"]
