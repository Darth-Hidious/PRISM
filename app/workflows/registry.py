"""Workflow registry for YAML-defined PRISM workflows."""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional
import logging
import os

logger = logging.getLogger(__name__)

_BUILTIN_WORKFLOWS_DIR = Path(__file__).parent / "builtin"
_USER_WORKFLOWS_DIR = Path.home() / ".prism" / "workflows"


@dataclass(frozen=True)
class WorkflowArgument:
    """CLI argument/option definition for a workflow."""

    name: str
    type: str = "string"
    required: bool = False
    help: str = ""
    default: Any = None
    env: str = ""
    is_flag: bool = False


@dataclass(frozen=True)
class WorkflowStep:
    """One step in a workflow execution graph."""

    id: str
    action: str
    config: dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class WorkflowSpec:
    """Parsed workflow manifest."""

    name: str
    description: str
    command_name: str
    source_path: Path
    default_mode: str = "dry_run"
    arguments: tuple[WorkflowArgument, ...] = ()
    steps: tuple[WorkflowStep, ...] = ()
    raw: dict[str, Any] = field(default_factory=dict)


def workflow_search_paths(project_root: Path | None = None) -> list[Path]:
    """Return the workflow search paths in precedence order."""
    paths = [_BUILTIN_WORKFLOWS_DIR]
    if project_root is None:
        project_root = Path.cwd()
    paths.append(project_root / ".prism" / "workflows")
    paths.append(_USER_WORKFLOWS_DIR)
    return paths


def _load_yaml(path: Path) -> dict[str, Any]:
    import yaml  # noqa: delay import

    data = yaml.safe_load(path.read_text())
    if not isinstance(data, dict):
        raise ValueError(f"workflow file {path} must contain a mapping at the top level")
    return data


def _parse_argument(data: dict[str, Any]) -> WorkflowArgument:
    if "name" not in data:
        raise ValueError("workflow argument missing required field 'name'")
    return WorkflowArgument(
        name=str(data["name"]),
        type=str(data.get("type", "string")),
        required=bool(data.get("required", False)),
        help=str(data.get("help", "")),
        default=data.get("default"),
        env=str(data.get("env", "")),
        is_flag=bool(data.get("is_flag", False)),
    )


def _parse_step(data: dict[str, Any]) -> WorkflowStep:
    if "id" not in data or "action" not in data:
        raise ValueError("workflow step must contain 'id' and 'action'")
    return WorkflowStep(
        id=str(data["id"]),
        action=str(data["action"]),
        config={k: v for k, v in data.items() if k not in {"id", "action"}},
    )


def load_workflow_file(path: Path) -> WorkflowSpec:
    """Load and validate a workflow YAML file."""
    data = _load_yaml(path)
    if data.get("kind", "workflow") != "workflow":
        raise ValueError(f"{path} is not a workflow manifest")

    name = str(data.get("name") or path.stem)
    command_name = str(data.get("command_name") or name.replace("_", "-"))
    description = str(data.get("description", "")).strip() or f"Run workflow '{name}'"
    args = tuple(_parse_argument(arg) for arg in data.get("arguments", []) or [])
    steps = tuple(_parse_step(step) for step in data.get("steps", []) or [])
    return WorkflowSpec(
        name=name,
        description=description,
        command_name=command_name,
        source_path=path,
        default_mode=str(data.get("default_mode", "dry_run")),
        arguments=args,
        steps=steps,
        raw=data,
    )


def discover_workflows(project_root: Path | None = None) -> dict[str, WorkflowSpec]:
    """Load workflows from builtin, project, and user directories.

    Later directories override earlier ones on workflow name.
    """
    specs: dict[str, WorkflowSpec] = {}
    for directory in workflow_search_paths(project_root):
        if not directory.is_dir():
            continue
        for path in sorted(directory.glob("*.y*ml")):
            try:
                spec = load_workflow_file(path)
            except ImportError:
                logger.debug("PyYAML not installed; skipping workflows")
                return specs
            except Exception as exc:
                logger.warning("Failed to load workflow %s: %s", path, exc)
                continue
            specs[spec.name] = spec
    return specs


def get_workflow(name: str, project_root: Path | None = None) -> Optional[WorkflowSpec]:
    """Look up a workflow by name or command name."""
    specs = discover_workflows(project_root)
    if name in specs:
        return specs[name]
    for spec in specs.values():
        if spec.command_name == name:
            return spec
    return None


def argument_default(argument: WorkflowArgument) -> Any:
    """Resolve the default value for a workflow argument."""
    if argument.env:
        env_value = os.getenv(argument.env)
        if env_value not in (None, ""):
            return env_value
    return argument.default

