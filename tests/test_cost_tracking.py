"""Tests for token/cost tracking in AgentCore."""
from unittest.mock import MagicMock
from app.agent.core import AgentCore
from app.agent.events import AgentResponse, UsageInfo, TurnComplete
from app.agent.models import ModelConfig
from app.tools.base import ToolRegistry


def _make_backend_with_usage(text, usage):
    backend = MagicMock()
    response = AgentResponse(text=text, usage=usage)
    backend.complete.return_value = response
    backend.model_config = ModelConfig(
        id="test-model", provider="test", context_window=128_000,
        max_output_tokens=16_384, default_max_tokens=8_192,
        input_price_per_mtok=3.0, output_price_per_mtok=15.0,
    )
    return backend


class TestCostTracking:
    def test_calculate_cost_basic(self):
        backend = _make_backend_with_usage("hi", UsageInfo(input_tokens=1000, output_tokens=500))
        agent = AgentCore(backend=backend, tools=ToolRegistry())
        cost = agent._calculate_cost(UsageInfo(input_tokens=1000, output_tokens=500))
        assert abs(cost - 0.0105) < 0.0001

    def test_calculate_cost_with_cache(self):
        backend = _make_backend_with_usage("hi", UsageInfo(input_tokens=1000, output_tokens=500, cache_read_tokens=800))
        agent = AgentCore(backend=backend, tools=ToolRegistry())
        cost = agent._calculate_cost(UsageInfo(input_tokens=1000, output_tokens=500, cache_read_tokens=800))
        assert abs(cost - 0.01074) < 0.0001

    def test_calculate_cost_no_model_config(self):
        backend = MagicMock(spec=[])  # no model_config attribute
        agent = AgentCore(backend=backend, tools=ToolRegistry())
        cost = agent._calculate_cost(UsageInfo(input_tokens=1000, output_tokens=500))
        assert cost == 0.0

    def test_total_usage_accumulates(self):
        backend = MagicMock()
        backend.model_config = ModelConfig(
            id="test", provider="test", context_window=128_000,
            max_output_tokens=16_384, default_max_tokens=8_192,
            input_price_per_mtok=3.0, output_price_per_mtok=15.0,
        )
        backend.complete.side_effect = [
            AgentResponse(text="first", usage=UsageInfo(input_tokens=100, output_tokens=50)),
            AgentResponse(text="second", usage=UsageInfo(input_tokens=200, output_tokens=100)),
        ]
        agent = AgentCore(backend=backend, tools=ToolRegistry())
        agent.process("q1")
        agent.process("q2")
        assert agent._total_usage.input_tokens == 300
        assert agent._total_usage.output_tokens == 150

    def test_stream_turn_complete_has_cost(self):
        backend = MagicMock()
        backend.model_config = ModelConfig(
            id="test", provider="test", context_window=128_000,
            max_output_tokens=16_384, default_max_tokens=8_192,
            input_price_per_mtok=3.0, output_price_per_mtok=15.0,
        )
        response = AgentResponse(text="hello", usage=UsageInfo(input_tokens=100, output_tokens=50))

        def fake_stream(messages, tools, system_prompt=None):
            backend._last_stream_response = response
            yield TurnComplete(text="hello", has_more=False)

        backend.complete_stream = fake_stream
        agent = AgentCore(backend=backend, tools=ToolRegistry())
        events = list(agent.process_stream("hi"))
        tc = [e for e in events if isinstance(e, TurnComplete)]
        assert len(tc) == 1
        assert tc[0].estimated_cost is not None
        assert tc[0].estimated_cost > 0

    def test_reset_clears_usage(self):
        backend = _make_backend_with_usage("hi", UsageInfo(input_tokens=100, output_tokens=50))
        agent = AgentCore(backend=backend, tools=ToolRegistry())
        agent.process("q1")
        assert agent._total_usage.total_tokens > 0
        agent.reset()
        assert agent._total_usage.total_tokens == 0
