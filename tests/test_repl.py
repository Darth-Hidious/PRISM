"""Tests for the interactive REPL."""
import tempfile
from io import StringIO
import pytest
from unittest.mock import patch, MagicMock, PropertyMock
from rich.console import Console
from app.cli.tui.app import AgentREPL
from app.agent.events import AgentResponse, TextDelta, TurnComplete


def _make_repl(backend=None, **kwargs):
    """Create an AgentREPL with mocked prompt_toolkit session."""
    if backend is None:
        backend = MagicMock()
    with patch("app.cli.tui.prompt.PromptSession"):
        repl = AgentREPL(backend=backend, **kwargs)
    return repl


def _run_repl_with_inputs(repl, inputs):
    """Run the REPL with a sequence of prompt inputs."""
    repl.session.prompt = MagicMock(side_effect=inputs)
    repl.run()


class TestAgentREPL:
    def test_init(self):
        backend = MagicMock()
        repl = _make_repl(backend)
        assert repl.agent is not None

    @patch("app.cli.tui.prompt.PromptSession")
    def test_exit_command(self, mock_ps):
        backend = MagicMock()
        backend._last_stream_response = AgentResponse(text="Hi there!")
        backend.complete_stream = MagicMock(
            return_value=iter([TextDelta(text="Hi there!"), TurnComplete(text="Hi there!")])
        )
        repl = _make_repl(backend)
        # Mock save prompt to decline
        repl.session.prompt = MagicMock(side_effect=["hello", "/exit", EOFError()])
        with patch("app.cli.tui.prompt.ask_save_on_exit", return_value=False):
            repl.run()

    @patch("app.cli.tui.app.Console")
    def test_help_command(self, mock_console_cls):
        repl = _make_repl()
        repl.session.prompt = MagicMock(side_effect=["/help", "/exit", EOFError()])
        with patch("app.cli.tui.prompt.ask_save_on_exit", return_value=False):
            repl.run()

    @patch("app.cli.tui.app.Console")
    def test_clear_command(self, mock_console_cls):
        repl = _make_repl()
        repl.session.prompt = MagicMock(side_effect=["/clear", "/exit", EOFError()])
        with patch("app.cli.tui.prompt.ask_save_on_exit", return_value=False):
            repl.run()

    @patch("app.cli.tui.app.Console")
    def test_sessions_command(self, mock_console_cls):
        repl = _make_repl()
        repl.session.prompt = MagicMock(side_effect=["/sessions", "/exit", EOFError()])
        with patch("app.cli.tui.prompt.ask_save_on_exit", return_value=False):
            repl.run()

    @patch("app.cli.tui.app.Console")
    def test_export_no_results(self, mock_console_cls):
        repl = _make_repl()
        repl.session.prompt = MagicMock(side_effect=["/export", "/exit", EOFError()])
        with patch("app.cli.tui.prompt.ask_save_on_exit", return_value=False):
            repl.run()

    @patch("app.cli.tui.app.Console")
    def test_handle_export_with_results(self, mock_console_cls):
        repl = _make_repl()
        repl.agent.history = [
            {"role": "user", "content": "find silicon"},
            {"role": "tool_result", "tool_call_id": "c1", "result": {
                "results": [{"id": "mp-1", "formula": "Si"}],
                "count": 1,
            }},
        ]
        from app.cli.slash.handlers import handle_export
        with tempfile.NamedTemporaryFile(suffix=".csv", delete=False) as f:
            handle_export(repl, f.name)

    @patch("app.cli.tui.app.Console")
    def test_load_session(self, mock_console_cls):
        repl = _make_repl()
        repl.memory = MagicMock()
        repl.memory.get_history.return_value = [{"role": "user", "content": "hi"}]
        repl.load_session("fake-id")
        repl.memory.load.assert_called_once_with("fake-id")

    @patch("app.cli.tui.app.Console")
    def test_handle_command_load_no_arg(self, mock_console_cls):
        repl = _make_repl()
        from app.cli.slash.handlers import handle_command
        result = handle_command(repl, "/load")
        assert result is False

    def test_save_on_exit_prompt(self):
        repl = _make_repl()
        repl.agent.history = [{"role": "user", "content": "test"}]
        repl.memory = MagicMock()
        repl.memory.save.return_value = "sess_123"
        # Mock the save prompt to accept (patch where it's looked up)
        with patch("app.cli.tui.app.ask_save_on_exit", return_value=True):
            repl.session.prompt = MagicMock(side_effect=["/exit"])
            repl.run()
        repl.memory.set_history.assert_called_once()
        repl.memory.save.assert_called_once()

    def test_approval_always_sets_auto(self):
        repl = _make_repl()
        repl._auto_approve_tools = set()
        # Mock prompt_toolkit to return "a"
        repl.session.prompt = MagicMock(return_value="a")
        result = repl._approval_callback("predict_property", {"target": "band_gap"})
        assert result is True
        assert "predict_property" in repl._auto_approve_tools

    def test_spinner_imported(self):
        from app.agent.spinner import Spinner
        assert Spinner is not None

    def test_welcome_banner_has_hex_crystal(self):
        repl = _make_repl()
        output = StringIO()
        repl.console = Console(file=output, highlight=False, force_terminal=True)
        from app.cli.tui.welcome import show_welcome
        show_welcome(repl.console, repl.agent, repl._auto_approve)
        text = output.getvalue()
        assert "\u2b22" in text or "\u2b21" in text


def test_repl_has_session_cost():
    repl = _make_repl()
    assert hasattr(repl, "session_cost")
    assert repl.session_cost == 0.0
