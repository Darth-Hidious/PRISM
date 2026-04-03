"""Tests for skill loading in REPL and autonomous mode."""

from unittest.mock import MagicMock, patch

from app.tools.base import ToolRegistry


class TestSkillLoadingAutonomous:
    def test_make_tools_includes_skills(self):
        from app.agent.autonomous import _make_tools

        tools = _make_tools(enable_mcp=False)

        # Skills should be registered as tools
        tool_names = {t.name for t in tools.list_tools()}
        assert "acquire_materials" in tool_names
        assert "predict_properties" in tool_names
        assert "materials_discovery" in tool_names
        assert "plan_simulations" in tool_names

    def test_make_tools_preserves_existing(self):
        from app.agent.autonomous import _make_tools

        tools = _make_tools(enable_mcp=False)

        # Original tools should also be present
        tool_names = {t.name for t in tools.list_tools()}
        assert "search_materials" in tool_names
        assert "plot_materials_comparison" in tool_names


