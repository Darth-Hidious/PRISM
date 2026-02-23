"""Tests for autonomous mode."""
import pytest
from unittest.mock import MagicMock
from app.agent.autonomous import run_autonomous, run_autonomous_stream
from app.agent.events import AgentResponse, TextDelta, TurnComplete


class TestAutonomousMode:
    def test_runs_to_completion(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="Silicon has a band gap of 1.1 eV.")
        result = run_autonomous(goal="What is the band gap of silicon?", backend=backend)
        assert "silicon" in result.lower() or "1.1" in result

    def test_returns_string(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="Done.")
        result = run_autonomous(goal="test", backend=backend)
        assert isinstance(result, str)


class TestAutonomousStream:
    def test_stream_yields_events(self):
        backend = MagicMock()
        backend._last_stream_response = AgentResponse(text="Result.")

        def fake_stream(messages, tools, system_prompt=None):
            backend._last_stream_response = AgentResponse(text="Result.")
            yield TextDelta(text="Result.")
            yield TurnComplete(text="Result.")

        backend.complete_stream = fake_stream
        events = list(run_autonomous_stream(goal="test goal", backend=backend))
        types = [type(e).__name__ for e in events]
        assert "TextDelta" in types
        assert "TurnComplete" in types
