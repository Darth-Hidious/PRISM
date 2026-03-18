"""Tests for plan-then-execute mode (Phase F-4)."""
import pytest
from app.agent.prompts import INTERACTIVE_SYSTEM_PROMPT, AUTONOMOUS_SYSTEM_PROMPT


class TestPlanPrompts:
    def test_interactive_prompt_has_plan_instruction(self):
        assert "<plan>" in INTERACTIVE_SYSTEM_PROMPT
        assert "Plan" in INTERACTIVE_SYSTEM_PROMPT

    def test_autonomous_prompt_has_plan_instruction(self):
        assert "<plan>" in AUTONOMOUS_SYSTEM_PROMPT
        assert "Plan" in AUTONOMOUS_SYSTEM_PROMPT

    def test_interactive_prompt_mentions_review(self):
        assert "review" in INTERACTIVE_SYSTEM_PROMPT.lower() or "approval" in INTERACTIVE_SYSTEM_PROMPT.lower()

    def test_interactive_prompt_enforces_selection_screen(self):
        """Vague queries should get ONE question with options, not a questionnaire."""
        assert "ONE question" in INTERACTIVE_SYSTEM_PROMPT
        assert "questionnaire" in INTERACTIVE_SYSTEM_PROMPT.lower()

    def test_autonomous_prompt_states_assumptions(self):
        """Autonomous mode should state assumptions instead of asking."""
        assert "assumptions" in AUTONOMOUS_SYSTEM_PROMPT.lower()
