"""Tests for the interactive REPL."""
import tempfile
from io import StringIO
import pytest
from unittest.mock import patch, MagicMock, PropertyMock
from rich.console import Console
from app.agent.repl import AgentREPL
from app.agent.events import AgentResponse, TextDelta, TurnComplete


def _make_repl(backend=None, **kwargs):
    """Create an AgentREPL with mocked prompt_toolkit session."""
    if backend is None:
        backend = MagicMock()
    with patch("app.agent.repl.PromptSession"):
        repl = AgentREPL(backend=backend, **kwargs)
    return repl


def _run_repl_with_inputs(repl, inputs):
    """Run the REPL with a sequence of prompt inputs."""
    repl._session.prompt = MagicMock(side_effect=inputs)
    repl.run()


class TestAgentREPL:
    def test_init(self):
        backend = MagicMock()
        repl = _make_repl(backend)
        assert repl.agent is not None

    @patch("builtins.input", side_effect=["n"])
    @patch("app.agent.repl.Console")
    def test_exit_command(self, mock_console_cls, mock_input):
        backend = MagicMock()
        backend._last_stream_response = AgentResponse(text="Hi there!")
        backend.complete_stream = MagicMock(return_value=iter([TextDelta(text="Hi there!"), TurnComplete(text="Hi there!")]))
        repl = _make_repl(backend)
        _run_repl_with_inputs(repl, ["hello", "/exit", EOFError()])

    @patch("app.agent.repl.Console")
    def test_help_command(self, mock_console_cls):
        repl = _make_repl()
        _run_repl_with_inputs(repl, ["/help", "/exit", EOFError()])

    @patch("app.agent.repl.Console")
    def test_clear_command(self, mock_console_cls):
        repl = _make_repl()
        _run_repl_with_inputs(repl, ["/clear", "/exit", EOFError()])

    @patch("app.agent.repl.Console")
    def test_sessions_command(self, mock_console_cls):
        repl = _make_repl()
        _run_repl_with_inputs(repl, ["/sessions", "/exit", EOFError()])

    @patch("app.agent.repl.Console")
    def test_export_no_results(self, mock_console_cls):
        repl = _make_repl()
        _run_repl_with_inputs(repl, ["/export", "/exit", EOFError()])

    @patch("app.agent.repl.Console")
    def test_handle_export_with_results(self, mock_console_cls):
        repl = _make_repl()
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
        repl = _make_repl()
        repl.memory = MagicMock()
        repl.memory.get_history.return_value = [{"role": "user", "content": "hi"}]
        repl._load_session("fake-id")
        repl.memory.load.assert_called_once_with("fake-id")

    @patch("app.agent.repl.Console")
    def test_handle_command_load_no_arg(self, mock_console_cls):
        repl = _make_repl()
        result = repl._handle_command("/load")
        assert result is False

    @patch("builtins.input", side_effect=["y"])
    @patch("app.agent.repl.Console")
    def test_save_on_exit_prompt(self, mock_console_cls, mock_input):
        repl = _make_repl()
        repl.agent.history = [{"role": "user", "content": "test"}]
        repl.memory = MagicMock()
        repl.memory.save.return_value = "sess_123"
        _run_repl_with_inputs(repl, ["/exit"])
        repl.memory.set_history.assert_called_once()
        repl.memory.save.assert_called_once()

    @patch("builtins.input", return_value="a")
    def test_approval_always_sets_auto(self, mock_input):
        repl = _make_repl()
        repl._auto_approve_tools = set()
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
        repl._show_welcome()
        text = output.getvalue()
        # Hex crystal mascot should contain hex characters
        assert "\u2b22" in text or "\u2b21" in text
