"""Tests for the braille spinner."""

from io import StringIO
from rich.console import Console
from app.agent.spinner import Spinner, TOOL_VERBS, BRAILLE_FRAMES


class TestSpinner:
    def _make_spinner(self):
        return Spinner(console=Console(file=StringIO()))

    def test_initial_state(self):
        s = self._make_spinner()
        assert s._status is None

    def test_tool_verbs_mapping(self):
        assert "search_materials" in TOOL_VERBS
        assert "predict_property" in TOOL_VERBS
        assert "calculate_phase_diagram" in TOOL_VERBS

    def test_verb_for_tool(self):
        s = self._make_spinner()
        assert s.verb_for_tool("search_materials") == TOOL_VERBS["search_materials"]
        assert s.verb_for_tool("unknown_tool") == "Thinking\u2026"

    def test_start_and_stop(self):
        s = self._make_spinner()
        s.start("Testing...")
        assert s._status is not None
        s.stop()
        assert s._status is None

    def test_update(self):
        s = self._make_spinner()
        s.start("First...")
        s.update("Second...")
        s.stop()

    def test_stop_without_start(self):
        s = self._make_spinner()
        s.stop()  # should not raise

    def test_braille_frames(self):
        assert len(BRAILLE_FRAMES) == 10
        assert BRAILLE_FRAMES[0] == "\u280b"
