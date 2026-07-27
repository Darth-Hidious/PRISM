"""Tests for platform_status tools (policy_evaluate / usage_status / billing_balance).

These tools wrap MARC27 platform read endpoints. The tests verify
registration shape + that each dispatcher fails cleanly when no
auth is configured (the most likely runtime error in development).

Network-dependent behavior is stubbed at the socket under
`PlatformClient` (the shared `platform_http` fixture); the underlying
platform routes are tested in marc27-core's own suite.
"""
from unittest.mock import patch

import pytest

from tests.conftest import assert_not_connected

from app.tools.base import ToolRegistry
from app.tools.platform_status import (
    _billing_balance,
    _policy_evaluate,
    _usage_status,
    create_platform_status_tools,
)


@pytest.fixture(autouse=True)
def _no_credentials_env(monkeypatch, tmp_path):
    """Run each test with empty creds env + a non-existent creds file
    so the tools take the "not authenticated" branch deterministically."""
    monkeypatch.delenv("MARC27_API_KEY", raising=False)
    monkeypatch.delenv("MARC27_API_URL", raising=False)
    # Point HOME at an empty tmpdir so credentials.json is missing.
    monkeypatch.setenv("HOME", str(tmp_path))


class TestRegistration:
    def test_registers_three_tools(self):
        registry = ToolRegistry()
        create_platform_status_tools(registry)
        tools = registry.list_tools()
        names = {t.name for t in tools}
        assert names == {"policy_evaluate", "usage_status", "billing_balance"}

    def test_no_approval_required(self):
        """All three are read-only and should not be approval-gated."""
        registry = ToolRegistry()
        create_platform_status_tools(registry)
        for name in ("policy_evaluate", "usage_status", "billing_balance"):
            assert registry.get(name).requires_approval is False, (
                f"{name} is read-only and must not be approval-gated"
            )


class TestPolicyEvaluate:
    def test_missing_action_returns_clear_error(self):
        result = _policy_evaluate()
        assert "error" in result
        assert "action" in result["error"].lower()

    def test_no_credentials_returns_login_hint(self):
        result = _policy_evaluate(action="compute.submit")
        assert_not_connected(result)


class TestUsageStatus:
    def test_no_credentials_returns_login_hint(self):
        result = _usage_status()
        assert_not_connected(result)

    def test_project_id_no_credentials(self):
        # Same path; just exercises the project_id branch.
        result = _usage_status(project_id="00000000-0000-4000-8000-000000000001")
        assert_not_connected(result)


class TestBillingBalance:
    def test_default_action_balance(self):
        # No credentials → still hits the error path (auth check before HTTP).
        result = _billing_balance()
        assert_not_connected(result)

    def test_unknown_action_clear_error(self):
        # Provide credentials so we get past the auth check, hit the
        # action validation branch.
        with patch.dict("os.environ", {"MARC27_API_KEY": "fake-token"}):
            result = _billing_balance(action="topup")
            assert "error" in result
            assert "Unknown action" in result["error"]
            assert "balance" in result["error"]

    def test_valid_actions_call_correct_endpoint(self, monkeypatch, platform_http):
        """The three valid actions should hit distinct endpoints."""
        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")

        for action in ("balance", "usage", "prices"):
            result = _billing_balance(action=action)
            assert result == {"ok": True}, f"action={action} returned {result}"

        assert platform_http.urls_for("GET") == [
            "https://example.invalid/api/v1/billing/balance",
            "https://example.invalid/api/v1/billing/usage",
            "https://example.invalid/api/v1/billing/prices",
        ]
