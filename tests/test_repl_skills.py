"""Tests for /skill and /plan REPL commands."""

from io import StringIO
from unittest.mock import MagicMock, patch

import pytest

from app.agent.repl import AgentREPL, REPL_COMMANDS


def _make_repl(mock_backend=None, **kwargs):
    """Create an AgentREPL with mocked prompt_toolkit session."""
    if mock_backend is None:
        mock_backend = MagicMock()
    with patch("app.agent.repl.PromptSession"):
        with patch("app.agent.repl.AgentCore"):
            repl = AgentREPL(backend=mock_backend, enable_mcp=False, **kwargs)
    return repl


class TestReplSkillCommands:
    def test_commands_registered(self):
        assert "/skill" in REPL_COMMANDS
        assert "/plan" in REPL_COMMANDS

    def test_handle_skill_list(self):
        repl = _make_repl()

        output = StringIO()
        repl.console.file = output

        repl._handle_skill(None)
        text = output.getvalue()
        assert "acquire_materials" in text
        assert "materials_discovery" in text

    def test_handle_skill_detail(self):
        repl = _make_repl()

        output = StringIO()
        repl.console.file = output

        repl._handle_skill("materials_discovery")
        text = output.getvalue()
        assert "materials_discovery" in text
        # Check for numbered step listing (new format)
        assert "acquire" in text
        assert "predict" in text

    def test_handle_skill_not_found(self):
        repl = _make_repl()

        output = StringIO()
        repl.console.file = output

        repl._handle_skill("nonexistent_skill")
        text = output.getvalue()
        assert "not found" in text

    def test_handle_command_skill(self):
        repl = _make_repl()
        result = repl._handle_command("/skill")
        assert result is False

    def test_handle_command_plan_no_arg(self):
        repl = _make_repl()

        output = StringIO()
        repl.console.file = output

        result = repl._handle_command("/plan")
        assert result is False
        text = output.getvalue()
        assert "Usage" in text
