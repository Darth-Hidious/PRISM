"""Execution engine for YAML-defined workflows."""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
import re
from typing import Any, Optional

import httpx

from app.workflows.registry import WorkflowSpec, WorkflowStep, argument_default

_TEMPLATE_RE = re.compile(r"\{\{\s*([a-zA-Z0-9_.-]+)\s*\}\}")


@dataclass
class WorkflowStepResult:
    id: str
    action: str
    status: str
    summary: str
    data: dict[str, Any] = field(default_factory=dict)


@dataclass
class WorkflowRunResult:
    workflow: str
    mode: str
    context: dict[str, Any]
    steps: list[WorkflowStepResult] = field(default_factory=list)


def build_initial_context(spec: WorkflowSpec, values: dict[str, Any]) -> dict[str, Any]:
    """Resolve workflow arguments and seed runtime context."""
    context = dict(values)
    for argument in spec.arguments:
        if argument.name not in context or context[argument.name] in (None, ""):
            default = argument_default(argument)
            if default not in (None, ""):
                context[argument.name] = default
        if argument.required and context.get(argument.name) in (None, "") and not argument.is_flag:
            raise ValueError(f"missing required workflow argument: {argument.name}")

    context.setdefault("workflow_name", spec.name)
    context.setdefault("command_name", spec.command_name)
    context.setdefault("now_iso", datetime.now(timezone.utc).isoformat())
    return context


def _resolve_path(context: dict[str, Any], path: str) -> Any:
    current: Any = context
    for segment in path.split("."):
        if isinstance(current, dict) and segment in current:
            current = current[segment]
        else:
            raise KeyError(path)
    return current


def _render_string(value: str, context: dict[str, Any]) -> Any:
    matches = _TEMPLATE_RE.findall(value)
    if not matches:
        return value

    if len(matches) == 1 and _TEMPLATE_RE.fullmatch(value):
        return _resolve_path(context, matches[0])

    def replace(match: re.Match[str]) -> str:
        resolved = _resolve_path(context, match.group(1))
        if isinstance(resolved, (dict, list)):
            return str(resolved)
        return "" if resolved is None else str(resolved)

    return _TEMPLATE_RE.sub(replace, value)


def render_value(value: Any, context: dict[str, Any]) -> Any:
    """Recursively render templates inside a manifest value."""
    if isinstance(value, str):
        return _render_string(value, context)
    if isinstance(value, list):
        return [render_value(item, context) for item in value]
    if isinstance(value, dict):
        return {key: render_value(val, context) for key, val in value.items()}
    return value


def _summarize_http_response(response: httpx.Response, parsed: Any) -> str:
    if isinstance(parsed, dict):
        if "id" in parsed:
            return f"HTTP {response.status_code} id={parsed['id']}"
        if "count" in parsed:
            return f"HTTP {response.status_code} count={parsed['count']}"
    return f"HTTP {response.status_code}"


def _run_set_step(step: WorkflowStep, context: dict[str, Any], dry_run: bool) -> WorkflowStepResult:
    values = render_value(step.config.get("values", {}), context)
    context[step.id] = values
    if isinstance(values, dict):
        context.update(values)
    return WorkflowStepResult(
        id=step.id,
        action=step.action,
        status="planned" if dry_run else "completed",
        summary=f"set {len(values) if isinstance(values, dict) else 1} value(s)",
        data={"values": values},
    )


def _run_message_step(step: WorkflowStep, context: dict[str, Any], dry_run: bool) -> WorkflowStepResult:
    text = render_value(step.config.get("text", ""), context)
    if not dry_run:
        context[step.id] = {"message": text}
    return WorkflowStepResult(
        id=step.id,
        action=step.action,
        status="planned" if dry_run else "completed",
        summary=str(text),
        data={"message": text},
    )


def _run_http_step(
    step: WorkflowStep,
    context: dict[str, Any],
    dry_run: bool,
    client: Optional[httpx.Client],
) -> WorkflowStepResult:
    method = str(step.config.get("method", "GET")).upper()
    url = str(render_value(step.config.get("url", ""), context))
    headers = render_value(step.config.get("headers", {}), context)
    body = render_value(step.config.get("body"), context)
    expect_status = step.config.get("expect_status", [200, 201, 202])
    if isinstance(expect_status, int):
        expect_status = [expect_status]

    if dry_run:
        return WorkflowStepResult(
            id=step.id,
            action=step.action,
            status="planned",
            summary=f"{method} {url}",
            data={"method": method, "url": url, "headers": headers, "body": body},
        )

    if client is None:
        client = httpx.Client(timeout=float(step.config.get("timeout_secs", 30)))

    response = client.request(method=method, url=url, headers=headers, json=body)
    if response.status_code not in expect_status:
        raise ValueError(
            f"workflow step {step.id} expected status {expect_status} but got {response.status_code}"
        )

    try:
        parsed = response.json()
    except Exception:
        parsed = {"text": response.text}

    stored = {
        "status_code": response.status_code,
        "headers": dict(response.headers),
        "body": parsed,
    }
    context[step.id] = stored
    context["last_response"] = stored
    return WorkflowStepResult(
        id=step.id,
        action=step.action,
        status="completed",
        summary=_summarize_http_response(response, parsed),
        data={"request": {"method": method, "url": url}, "response": stored},
    )


def execute_workflow(
    spec: WorkflowSpec,
    values: dict[str, Any],
    *,
    execute: bool = False,
    client: Optional[httpx.Client] = None,
) -> WorkflowRunResult:
    """Run or dry-run a workflow and return structured step results."""
    context = build_initial_context(spec, values)
    result = WorkflowRunResult(
        workflow=spec.name,
        mode="execute" if execute else "dry_run",
        context=dict(context),
    )

    for step in spec.steps:
        if step.action == "set":
            step_result = _run_set_step(step, context, dry_run=not execute)
        elif step.action == "message":
            step_result = _run_message_step(step, context, dry_run=not execute)
        elif step.action == "http":
            step_result = _run_http_step(step, context, dry_run=not execute, client=client)
        else:
            raise ValueError(f"unsupported workflow step action: {step.action}")
        result.steps.append(step_result)

    result.context = dict(context)
    return result
