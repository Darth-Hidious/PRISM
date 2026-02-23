"""Tests for mcp.py filter parsing and ModelContext."""
import sys
import pytest
from unittest.mock import MagicMock, patch

# Mock optimade before importing app.mcp so the import succeeds
# even when optimade is not installed.
sys.modules.setdefault("optimade", MagicMock())
sys.modules.setdefault("optimade.client", MagicMock())

from app.mcp import ModelContext
from app.config.settings import MAX_RESULTS_DISPLAY


class TestModelContext:
    def test_to_prompt_basic(self):
        ctx = ModelContext(
            query="Find silicon materials",
            results=[{
                "id": "mp-1",
                "attributes": {"chemical_formula_descriptive": "Si", "elements": ["Si"]},
                "meta": {"provider": {"name": "mp"}},
            }],
        )
        prompt = ctx.to_prompt()
        assert "silicon" in prompt.lower()
        assert "Si" in prompt

    def test_to_prompt_limits_results(self):
        results = [
            {
                "id": f"mp-{i}",
                "attributes": {"chemical_formula_descriptive": f"X{i}", "elements": [f"X{i}"]},
                "meta": {"provider": {"name": "test"}},
            }
            for i in range(50)
        ]
        ctx = ModelContext(query="test", results=results)
        prompt = ctx.to_prompt()
        # Results beyond MAX_RESULTS_DISPLAY should not appear
        assert f"X{MAX_RESULTS_DISPLAY + 5}" not in prompt

    def test_to_prompt_reasoning_mode(self):
        ctx = ModelContext(query="test", results=[])
        prompt = ctx.to_prompt(reasoning_mode=True)
        assert isinstance(prompt, str)
