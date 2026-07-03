"""Tests for platform_jobs / platform_jobs_submit tools.

The submit action is broken out as a standalone tool with
`requires_approval=True`; the read/cancel/events actions live in the
unified `platform_jobs` tool with no approval gate.

Network-dependent behavior is mocked at the `requests` level; the
underlying platform routes are tested in marc27-core's own suite.
"""
from unittest.mock import patch

import pytest

from app.tools.base import ToolRegistry
from app.tools.platform_jobs import (
    _platform_jobs,
    _platform_jobs_submit,
    create_platform_jobs_tools,
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
        create_platform_jobs_tools(registry)
        names = {t.name for t in registry.list_tools()}
        assert names == {"platform_jobs", "platform_jobs_submit"}

    def test_platform_jobs_no_approval(self):
        registry = ToolRegistry()
        create_platform_jobs_tools(registry)
        assert registry.get("platform_jobs").requires_approval is False

    def test_platform_jobs_submit_requires_approval(self):
        registry = ToolRegistry()
        create_platform_jobs_tools(registry)
        assert registry.get("platform_jobs_submit").requires_approval is True

    def test_submit_action_not_in_read_tool_enum(self):
        """Submit must not appear in the read-only dispatcher's enum;
        approval gating depends on it being a separate tool."""
        registry = ToolRegistry()
        create_platform_jobs_tools(registry)
        actions = registry.get("platform_jobs").input_schema["properties"]["action"]["enum"]
        assert "submit" not in actions
        assert set(actions) == {"status", "cancel", "events"}


class TestPlatformJobsDispatcher:
    def test_missing_action(self):
        result = _platform_jobs()
        assert "error" in result
        assert "Missing 'action'" in result["error"]

    def test_unknown_action(self):
        result = _platform_jobs(action="bogus", job_id="abc")
        assert "error" in result
        assert "Unknown action" in result["error"]

    def test_status_requires_job_id(self):
        result = _platform_jobs(action="status")
        assert "error" in result
        assert "job_id" in result["error"]

    def test_cancel_requires_job_id(self):
        result = _platform_jobs(action="cancel")
        assert "error" in result
        assert "job_id" in result["error"]

    def test_events_requires_job_id(self):
        result = _platform_jobs(action="events")
        assert "error" in result
        assert "job_id" in result["error"]

    def test_no_credentials_returns_login_hint(self):
        result = _platform_jobs(action="status", job_id="11111111-1111-4111-8111-111111111111")
        assert "error" in result
        assert "Not authenticated" in result["error"]
        assert "prism login" in result.get("hint", "")

    def test_status_action_hits_correct_url(self, monkeypatch):
        called_paths = []

        class _StubResp:
            status_code = 200

            def json(self):
                return {"id": "abc", "status": "running"}

        def _stub_get(url, **_kwargs):
            called_paths.append(url)
            return _StubResp()

        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        monkeypatch.setattr("app.tools.platform_jobs.requests.get", _stub_get)

        result = _platform_jobs(action="status", job_id="job-xyz")
        assert result == {"id": "abc", "status": "running"}
        assert called_paths == ["https://example.invalid/api/v1/jobs/job-xyz"]

    def test_cancel_action_hits_correct_url(self, monkeypatch):
        called = []

        class _StubResp:
            status_code = 200

            @property
            def content(self):
                return b'{"status": "cancelled"}'

            def json(self):
                return {"status": "cancelled"}

        def _stub_post(url, **_kwargs):
            called.append(url)
            return _StubResp()

        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        monkeypatch.setattr("app.tools.platform_jobs.requests.post", _stub_post)

        result = _platform_jobs(action="cancel", job_id="job-xyz")
        assert result == {"status": "cancelled"}
        assert called == ["https://example.invalid/api/v1/jobs/job-xyz/cancel"]

    def test_events_action_reads_sse_frames(self, monkeypatch):
        called = []

        class _StubResp:
            status_code = 200

            def iter_lines(self, decode_unicode=True):
                # Two events with `data:` and `event:` fields, separated by
                # blank lines. The third frame is incomplete to verify we
                # cap at max_events.
                return iter([
                    "event: created",
                    'data: {"job_id": "abc"}',
                    "",
                    "event: started",
                    'data: {"job_id": "abc"}',
                    "",
                ])

            def close(self):
                pass

        def _stub_get(url, **_kwargs):
            called.append(url)
            return _StubResp()

        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        monkeypatch.setattr("app.tools.platform_jobs.requests.get", _stub_get)

        result = _platform_jobs(action="events", job_id="abc", max_events=10)
        assert result["count"] == 2
        assert called == ["https://example.invalid/api/v1/jobs/abc/events"]
        assert result["events"][0]["event"] == "created"
        assert result["events"][0]["data"] == {"job_id": "abc"}
        assert result["events"][1]["event"] == "started"


class TestPlatformJobsSubmit:
    def test_missing_job_type(self):
        result = _platform_jobs_submit(project_id="p", payload={})
        assert "error" in result
        assert "job_type" in result["error"]

    def test_missing_project_id(self):
        result = _platform_jobs_submit(job_type="t", payload={})
        assert "error" in result
        assert "project_id" in result["error"]

    def test_missing_payload(self):
        result = _platform_jobs_submit(job_type="t", project_id="p")
        assert "error" in result
        assert "payload" in result["error"]

    def test_no_credentials_returns_login_hint(self):
        result = _platform_jobs_submit(
            job_type="compute.simulation",
            project_id="11111111-1111-4111-8111-111111111111",
            payload={},
        )
        assert "error" in result
        assert "Not authenticated" in result["error"]

    def test_happy_path_hits_jobs_root(self, monkeypatch):
        called = []
        sent_bodies = []

        class _StubResp:
            status_code = 201

            @property
            def content(self):
                return b'{"id": "job-1"}'

            def json(self):
                return {"id": "job-1", "status": "queued"}

        def _stub_post(url, **kwargs):
            called.append(url)
            sent_bodies.append(kwargs.get("json"))
            return _StubResp()

        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv("MARC27_API_URL", "https://example.invalid/api/v1")
        monkeypatch.setattr("app.tools.platform_jobs.requests.post", _stub_post)

        result = _platform_jobs_submit(
            job_type="compute.simulation",
            project_id="00000000-0000-4000-8000-000000000001",
            payload={"input": "..."},
            priority=5,
        )
        assert result == {"id": "job-1", "status": "queued"}
        assert called == ["https://example.invalid/api/v1/jobs"]
        assert sent_bodies[0]["job_type"] == "compute.simulation"
        assert sent_bodies[0]["priority"] == 5
        assert sent_bodies[0]["payload"] == {"input": "..."}
