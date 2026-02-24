"""Tests for the braille spinner."""

from unittest.mock import MagicMock
from app.agent.spinner import Spinner, TOOL_VERBS, BRAILLE_FRAMES


class TestSpinner:
    def test_default_verb(self):
        s = Spinner(console=MagicMock())
        assert s._verb == "Thinking..."

    def test_tool_verbs_mapping(self):
        assert "search_optimade" in TOOL_VERBS
        assert "predict_property" in TOOL_VERBS
        assert "calculate_phase_diagram" in TOOL_VERBS

    def test_verb_for_tool(self):
        s = Spinner(console=MagicMock())
        assert s.verb_for_tool("search_optimade") == TOOL_VERBS["search_optimade"]
        assert s.verb_for_tool("unknown_tool") == "Thinking..."

    def test_start_and_stop(self):
        console = MagicMock()
        s = Spinner(console=console)
        s.start("Testing...")
        assert s._running is True
        assert s._verb == "Testing..."
        s.stop()
        assert s._running is False

    def test_update_verb(self):
        console = MagicMock()
        s = Spinner(console=console)
        s.start("First...")
        s.update("Second...")
        assert s._verb == "Second..."
        s.stop()

    def test_stop_without_start(self):
        s = Spinner(console=MagicMock())
        s.stop()  # should not raise

    def test_braille_frames(self):
        assert len(BRAILLE_FRAMES) == 10
        assert BRAILLE_FRAMES[0] == "⠋"
