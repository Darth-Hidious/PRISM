"""Tests for system tools."""
import pytest
from app.tools.system import create_system_tools
from app.tools.base import ToolRegistry


class TestSystemTools:
    def test_creates_registry_with_tools(self):
        registry = ToolRegistry()
        create_system_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "web_search" in names
        assert "read_file" in names
        assert "write_file" in names

    def test_read_file_tool(self, tmp_path):
        registry = ToolRegistry()
        create_system_tools(registry)
        f = tmp_path / "test.txt"
        f.write_text("hello world")
        tool = registry.get("read_file")
        result = tool.execute(path=str(f))
        assert result["content"] == "hello world"

    def test_read_file_not_found(self):
        registry = ToolRegistry()
        create_system_tools(registry)
        tool = registry.get("read_file")
        result = tool.execute(path="/nonexistent/path.txt")
        assert "error" in result

    def test_write_file_tool(self, tmp_path):
        registry = ToolRegistry()
        create_system_tools(registry)
        f = tmp_path / "out.txt"
        tool = registry.get("write_file")
        result = tool.execute(path=str(f), content="written by prism")
        assert result["success"] is True
        assert f.read_text() == "written by prism"
