"""Integration tests for the run command, model config, and streaming display."""
import json
from unittest.mock import MagicMock, patch, call

import pytest
from click.testing import CliRunner

from app.agent.backends.base import Backend
from app.agent.core import AgentCore
from app.agent.events import (
    AgentResponse,
    TextDelta,
    ToolCallEvent,
    ToolCallResult,
    ToolCallStart,
    TurnComplete,
    UsageInfo,
)
from app.agent.models import ModelConfig, get_model_config
from app.tools.base import Tool, ToolRegistry


# ---------------------------------------------------------------------------
# 1. Backend creation uses model config (not hardcoded 4096)
# ---------------------------------------------------------------------------


class TestModelConfigIntegration:
    """Verify that backends use ModelConfig.default_max_tokens."""

    def test_anthropic_backend_uses_model_config_max_tokens(self):
        """AnthropicBackend should pass model_config.default_max_tokens, not 4096."""
        with patch("app.agent.backends.anthropic_backend.Anthropic") as mock_cls:
            mock_client = MagicMock()
            mock_cls.return_value = mock_client

            from app.agent.backends.anthropic_backend import AnthropicBackend

            backend = AnthropicBackend(model="claude-sonnet-4-20250514", api_key="fake")
            config = get_model_config("claude-sonnet-4-20250514")

            assert backend.model_config.default_max_tokens == config.default_max_tokens
            assert backend.model_config.default_max_tokens == 16_384
            assert backend.model_config.default_max_tokens != 4096

    def test_openai_backend_uses_model_config_max_tokens(self):
        """OpenAIBackend should pass model_config.default_max_tokens, not 4096."""
        with patch("app.agent.backends.openai_backend.OpenAI") as mock_cls:
            mock_client = MagicMock()
            mock_cls.return_value = mock_client

            from app.agent.backends.openai_backend import OpenAIBackend

            backend = OpenAIBackend(model="gpt-4o", api_key="fake")
            config = get_model_config("gpt-4o")

            assert backend.model_config.default_max_tokens == config.default_max_tokens
            assert backend.model_config.default_max_tokens == 8_192
            assert backend.model_config.default_max_tokens != 4096

    def test_backend_passes_max_tokens_to_api_call(self):
        """The actual API call should use model_config.default_max_tokens."""
        with patch("app.agent.backends.anthropic_backend.Anthropic") as mock_cls:
            mock_client = MagicMock()
            mock_cls.return_value = mock_client

            # Set up a mock response
            mock_response = MagicMock()
            mock_response.content = [MagicMock(type="text", text="hello")]
            mock_response.usage = MagicMock(
                input_tokens=10, output_tokens=5,
                cache_creation_input_tokens=0, cache_read_input_tokens=0,
            )
            mock_client.messages.create.return_value = mock_response

            from app.agent.backends.anthropic_backend import AnthropicBackend

            backend = AnthropicBackend(model="claude-sonnet-4-20250514", api_key="fake")
            backend.complete(messages=[{"role": "user", "content": "hi"}], tools=[])

            create_call = mock_client.messages.create.call_args
            assert create_call.kwargs["max_tokens"] == 16_384

    def test_unknown_model_gets_conservative_defaults(self):
        """Unknown models should get fallback config, not crash."""
        config = get_model_config("some-unknown-model-xyz")
        assert config.default_max_tokens == 8_192
        assert config.provider == "unknown"


# ---------------------------------------------------------------------------
# 2. Streaming display: flush reasoning before tool calls
# ---------------------------------------------------------------------------


class TestStreamingFlushBehavior:
    """Verify that accumulated reasoning text is flushed before tool call panels."""

    def _make_registry(self):
        registry = ToolRegistry()
        registry.register(Tool(
            name="search_db", description="Search database",
            input_schema={}, func=lambda **kw: {"count": 3},
        ))
        return registry

    def _make_backend_stream(self, events_sequence):
        """Create a mock backend whose complete_stream yields given events."""
        backend = MagicMock(spec=Backend)
        backend.model_config = get_model_config("claude-sonnet-4-20250514")

        call_count = [0]

        def fake_stream(messages, tools, system_prompt=None):
            idx = call_count[0]
            call_count[0] += 1
            events, response = events_sequence[idx]
            backend._last_stream_response = response
            yield from events

        backend.complete_stream = fake_stream
        return backend

    def test_text_deltas_appear_before_tool_call_start(self):
        """AgentCore.process_stream should yield TextDeltas before ToolCallStart."""
        registry = self._make_registry()

        # Turn 1: LLM reasons then calls a tool
        resp1 = AgentResponse(
            text="Let me search for that.",
            tool_calls=[ToolCallEvent(tool_name="search_db", tool_args={}, call_id="c1")],
            usage=UsageInfo(input_tokens=100, output_tokens=50),
        )
        events1 = [
            TextDelta(text="Let me "),
            TextDelta(text="search for that."),
            ToolCallStart(tool_name="search_db", call_id="c1"),
            TurnComplete(text="Let me search for that.", has_more=True),
        ]

        # Turn 2: LLM gives final answer
        resp2 = AgentResponse(
            text="Found 3 results.",
            usage=UsageInfo(input_tokens=200, output_tokens=30),
        )
        events2 = [
            TextDelta(text="Found 3 results."),
            TurnComplete(text="Found 3 results.", has_more=False),
        ]

        backend = self._make_backend_stream([(events1, resp1), (events2, resp2)])
        agent = AgentCore(backend=backend, tools=registry)
        all_events = list(agent.process_stream("find materials"))

        # Verify ordering: TextDeltas come before ToolCallStart
        text_indices = [i for i, e in enumerate(all_events) if isinstance(e, TextDelta)]
        tool_start_indices = [i for i, e in enumerate(all_events) if isinstance(e, ToolCallStart)]

        assert len(text_indices) >= 2
        assert len(tool_start_indices) == 1
        # All text deltas from turn 1 should appear before the tool call start
        assert all(ti < tool_start_indices[0] for ti in text_indices[:2])

    def test_run_command_flushes_text_before_tool_panel(self):
        """The run_goal CLI command should flush accumulated text before printing tool panel.

        This tests the display logic: _flush_text is called on ToolCallStart,
        ensuring reasoning text is not wiped by the Live update.
        """
        registry = self._make_registry()

        stream_events = [
            TextDelta(text="Analyzing your request..."),
            ToolCallStart(tool_name="search_db", call_id="c1"),
            ToolCallResult(call_id="c1", tool_name="search_db", result={"count": 3}, summary="search_db: 3 results"),
            TurnComplete(
                text="Done.",
                total_usage=UsageInfo(input_tokens=100, output_tokens=50),
                estimated_cost=0.001,
            ),
        ]

        with patch("app.agent.factory.create_backend") as mock_backend, \
             patch("app.agent.autonomous.run_autonomous_stream", return_value=iter(stream_events)), \
             patch("app.plugins.bootstrap.build_full_registry", return_value=(registry, MagicMock(), MagicMock())):

            from app.cli import cli

            runner = CliRunner()
            result = runner.invoke(cli, ["run", "find silicon"])

            assert result.exit_code == 0
            # The accumulated text should have been printed (flushed), not lost
            assert "Analyzing your request" in result.output
            # Tool panel should appear
            assert "search_db" in result.output


