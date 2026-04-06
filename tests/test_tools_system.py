"""Tests for system tools."""
import pytest
from unittest.mock import patch
from pathlib import Path
from app.tools.system import create_system_tools, _is_safe_path
from app.tools.base import ToolRegistry


class TestSystemTools:
    def test_creates_registry_with_tools(self):
        registry = ToolRegistry()
        create_system_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "web_search" in names
        assert "read_file" in names
        assert "write_file" in names
        assert "edit_file" in names

    def test_read_file_tool(self, tmp_path):
        registry = ToolRegistry()
        create_system_tools(registry)
        f = tmp_path / "test.txt"
        f.write_text("hello world")
        tool = registry.get("read_file")
        # Patch allowed base to tmp_path for testing
        with patch("app.tools.system._ALLOWED_BASE", tmp_path.resolve()):
            result = tool.execute(path=str(f))
        assert result["content"] == "hello world"
        assert result["path"] == str(f.resolve())
        assert result["size_bytes"] == len("hello world".encode("utf-8"))

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
        with patch("app.tools.system._ALLOWED_BASE", tmp_path.resolve()):
            result = tool.execute(path=str(f), content="written by prism")
        assert result["success"] is True
        assert result["path"] == str(f.resolve())
        assert result["size_bytes"] == len("written by prism".encode("utf-8"))
        assert f.read_text() == "written by prism"

    def test_edit_file_tool(self, tmp_path):
        registry = ToolRegistry()
        create_system_tools(registry)
        f = tmp_path / "edit.txt"
        f.write_text("hello world")
        tool = registry.get("edit_file")
        with patch("app.tools.system._ALLOWED_BASE", tmp_path.resolve()):
            result = tool.execute(
                path=str(f),
                old_text="world",
                new_text="prism",
            )
        assert result["success"] is True
        assert result["path"] == str(f.resolve())
        assert result["replacements"] == 1
        assert f.read_text() == "hello prism"

    def test_edit_file_multiple_matches_requires_replace_all(self, tmp_path):
        registry = ToolRegistry()
        create_system_tools(registry)
        f = tmp_path / "edit-many.txt"
        f.write_text("x\nx\n")
        tool = registry.get("edit_file")
        with patch("app.tools.system._ALLOWED_BASE", tmp_path.resolve()):
            result = tool.execute(
                path=str(f),
                old_text="x",
                new_text="y",
            )
        assert "error" in result
        assert "multiple locations" in result["error"]
        assert result["match_count"] == 2

    def test_read_file_path_traversal_blocked(self, tmp_path):
        """Prevent reading files outside allowed directory."""
        registry = ToolRegistry()
        create_system_tools(registry)
        # Create a file outside the allowed base
        outside = tmp_path / "sensitive.txt"
        outside.write_text("SECRET")
        tool = registry.get("read_file")
        # Allowed base is cwd, not tmp_path
        result = tool.execute(path=str(outside))
        assert "error" in result
        assert "Access denied" in result["error"]

    def test_write_file_path_traversal_blocked(self, tmp_path):
        """Prevent writing files outside allowed directory."""
        registry = ToolRegistry()
        create_system_tools(registry)
        outside = tmp_path / "malicious.txt"
        tool = registry.get("write_file")
        result = tool.execute(path=str(outside), content="MALWARE")
        assert "error" in result
        assert "Access denied" in result["error"]


class TestSafePath:
    def test_safe_path_within_base(self, tmp_path):
        with patch("app.tools.system._ALLOWED_BASE", tmp_path.resolve()):
            assert _is_safe_path(str(tmp_path / "file.txt")) is True

    def test_unsafe_path_outside_base(self, tmp_path):
        with patch("app.tools.system._ALLOWED_BASE", tmp_path.resolve()):
            assert _is_safe_path("/etc/passwd") is False

    def test_unsafe_path_traversal(self, tmp_path):
        with patch("app.tools.system._ALLOWED_BASE", tmp_path.resolve()):
            assert _is_safe_path(str(tmp_path / ".." / ".." / "etc" / "passwd")) is False
