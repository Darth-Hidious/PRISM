"""Tests for mcp_services / mcp_services_invoke tools.

proxy + scale are broken out as approval-gated under
`mcp_services_invoke`; list + get live in `mcp_services` with no
approval gate.

`project_id` is required for all four actions and is auto-resolved from
explicit-arg → MARC27_PROJECT_ID env var → ~/.prism/credentials.json.

Network-dependent behavior is mocked at the `requests` level; the
underlying platform routes are tested in marc27-core's own suite.
"""
from unittest.mock import patch

import pytest

from app.tools.base import ToolRegistry
from app.tools.mcp_services import (
    _mcp_services,
    _mcp_services_invoke,
    create_mcp_services_tools,
)


@pytest.fixture(autouse=True)
def _no_credentials_env(monkeypatch, tmp_path):
    """Run each test with empty creds env + a non-existent creds file
    so the tools take the "not authenticated" branch deterministically."""
    monkeypatch.delenv("MARC27_API_KEY", raising=False)
    monkeypatch.delenv("MARC27_API_URL", raising=False)
    monkeypatch.delenv("MARC27_PROJECT_ID", raising=False)
    monkeypatch.setenv("HOME", str(tmp_path))


class TestRegistration:
    def test_registers_two_tools(self):
        registry = ToolRegistry()
        create_mcp_services_tools(registry)
        names = {t.name for t in registry.list_tools()}
        assert names == {"mcp_services", "mcp_services_invoke"}

    def test_mcp_services_no_approval(self):
        registry = ToolRegistry()
        create_mcp_services_tools(registry)
        assert registry.get("mcp_services").requires_approval is False

    def test_mcp_services_invoke_requires_approval(self):
        registry = ToolRegistry()
        create_mcp_services_tools(registry)
        assert registry.get("mcp_services_invoke").requires_approval is True

    def test_state_changing_actions_not_in_read_tool_enum(self):
        registry = ToolRegistry()
        create_mcp_services_tools(registry)
        actions = (
            registry.get("mcp_services").input_schema["properties"]["action"]["enum"]
        )
        assert "proxy" not in actions
        assert "scale" not in actions
        assert set(actions) == {"list", "get"}


class TestMcpServicesDispatcher:
    def test_missing_action(self):
        result = _mcp_services()
        assert "error" in result
        assert "Missing 'action'" in result["error"]

    def test_unknown_action(self):
        result = _mcp_services(action="bogus", project_id="p")
        assert "error" in result
        assert "Unknown action" in result["error"]

    def test_no_project_id_returns_clear_error(self):
        # Empty creds env + tmpdir HOME = no project_id source.
        result = _mcp_services(action="list")
        assert "error" in result
        assert "project_id" in result["error"]

    def test_get_requires_instance_id(self, monkeypatch):
        monkeypatch.setenv("MARC27_PROJECT_ID", "00000000-0000-4000-8000-000000000001")
        result = _mcp_services(action="get")
        assert "error" in result
        assert "instance_id" in result["error"]

    def test_no_credentials_returns_login_hint(self, monkeypatch):
        # project_id resolves but token doesn't.
        monkeypatch.setenv("MARC27_PROJECT_ID", "00000000-0000-4000-8000-000000000001")
        result = _mcp_services(action="list")
        assert "error" in result
        assert "Not authenticated" in result["error"]
        assert "prism login" in result.get("hint", "")

    def test_list_action_hits_correct_url(self, monkeypatch):
        called = []

        class _StubResp:
            status_code = 200

            def json(self):
                return [{"id": "i-1"}]

        def _stub_get(url, **_kwargs):
            called.append(url)
            return _StubResp()

        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        monkeypatch.setenv("MARC27_PROJECT_ID", "proj-1")
        monkeypatch.setattr("app.tools.mcp_services.requests.get", _stub_get)

        result = _mcp_services(action="list")
        assert result == [{"id": "i-1"}]
        assert called == ["https://example.invalid/api/v1/projects/proj-1/mcp-services"]

    def test_get_action_hits_correct_url(self, monkeypatch):
        called = []

        class _StubResp:
            status_code = 200

            def json(self):
                return {"id": "inst-1"}

        def _stub_get(url, **_kwargs):
            called.append(url)
            return _StubResp()

        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        monkeypatch.setenv("MARC27_PROJECT_ID", "proj-1")
        monkeypatch.setattr("app.tools.mcp_services.requests.get", _stub_get)

        result = _mcp_services(action="get", instance_id="inst-1")
        assert result == {"id": "inst-1"}
        assert called == [
            "https://example.invalid/api/v1/projects/proj-1/mcp-services/inst-1"
        ]

    def test_explicit_project_id_overrides_env(self, monkeypatch):
        called = []

        class _StubResp:
            status_code = 200

            def json(self):
                return []

        def _stub_get(url, **_kwargs):
            called.append(url)
            return _StubResp()

        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        monkeypatch.setenv("MARC27_PROJECT_ID", "env-proj")
        monkeypatch.setattr("app.tools.mcp_services.requests.get", _stub_get)

        _mcp_services(action="list", project_id="explicit-proj")
        # Explicit beats env
        assert called == [
            "https://example.invalid/api/v1/projects/explicit-proj/mcp-services"
        ]


