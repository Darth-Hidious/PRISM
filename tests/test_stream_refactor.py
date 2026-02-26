"""Tests for stream.py refactor — verify it consumes UIEmitter events.

These tests confirm that handle_streaming_response creates a UIEmitter,
iterates its process() generator, and dispatches events to the correct
Rich renderers.
"""
import pytest
from unittest.mock import MagicMock, patch, call

from app.backend.protocol import make_event


def _make_mock_emitter(events, final_session_cost=0.0):
    """Create a mock UIEmitter that yields the given protocol events.

    After process() is fully iterated, session_cost reflects the final value.
    """
    emitter = MagicMock()
    emitter.session_cost = 0.0

    def _process_side_effect(user_input):
        yield from events
        emitter.session_cost = final_session_cost

    emitter.process.side_effect = _process_side_effect
    return emitter


class TestStreamUsesUIEmitter:
    """Verify that handle_streaming_response delegates to UIEmitter."""

    @patch("app.cli.tui.stream.UIEmitter")
    @patch("app.cli.tui.stream.render_input_card")
    def test_stream_uses_ui_emitter(self, mock_render_input, MockUIEmitter):
        """UIEmitter is instantiated with the agent and process() is called."""
        from app.cli.tui.stream import handle_streaming_response

        mock_emitter = _make_mock_emitter([
            make_event("ui.turn.complete", {}),
        ], final_session_cost=0.005)
        MockUIEmitter.return_value = mock_emitter

        console = MagicMock()
        # Prevent Rich Live from actually rendering
        console.is_terminal = False
        agent = MagicMock()
        session = MagicMock()

        result = handle_streaming_response(
            console, agent, "hello", session, session_cost=0.001,
        )

        # UIEmitter was created with the agent
        MockUIEmitter.assert_called_once_with(agent)
        # process() was called with user_input
        mock_emitter.process.assert_called_once_with("hello")
        # Returns the emitter's final session_cost (updated after processing)
        assert result == 0.005


class TestStreamRendersToolCard:
    """Verify that ui.card events dispatch to render_tool_result."""

    @patch("app.cli.tui.stream.UIEmitter")
    @patch("app.cli.tui.stream.render_input_card")
    @patch("app.cli.tui.stream.render_tool_result")
    def test_stream_renders_tool_card(
        self, mock_render_tool, mock_render_input, MockUIEmitter,
    ):
        """A ui.card event with card_type != 'plan' calls render_tool_result."""
        from app.cli.tui.stream import handle_streaming_response

        tool_card = make_event("ui.card", {
            "card_type": "tool",
            "tool_name": "search_materials",
            "elapsed_ms": 123.4,
            "content": "Found 5 results",
            "data": {"count": 5},
        })
        mock_emitter = _make_mock_emitter([
            tool_card,
            make_event("ui.turn.complete", {}),
        ], final_session_cost=0.0)
        MockUIEmitter.return_value = mock_emitter

        console = MagicMock()
        console.is_terminal = False
        agent = MagicMock()
        session = MagicMock()

        handle_streaming_response(console, agent, "search", session)

        mock_render_tool.assert_called_once_with(
            console,
            "search_materials",
            "Found 5 results",
            123.4,
            {"count": 5},
        )
