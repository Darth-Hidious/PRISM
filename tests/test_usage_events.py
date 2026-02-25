"""Tests for UsageInfo and usage tracking events."""
from app.agent.events import UsageInfo, AgentResponse, TurnComplete


class TestUsageInfo:
    def test_defaults_to_zero(self):
        u = UsageInfo()
        assert u.input_tokens == 0
        assert u.output_tokens == 0

    def test_add_two_usages(self):
        a = UsageInfo(input_tokens=100, output_tokens=50, cache_read_tokens=10)
        b = UsageInfo(input_tokens=200, output_tokens=100, cache_creation_tokens=5)
        c = a + b
        assert c.input_tokens == 300
        assert c.output_tokens == 150
        assert c.cache_read_tokens == 10
        assert c.cache_creation_tokens == 5

    def test_total_tokens(self):
        u = UsageInfo(input_tokens=100, output_tokens=50)
        assert u.total_tokens == 150


class TestAgentResponseUsage:
    def test_default_no_usage(self):
        r = AgentResponse(text="hi")
        assert r.usage is None

    def test_usage_attached(self):
        u = UsageInfo(input_tokens=100, output_tokens=50)
        r = AgentResponse(text="hi", usage=u)
        assert r.usage.input_tokens == 100


class TestTurnCompleteUsage:
    def test_default_no_usage(self):
        tc = TurnComplete(text="done")
        assert tc.usage is None
        assert tc.total_usage is None
        assert tc.estimated_cost is None

    def test_usage_fields(self):
        u = UsageInfo(input_tokens=1000, output_tokens=500)
        tc = TurnComplete(text="done", usage=u, total_usage=u, estimated_cost=0.05)
        assert tc.usage.input_tokens == 1000
        assert tc.estimated_cost == 0.05
