"""Tests for autonomous mode."""
import pytest
from unittest.mock import MagicMock
from app.agent.autonomous import run_autonomous
from app.agent.events import AgentResponse


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