class TestMcpServicesInvoke:
    def test_missing_action(self):
        result = _mcp_services_invoke()
        assert "error" in result
        assert "Missing 'action'" in result["error"]

    def test_unknown_action(self):
        result = _mcp_services_invoke(
            action="bogus", project_id="p", instance_id="i"
        )
        assert "error" in result
        assert "Unknown action" in result["error"]

    def test_no_project_id_returns_clear_error(self):
        result = _mcp_services_invoke(action="proxy", instance_id="i", path="/x")
        assert "error" in result
        assert "project_id" in result["error"]

    def test_proxy_requires_instance_id(self, monkeypatch):
        monkeypatch.setenv("MARC27_PROJECT_ID", "p")
        result = _mcp_services_invoke(action="proxy", path="/x")
        assert "error" in result
        assert "instance_id" in result["error"]

    def test_proxy_requires_path(self, monkeypatch):
        monkeypatch.setenv("MARC27_PROJECT_ID", "p")
        result = _mcp_services_invoke(action="proxy", instance_id="i")
        assert "error" in result
        assert "path" in result["error"]

    def test_scale_requires_replicas(self, monkeypatch):
        monkeypatch.setenv("MARC27_PROJECT_ID", "p")
        result = _mcp_services_invoke(action="scale", instance_id="i")
        assert "error" in result
        assert "replicas" in result["error"]

    def test_scale_rejects_replicas_above_one(self, monkeypatch):
        monkeypatch.setenv("MARC27_PROJECT_ID", "p")
        result = _mcp_services_invoke(action="scale", instance_id="i", replicas=2)
        assert "error" in result
        assert "0 or 1" in result["error"]

    def test_proxy_action_hits_correct_url(self, monkeypatch):
        called = []
        sent_bodies = []

        class _StubResp:
            status_code = 200

            def json(self):
                return {"status_code": 200, "body": {"ok": True}, "headers": {}}

        def _stub_post(url, **kwargs):
            called.append(url)
            sent_bodies.append(kwargs.get("json"))
            return _StubResp()

        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        monkeypatch.setenv("MARC27_PROJECT_ID", "proj-1")
        monkeypatch.setattr("app.tools.mcp_services.requests.post", _stub_post)

        result = _mcp_services_invoke(
            action="proxy",
            instance_id="inst-1",
            path="/tools/list",
            method="GET",
        )
        assert result["status_code"] == 200
        assert called == [
            "https://example.invalid/api/v1/projects/proj-1/mcp-services/inst-1/proxy"
        ]
        assert sent_bodies[0]["path"] == "/tools/list"
        assert sent_bodies[0]["method"] == "GET"

    def test_scale_action_hits_correct_url(self, monkeypatch):
        called = []
        sent_bodies = []

        class _StubResp:
            status_code = 200

            def json(self):
                return {"instance_id": "inst-1", "replicas": 0}

        def _stub_post(url, **kwargs):
            called.append(url)
            sent_bodies.append(kwargs.get("json"))
            return _StubResp()

        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        monkeypatch.setenv("MARC27_PROJECT_ID", "proj-1")
        monkeypatch.setattr("app.tools.mcp_services.requests.post", _stub_post)

        result = _mcp_services_invoke(
            action="scale",
            instance_id="inst-1",
            replicas=0,
        )
        assert result == {"instance_id": "inst-1", "replicas": 0}
        assert called == [
            "https://example.invalid/api/v1/projects/proj-1/mcp-services/inst-1/scale"
        ]
        assert sent_bodies[0] == {"replicas": 0}