# ---------------------------------------------------------------------------
# 3. Token/cost info is displayed at the end
# ---------------------------------------------------------------------------


class TestTokenCostDisplay:
    """Verify that token usage and cost info are displayed after streaming."""

    def test_turn_complete_with_cost_displays_info(self):
        """When TurnComplete has estimated_cost, it should be printed."""
        stream_events = [
            TextDelta(text="Silicon has band gap 1.1 eV."),
            TurnComplete(
                text="Silicon has band gap 1.1 eV.",
                total_usage=UsageInfo(input_tokens=1500, output_tokens=200),
                estimated_cost=0.0075,
            ),
        ]

        registry = ToolRegistry()
        with patch("app.agent.factory.create_backend") as mock_backend, \
             patch("app.agent.autonomous.run_autonomous_stream", return_value=iter(stream_events)), \
             patch("app.plugins.bootstrap.build_full_registry", return_value=(registry, MagicMock(), MagicMock())):

            from app.cli import cli

            runner = CliRunner()
            result = runner.invoke(cli, ["run", "silicon band gap"])

            assert result.exit_code == 0
            assert "tokens:" in result.output or "1,500" in result.output
            assert "cost:" in result.output or "$0.0075" in result.output

    def test_turn_complete_without_cost_no_crash(self):
        """When TurnComplete has no cost info, the command should still succeed."""
        stream_events = [
            TextDelta(text="Answer."),
            TurnComplete(text="Answer.", estimated_cost=None),
        ]

        registry = ToolRegistry()
        with patch("app.agent.factory.create_backend") as mock_backend, \
             patch("app.agent.autonomous.run_autonomous_stream", return_value=iter(stream_events)), \
             patch("app.plugins.bootstrap.build_full_registry", return_value=(registry, MagicMock(), MagicMock())):

            from app.cli import cli

            runner = CliRunner()
            result = runner.invoke(cli, ["run", "quick question"])

            assert result.exit_code == 0
            # No cost line should appear
            assert "cost:" not in result.output


# ---------------------------------------------------------------------------
# 4. AgentCore cost calculation uses backend model config
# ---------------------------------------------------------------------------


class TestAgentCoreCostCalculation:
    """Verify that AgentCore._calculate_cost uses the backend's model_config."""

    def test_calculate_cost_from_model_config(self):
        backend = MagicMock()
        backend.model_config = get_model_config("claude-sonnet-4-20250514")
        agent = AgentCore(backend=backend, tools=ToolRegistry())

        usage = UsageInfo(input_tokens=1_000_000, output_tokens=1_000_000)
        cost = agent._calculate_cost(usage)

        # claude-sonnet-4: $3/Mtok in, $15/Mtok out
        expected = 3.00 + 15.00
        assert abs(cost - expected) < 0.01

    def test_calculate_cost_with_cache_tokens(self):
        backend = MagicMock()
        backend.model_config = get_model_config("claude-sonnet-4-20250514")
        agent = AgentCore(backend=backend, tools=ToolRegistry())

        usage = UsageInfo(
            input_tokens=500_000,
            output_tokens=100_000,
            cache_read_tokens=200_000,
        )
        cost = agent._calculate_cost(usage)

        # 500k * 3/M + 100k * 15/M + 200k * 3 * 0.1/M
        expected = 1.50 + 1.50 + 0.06
        assert abs(cost - expected) < 0.01

    def test_calculate_cost_no_model_config(self):
        backend = MagicMock(spec=[])  # no model_config attribute
        agent = AgentCore(backend=backend, tools=ToolRegistry())

        usage = UsageInfo(input_tokens=1000, output_tokens=500)
        cost = agent._calculate_cost(usage)
        assert cost == 0.0
