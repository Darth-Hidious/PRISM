"""Tests for platform_workflows / platform_workflows_run tools.

start + register_spec are broken out as approval-gated under
`platform_workflows_run`; list/list_specs/status/cancel live in
`platform_workflows` with no approval gate.

Network-dependent behavior is mocked at the `requests` level; the
underlying platform routes are tested in marc27-core's own suite.
"""
from unittest.mock import patch

import pytest

from app.tools.base import ToolRegistry
from app.tools.platform_workflows import (
    _platform_workflows,
    _platform_workflows_run,
    create_platform_workflows_tools,
)


@pytest.fixture(autouse=True)
def _no_credentials_env(monkeypatch, tmp_path):
    """Run each test with empty creds env + a non-existent creds file
    so the tools take the "not authenticated" branch deterministically."""
    monkeypatch.delenv("MARC27_API_KEY", raising=False)
    monkeypatch.delenv("MARC27_API_URL", raising=False)
    monkeypatch.setenv("HOME", str(tmp_path))


class TestRegistration:
    def test_registers_two_tools(self):
        registry = ToolRegistry()
        create_platform_workflows_tools(registry)
        names = {t.name for t in registry.list_tools()}
        assert names == {"platform_workflows", "platform_workflows_run"}

    def test_platform_workflows_no_approval(self):
        registry = ToolRegistry()
        create_platform_workflows_tools(registry)
        assert registry.get("platform_workflows").requires_approval is False

    def test_platform_workflows_run_requires_approval(self):
        registry = ToolRegistry()
        create_platform_workflows_tools(registry)
        assert registry.get("platform_workflows_run").requires_approval is True

    def test_state_changing_actions_not_in_read_tool_enum(self):
        registry = ToolRegistry()
        create_platform_workflows_tools(registry)
        actions = (
            registry.get("platform_workflows").input_schema["properties"]["action"]["enum"]
        )
        assert "start" not in actions
        assert "register_spec" not in actions
        assert set(actions) == {"list", "list_specs", "status", "cancel"}


class TestPlatformWorkflowsDispatcher:
    def test_missing_action(self):
        result = _platform_workflows()
        assert "error" in result
        assert "Missing 'action'" in result["error"]

    def test_unknown_action(self):
        result = _platform_workflows(action="bogus")
        assert "error" in result
        assert "Unknown action" in result["error"]

    def test_status_requires_workflow_id(self):
        result = _platform_workflows(action="status")
        assert "error" in result
        assert "workflow_id" in result["error"]

    def test_cancel_requires_workflow_id(self):
        result = _platform_workflows(action="cancel")
        assert "error" in result
        assert "workflow_id" in result["error"]

    def test_no_credentials_returns_login_hint(self):
        result = _platform_workflows(action="list")
        assert "error" in result
        assert "Not authenticated" in result["error"]
        assert "prism login" in result.get("hint", "")

    def test_valid_actions_call_correct_endpoint(self, monkeypatch):
        """list / list_specs / status hit distinct GET endpoints,
        cancel hits a POST endpoint."""
        called_get = []
        called_post = []

        class _StubResp:
            status_code = 200

            @property
            def content(self):
                return b'{"ok": true}'

            def json(self):
                return {"ok": True}

        def _stub_get(url, **_kwargs):
            called_get.append(url)
            return _StubResp()

        def _stub_post(url, **_kwargs):
            called_post.append(url)
            return _StubResp()

        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        monkeypatch.setattr("app.tools.platform_workflows.requests.get", _stub_get)
        monkeypatch.setattr("app.tools.platform_workflows.requests.post", _stub_post)

        assert _platform_workflows(action="list") == {"ok": True}
        assert _platform_workflows(action="list_specs") == {"ok": True}
        assert _platform_workflows(action="status", workflow_id="wf-1") == {"ok": True}
        _platform_workflows(action="cancel", workflow_id="wf-1")

        assert called_get == [
            "https://example.invalid/api/v1/workflows",
            "https://example.invalid/api/v1/workflows/specs",
            "https://example.invalid/api/v1/workflows/wf-1",
        ]
        assert called_post == [
            "https://example.invalid/api/v1/workflows/wf-1/cancel",
        ]


class TestPlatformWorkflowsRun:
    def test_missing_action(self):
        result = _platform_workflows_run()
        assert "error" in result
        assert "Missing 'action'" in result["error"]

    def test_unknown_action(self):
        result = _platform_workflows_run(action="bogus")
        assert "error" in result
        assert "Unknown action" in result["error"]

    def test_start_requires_spec(self):
        result = _platform_workflows_run(action="start")
        assert "error" in result
        assert "spec" in result["error"]

    def test_register_spec_requires_spec_yaml(self):
        result = _platform_workflows_run(action="register_spec")
        assert "error" in result
        assert "spec_yaml" in result["error"]

    def test_no_credentials_returns_login_hint(self):
        result = _platform_workflows_run(action="start", spec="my-spec")
        assert "error" in result
        assert "Not authenticated" in result["error"]

    def test_start_action_hits_workflows_root(self, monkeypatch):
        called = []
        sent_bodies = []

        class _StubResp:
            status_code = 202

            @property
            def content(self):
                return b'{"workflow_id": "wf-1"}'

            def json(self):
                return {"workflow_id": "wf-1", "status": "running"}

        def _stub_post(url, **kwargs):
            called.append(url)
            sent_bodies.append(kwargs.get("json"))
            return _StubResp()

        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        monkeypatch.setattr("app.tools.platform_workflows.requests.post", _stub_post)

        result = _platform_workflows_run(
            action="start",
            spec="discover-mof",
            inputs={"target": "co2"},
            project_id="00000000-0000-4000-8000-000000000001",
        )
        assert result["workflow_id"] == "wf-1"
        assert called == ["https://example.invalid/api/v1/workflows"]
        assert sent_bodies[0]["spec"] == "discover-mof"
        assert sent_bodies[0]["inputs"] == {"target": "co2"}
        assert sent_bodies[0]["project_id"] == "00000000-0000-4000-8000-000000000001"

    def test_register_spec_hits_specs_endpoint(self, monkeypatch):
        called = []

        class _StubResp:
            status_code = 201

            @property
            def content(self):
                return b'{"id": "spec-1"}'

            def json(self):
                return {"id": "spec-1", "name": "x"}

        def _stub_post(url, **_kwargs):
            called.append(url)
            return _StubResp()

        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        monkeypatch.setattr("app.tools.platform_workflows.requests.post", _stub_post)

        result = _platform_workflows_run(
            action="register_spec",
            spec_yaml="name: foo\nsteps: []\n",
        )
        assert result == {"id": "spec-1", "name": "x"}
        assert called == ["https://example.invalid/api/v1/workflows/specs"]
