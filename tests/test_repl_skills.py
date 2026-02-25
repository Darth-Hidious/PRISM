"""Tests for /skill and /plan REPL commands."""

from io import StringIO
from unittest.mock import MagicMock, patch

import pytest
from rich.console import Console

from app.cli.slash.registry import REPL_COMMANDS
from app.cli.slash.handlers import handle_command, handle_skill, handle_tools


def _make_repl(mock_backend=None, **kwargs):
    """Create an AgentREPL with mocked prompt_toolkit session."""
    if mock_backend is None:
        mock_backend = MagicMock()
    with patch("app.cli.tui.prompt.PromptSession"):
        with patch("app.cli.tui.app.AgentCore"):
            from app.cli.tui.app import AgentREPL
            repl = AgentREPL(backend=mock_backend, enable_mcp=False, **kwargs)
    return repl


class TestReplSkillCommands:
    def test_commands_registered(self):
        assert "/skills" in REPL_COMMANDS
        assert "/plan" in REPL_COMMANDS

    def test_handle_skill_list(self):
        repl = _make_repl()

        output = StringIO()
        repl.console.file = output

        handle_skill(repl, None)
        text = output.getvalue()
        assert "acquire_materials" in text
        assert "materials_discovery" in text

    def test_handle_skill_detail(self):
        repl = _make_repl()

        output = StringIO()
        repl.console.file = output

        handle_skill(repl, "materials_discovery")
        text = output.getvalue()
        assert "materials_discovery" in text
        # Check for numbered step listing (new format)
        assert "acquire" in text
        assert "predict" in text

    def test_handle_skill_not_found(self):
        repl = _make_repl()

        output = StringIO()
        repl.console.file = output

        handle_skill(repl, "nonexistent_skill")
        text = output.getvalue()
        assert "not found" in text

    def test_handle_command_skill(self):
        repl = _make_repl()
        result = handle_command(repl, "/skill")
        assert result is False

    def test_handle_command_plan_no_arg(self):
        repl = _make_repl()

        output = StringIO()
        repl.console.file = output

        result = handle_command(repl, "/plan")
        assert result is False
        text = output.getvalue()
        assert "Usage" in text

    def test_tools_shows_approval_star(self):
        repl = _make_repl()
        output = StringIO()
        repl.console = Console(file=output, highlight=False, force_terminal=True)
        handle_tools(repl)
        text = output.getvalue()
        assert "★" in text
