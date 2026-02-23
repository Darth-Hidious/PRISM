"""Tests for the interactive REPL."""
import tempfile
import pytest
from unittest.mock import patch, MagicMock
from app.agent.repl import AgentREPL
from app.agent.events import AgentResponse, TextDelta, TurnComplete


class TestAgentREPL:
    def test_init(self):
        backend = MagicMock()
        repl = AgentREPL(backend=backend)
        assert repl.agent is not None

    @patch("builtins.input", side_effect=["hello", "/exit", "n"])
    @patch("app.agent.repl.Console")
    def test_exit_command(self, mock_console_cls, mock_input):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="Hi there!")
        backend._last_stream_response = AgentResponse(text="Hi there!")
        backend.complete_stream = MagicMock(return_value=iter([TextDelta(text="Hi there!"), TurnComplete(text="Hi there!")]))
        repl = AgentREPL(backend=backend)
        repl.run()

    @patch("builtins.input", side_effect=["/help", "/exit", "n"])
    @patch("app.agent.repl.Console")
    def test_help_command(self, mock_console_cls, mock_input):
        backend = MagicMock()
        repl = AgentREPL(backend=backend)
        repl.run()

    @patch("builtins.input", side_effect=["/clear", "/exit", "n"])
    @patch("app.agent.repl.Console")
    def test_clear_command(self, mock_console_cls, mock_input):
        backend = MagicMock()
        repl = AgentREPL(backend=backend)
        repl.run()

    @patch("builtins.input", side_effect=["/sessions", "/exit", "n"])
    @patch("app.agent.repl.Console")
    def test_sessions_command(self, mock_console_cls, mock_input):
        backend = MagicMock()
        repl = AgentREPL(backend=backend)
        repl.run()

    @patch("builtins.input", side_effect=["/export", "/exit", "n"])
    @patch("app.agent.repl.Console")
    def test_export_no_results(self, mock_console_cls, mock_input):
        backend = MagicMock()
        repl = AgentREPL(backend=backend)
        repl.run()
        # Should not raise, just print a warning

    @patch("app.agent.repl.Console")
    def test_handle_export_with_results(self, mock_console_cls):
        backend = MagicMock()
        repl = AgentREPL(backend=backend)
        # Inject a tool result with results array
        repl.agent.history = [
            {"role": "user", "content": "find silicon"},
            {"role": "tool_result", "tool_call_id": "c1", "result": {
                "results": [{"id": "mp-1", "formula": "Si"}],
                "count": 1,
            }},
        ]
        with tempfile.NamedTemporaryFile(suffix=".csv", delete=False) as f:
            repl._handle_export(f.name)

    @patch("app.agent.repl.Console")
    def test_load_session(self, mock_console_cls):
        with tempfile.TemporaryDirectory() as tmpdir:
            backend = MagicMock()
            repl = AgentREPL(backend=backend)
            repl.memory = MagicMock()
            repl.memory.get_history.return_value = [{"role": "user", "content": "hi"}]
            repl._load_session("fake-id")
            repl.memory.load.assert_called_once_with("fake-id")

    @patch("app.agent.repl.Console")
    def test_handle_command_load_no_arg(self, mock_console_cls):
        backend = MagicMock()
        repl = AgentREPL(backend=backend)
        result = repl._handle_command("/load")
        assert result is False  # should not exit

    @patch("builtins.input", side_effect=["/exit", "y"])
    @patch("app.agent.repl.Console")
    def test_save_on_exit_prompt(self, mock_console_cls, mock_input):
        backend = MagicMock()
        repl = AgentREPL(backend=backend)
        repl.agent.history = [{"role": "user", "content": "test"}]
        repl.memory = MagicMock()
        repl.memory.save.return_value = "sess_123"
        repl.run()
        repl.memory.set_history.assert_called_once()
        repl.memory.save.assert_called_once()
