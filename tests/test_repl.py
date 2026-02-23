"""Tests for the interactive REPL."""
import pytest
from unittest.mock import patch, MagicMock
from app.agent.repl import AgentREPL


class TestAgentREPL:
    def test_init(self):
        backend = MagicMock()
        repl = AgentREPL(backend=backend)
        assert repl.agent is not None

    @patch("builtins.input", side_effect=["hello", "/exit"])
    @patch("app.agent.repl.Console")
    def test_exit_command(self, mock_console_cls, mock_input):
        backend = MagicMock()
        from app.agent.events import AgentResponse
        backend.complete.return_value = AgentResponse(text="Hi there!")
        repl = AgentREPL(backend=backend)
        repl.run()

    @patch("builtins.input", side_effect=["/help", "/exit"])
    @patch("app.agent.repl.Console")
    def test_help_command(self, mock_console_cls, mock_input):
        backend = MagicMock()
        repl = AgentREPL(backend=backend)
        repl.run()

    @patch("builtins.input", side_effect=["/clear", "/exit"])
    @patch("app.agent.repl.Console")
    def test_clear_command(self, mock_console_cls, mock_input):
        backend = MagicMock()
        repl = AgentREPL(backend=backend)
        repl.run()
