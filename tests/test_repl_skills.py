"""Tests for /skill and /plan REPL commands."""

from io import StringIO
from unittest.mock import MagicMock, patch

import pytest

from app.agent.repl import AgentREPL, REPL_COMMANDS


class TestReplSkillCommands:
    def test_commands_registered(self):
        assert "/skill" in REPL_COMMANDS
        assert "/plan" in REPL_COMMANDS

    @patch("app.agent.repl.AgentCore")
    def test_handle_skill_list(self, MockAgent):
        mock_backend = MagicMock()
        repl = AgentREPL(backend=mock_backend, enable_mcp=False)

        # Capture console output
        output = StringIO()
        repl.console.file = output

        repl._handle_skill(None)
        text = output.getvalue()
        assert "acquire_materials" in text
        assert "materials_discovery" in text

    @patch("app.agent.repl.AgentCore")
    def test_handle_skill_detail(self, MockAgent):
        mock_backend = MagicMock()
        repl = AgentREPL(backend=mock_backend, enable_mcp=False)

        output = StringIO()
        repl.console.file = output

        repl._handle_skill("materials_discovery")
        text = output.getvalue()
        assert "materials_discovery" in text
        assert "Steps" in text

    @patch("app.agent.repl.AgentCore")
    def test_handle_skill_not_found(self, MockAgent):
        mock_backend = MagicMock()
        repl = AgentREPL(backend=mock_backend, enable_mcp=False)

        output = StringIO()
        repl.console.file = output

        repl._handle_skill("nonexistent_skill")
        text = output.getvalue()
        assert "not found" in text

    @patch("app.agent.repl.AgentCore")
    def test_handle_command_skill(self, MockAgent):
        mock_backend = MagicMock()
        repl = AgentREPL(backend=mock_backend, enable_mcp=False)

        # Should not exit
        result = repl._handle_command("/skill")
        assert result is False

    @patch("app.agent.repl.AgentCore")
    def test_handle_command_plan_no_arg(self, MockAgent):
        mock_backend = MagicMock()
        repl = AgentREPL(backend=mock_backend, enable_mcp=False)

        output = StringIO()
        repl.console.file = output

        result = repl._handle_command("/plan")
        assert result is False
        text = output.getvalue()
        assert "Usage" in text
