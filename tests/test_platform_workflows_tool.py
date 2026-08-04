"""Tests for platform_workflows / platform_workflows_run tools.

start + register_spec are broken out as approval-gated under
`platform_workflows_run`; list/list_specs/status/cancel live in
`platform_workflows` with no approval gate.

Network-dependent behavior is stubbed at the socket under
`PlatformClient` (the shared `platform_http` fixture); the underlying
platform routes are tested in marc27-core's own suite.
"""
import pytest

from tests.conftest import assert_not_connected

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
        assert_not_connected(result)

    def test_valid_actions_call_correct_endpoint(self, monkeypatch, platform_http):
        """list / list_specs / status hit distinct GET endpoints,
        cancel hits a POST endpoint."""
        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")

        assert _platform_workflows(action="list") == {"ok": True}
        assert _platform_workflows(action="list_specs") == {"ok": True}
        assert _platform_workflows(action="status", workflow_id="wf-1") == {"ok": True}
        _platform_workflows(action="cancel", workflow_id="wf-1")

        assert platform_http.urls_for("GET") == [
            "https://example.invalid/api/v1/workflows",
            "https://example.invalid/api/v1/workflows/specs",
            "https://example.invalid/api/v1/workflows/wf-1",
        ]
        assert platform_http.urls_for("POST") == [
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
        assert_not_connected(result)

    def test_start_action_hits_workflows_root(self, monkeypatch, platform_http):
        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        platform_http.status_code = 202
        platform_http.payload = {"workflow_id": "wf-1", "status": "running"}

        result = _platform_workflows_run(
            action="start",
            spec="discover-mof",
            inputs={"target": "co2"},
            project_id="00000000-0000-4000-8000-000000000001",
        )
        assert result["workflow_id"] == "wf-1"
        assert platform_http.urls_for("POST") == [
            "https://example.invalid/api/v1/workflows"
        ]
        body = platform_http.bodies[0]
        assert body["spec"] == "discover-mof"
        assert body["inputs"] == {"target": "co2"}
        assert body["project_id"] == "00000000-0000-4000-8000-000000000001"

    def test_register_spec_hits_specs_endpoint(self, monkeypatch, platform_http):
        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        platform_http.status_code = 201
        platform_http.payload = {"id": "spec-1", "name": "x"}

        result = _platform_workflows_run(
            action="register_spec",
            spec_yaml="name: foo\nsteps: []\n",
        )
        assert result == {"id": "spec-1", "name": "x"}
        assert platform_http.urls_for("POST") == [
            "https://example.invalid/api/v1/workflows/specs"
        ]
