"""Tests for agent_capabilities tool.

Wraps the MARC27 platform's GET /agent/capabilities self-discovery
endpoint. Tests verify registration shape + clean failure when no
auth is configured + that the dispatcher hits the right URL when
credentials are present (network mocked at the `requests` level).

The platform route itself is tested in marc27-core's own suite.
"""
from unittest.mock import patch

import pytest

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
        assert "error" in result
        assert "Not authenticated" in result["error"]
        assert "prism login" in result.get("hint", "")

    def test_hits_correct_endpoint_url(self, monkeypatch):
        """With credentials present, the tool should GET exactly
        `<api_url>/agent/capabilities`. Mock `requests.get` so we don't
        depend on platform network reachability, and inspect the URL."""
        called_urls = []

        class _StubResp:
            status_code = 200

            def json(self):
                return {"platform": "MARC27", "total_endpoints": 0}

        def _stub_get(url, **_kwargs):
            called_urls.append(url)
            return _StubResp()

        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        monkeypatch.setattr(
            "app.tools.agent_capabilities.requests.get", _stub_get
        )

        result = _agent_capabilities()
        assert result == {"platform": "MARC27", "total_endpoints": 0}
        assert called_urls == [
            "https://example.invalid/api/v1/agent/capabilities"
        ]
