"""Tests for YAML workflow loading and execution."""

from pathlib import Path

from app.workflows.engine import execute_workflow
from app.workflows.registry import discover_workflows, load_workflow_file


def test_builtin_forge_workflow_loads():
    workflows = discover_workflows()
    assert "forge" in workflows
    assert workflows["forge"].command_name == "forge"


def test_load_custom_workflow_file(tmp_path):
    path = tmp_path / "hello.yaml"
    path.write_text(
        """
kind: workflow
name: hello
command_name: hello
description: Test workflow
arguments:
  - name: who
    required: true
steps:
  - id: greet
    action: message
    text: "Hello {{who}}"
""".strip()
    )
    spec = load_workflow_file(path)
    assert spec.name == "hello"
    assert spec.arguments[0].name == "who"
    assert spec.steps[0].action == "message"


def test_execute_workflow_dry_run_message_step(tmp_path):
    path = tmp_path / "hello.yaml"
    path.write_text(
        """
kind: workflow
name: hello
steps:
  - id: one
    action: set
    values:
      greeting: "hello"
  - id: two
    action: message
    text: "{{greeting}} world"
""".strip()
    )
    spec = load_workflow_file(path)
    result = execute_workflow(spec, {}, execute=False)
    assert result.mode == "dry_run"
    assert [step.id for step in result.steps] == ["one", "two"]
    assert result.steps[1].summary == "hello world"


def test_execute_http_step_with_mock_client(tmp_path):
    class _Response:
        status_code = 200
        headers = {"content-type": "application/json"}

        @staticmethod
        def json():
            return {"id": "job-123"}

    class _Client:
        def __init__(self):
            self.requests = []

        def request(self, **kwargs):
            self.requests.append(kwargs)
            return _Response()

    path = tmp_path / "submit.yaml"
    path.write_text(
        """
kind: workflow
name: submit
arguments:
  - name: project_id
    required: true
steps:
  - id: submit_job
    action: http
    method: POST
    url: "https://api.marc27.com/api/v1/projects/{{project_id}}/compute/submit"
    body:
      kind: train
""".strip()
    )
    spec = load_workflow_file(path)
    client = _Client()
    result = execute_workflow(spec, {"project_id": "proj-1"}, execute=True, client=client)
    assert result.steps[0].status == "completed"
    assert client.requests[0]["url"].endswith("/projects/proj-1/compute/submit")
    assert result.context["submit_job"]["body"]["id"] == "job-123"
