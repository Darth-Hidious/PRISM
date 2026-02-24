"""Tests for plan-then-execute mode (Phase F-4)."""
import pytest
from app.agent.core import DEFAULT_SYSTEM_PROMPT
from app.agent.autonomous import AUTONOMOUS_SYSTEM_PROMPT


class TestPlanPrompts:
    def test_default_prompt_has_plan_instruction(self):
        assert "<plan>" in DEFAULT_SYSTEM_PROMPT
        assert "PLANNING" in DEFAULT_SYSTEM_PROMPT

    def test_autonomous_prompt_has_plan_instruction(self):
        assert "<plan>" in AUTONOMOUS_SYSTEM_PROMPT
        assert "PLANNING" in AUTONOMOUS_SYSTEM_PROMPT

    def test_plan_instruction_mentions_approval(self):
        assert "review" in DEFAULT_SYSTEM_PROMPT.lower() or "approve" in DEFAULT_SYSTEM_PROMPT.lower()

    def test_plan_instruction_skip_simple(self):
        """Simple queries should skip planning."""
        assert "skip" in DEFAULT_SYSTEM_PROMPT.lower()
