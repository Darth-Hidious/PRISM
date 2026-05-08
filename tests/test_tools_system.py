"""Tests for system tools.

After Round 4 batch 2: `read_file`, `write_file`, and `edit_file` were
collapsed into a single `file(action=…)` tool. The sandbox enforcement
(_is_safe_path / _ALLOWED_BASE) is unchanged.
"""
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
        # web_search and show_scratchpad still standalone
        assert "web_search" in names
        # Unified file tool replaced read_file/write_file/edit_file
        assert "file" in names
        assert "read_file" not in names
        assert "write_file" not in names
        assert "edit_file" not in names

    def test_action_enum_advertises_three_actions(self):
        registry = ToolRegistry()
        create_system_tools(registry)
        tool = registry.get("file")
        actions = tool.input_schema["properties"]["action"]["enum"]
        assert set(actions) == {"read", "write", "edit"}

    def test_missing_action(self):
        registry = ToolRegistry()
        create_system_tools(registry)
        tool = registry.get("file")
        result = tool.execute()
        assert "error" in result and "Missing 'action'" in result["error"]

    def test_unknown_action(self):
        registry = ToolRegistry()
        create_system_tools(registry)
        tool = registry.get("file")
        result = tool.execute(action="bogus")
        assert "error" in result and "Unknown action" in result["error"]

    def test_read_action(self, tmp_path):
        registry = ToolRegistry()
        create_system_tools(registry)
        f = tmp_path / "test.txt"
        f.write_text("hello world")
        tool = registry.get("file")
        with patch("app.tools.system._ALLOWED_BASE", tmp_path.resolve()):
            result = tool.execute(action="read", path=str(f))
        assert result["content"] == "hello world"
        assert result["path"] == str(f.resolve())
        assert result["size_bytes"] == len("hello world".encode("utf-8"))

    def test_read_requires_path(self):
        registry = ToolRegistry()
        create_system_tools(registry)
        tool = registry.get("file")
        result = tool.execute(action="read")
        assert "error" in result
        assert "requires `path`" in result["error"]

    def test_read_file_not_found(self):
        registry = ToolRegistry()
        create_system_tools(registry)
        tool = registry.get("file")
        result = tool.execute(action="read", path="/nonexistent/path.txt")
        assert "error" in result

    def test_write_action(self, tmp_path):
        registry = ToolRegistry()
        create_system_tools(registry)
        f = tmp_path / "out.txt"
        tool = registry.get("file")
        with patch("app.tools.system._ALLOWED_BASE", tmp_path.resolve()):
            result = tool.execute(
                action="write", path=str(f), content="written by prism",
            )
        assert result["success"] is True
        assert result["path"] == str(f.resolve())
        assert result["size_bytes"] == len("written by prism".encode("utf-8"))
        assert f.read_text() == "written by prism"

    def test_write_requires_path_and_content(self):
        registry = ToolRegistry()
        create_system_tools(registry)
        tool = registry.get("file")
        result = tool.execute(action="write")
        assert "error" in result and "requires `path`" in result["error"]
        result = tool.execute(action="write", path="/x")
        assert "error" in result and "requires `content`" in result["error"]

    def test_edit_action(self, tmp_path):
        registry = ToolRegistry()
        create_system_tools(registry)
        f = tmp_path / "edit.txt"
        f.write_text("hello world")
        tool = registry.get("file")
        with patch("app.tools.system._ALLOWED_BASE", tmp_path.resolve()):
            result = tool.execute(
                action="edit", path=str(f),
                old_text="world", new_text="prism",
            )
        assert result["success"] is True
        assert result["path"] == str(f.resolve())
        assert result["replacements"] == 1
        assert f.read_text() == "hello prism"

    def test_edit_requires_path_old_new(self):
        registry = ToolRegistry()
        create_system_tools(registry)
        tool = registry.get("file")
        result = tool.execute(action="edit")
        assert "error" in result and "requires `path`" in result["error"]
        result = tool.execute(action="edit", path="/x")
        assert "error" in result and "requires `old_text` and `new_text`" in result["error"]

    def test_edit_multiple_matches_requires_replace_all(self, tmp_path):
        registry = ToolRegistry()
        create_system_tools(registry)
        f = tmp_path / "edit-many.txt"
        f.write_text("x\nx\n")
        tool = registry.get("file")
        with patch("app.tools.system._ALLOWED_BASE", tmp_path.resolve()):
            result = tool.execute(
                action="edit", path=str(f),
                old_text="x", new_text="y",
            )
        assert "error" in result
        assert "multiple locations" in result["error"]
        assert result["match_count"] == 2

    def test_read_path_traversal_blocked(self, tmp_path):
        """Prevent reading files outside allowed directory."""
        registry = ToolRegistry()
        create_system_tools(registry)
        outside = tmp_path / "sensitive.txt"
        outside.write_text("SECRET")
        tool = registry.get("file")
        # Allowed base is cwd, not tmp_path → outside should be denied
        result = tool.execute(action="read", path=str(outside))
        assert "error" in result
        assert "Access denied" in result["error"]

    def test_write_path_traversal_blocked(self, tmp_path):
        """Prevent writing files outside allowed directory."""
        registry = ToolRegistry()
        create_system_tools(registry)
        outside = tmp_path / "malicious.txt"
        tool = registry.get("file")
        result = tool.execute(action="write", path=str(outside), content="MALWARE")
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
