"""Tests for agent_capabilities tool.

Wraps the MARC27 platform's GET /agent/capabilities self-discovery
endpoint. Tests verify registration shape + clean failure when no
auth is configured + that the dispatcher hits the right URL when
credentials are present (the socket under `PlatformClient` is stubbed
by the shared `platform_http` fixture).

The platform route itself is tested in marc27-core's own suite.
"""
import pytest

from tests.conftest import assert_not_connected

from app.tools.base import ToolRegistry
from app.tools.agent_capabilities import (
    _agent_capabilities,
    create_agent_capabilities_tool,
)


@pytest.fixture(autouse=True)
def _no_credentials_env(monkeypatch, tmp_path):
    """Run each test with empty creds env + a non-existent creds file
    so the tool takes the "not authenticated" branch deterministically
    unless a test opts back in."""
    monkeypatch.delenv("MARC27_API_KEY", raising=False)
    monkeypatch.delenv("MARC27_API_URL", raising=False)
    # Point HOME at an empty tmpdir so credentials.json is missing.
    monkeypatch.setenv("HOME", str(tmp_path))


class TestRegistration:
    def test_registers_exactly_one_tool_named_agent_capabilities(self):
        registry = ToolRegistry()
        create_agent_capabilities_tool(registry)
        tools = registry.list_tools()
        assert len(tools) == 1
        assert tools[0].name == "agent_capabilities"

    def test_no_approval_required(self):
        """Read-only — must not be approval-gated."""
        registry = ToolRegistry()
        create_agent_capabilities_tool(registry)
        assert registry.get("agent_capabilities").requires_approval is False


class TestAgentCapabilities:
    def test_no_credentials_returns_login_hint(self):
        result = _agent_capabilities()
        assert_not_connected(result)

    def test_hits_correct_endpoint_url(self, monkeypatch, platform_http):
        """With credentials present, the tool should GET exactly
        `<api_url>/agent/capabilities`, authenticated with `X-API-Key`."""
        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        platform_http.payload = {"platform": "MARC27", "total_endpoints": 0}

        result = _agent_capabilities()
        assert result == {"platform": "MARC27", "total_endpoints": 0}
        assert platform_http.urls_for("GET") == [
            "https://example.invalid/api/v1/agent/capabilities"
        ]
        assert platform_http.calls[0]["headers"]["X-API-Key"] == "fake-token"
