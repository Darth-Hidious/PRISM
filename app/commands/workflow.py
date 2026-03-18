"""Workflow CLI commands and dynamic YAML workflow command registration."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Callable

import click
from rich.console import Console
from rich.table import Table

from app.workflows.engine import execute_workflow
from app.workflows.registry import WorkflowArgument, WorkflowSpec, discover_workflows, get_workflow

console = Console()


def _click_type(argument: WorkflowArgument):
    if argument.is_flag:
        return None
    mapping = {
        "string": click.STRING,
        "int": click.INT,
        "integer": click.INT,
        "float": click.FLOAT,
        "bool": click.BOOL,
    }
    return mapping.get(argument.type, click.STRING)


def _workflow_values_from_kwargs(spec: WorkflowSpec, kwargs: dict[str, Any]) -> dict[str, Any]:
    values = {}
    for argument in spec.arguments:
        values[argument.name] = kwargs.get(argument.name)
    return values


def _render_workflow_result(spec: WorkflowSpec, result) -> None:
    console.print()
    console.print(f"[bold]{spec.command_name}[/bold]  [dim]{result.mode}[/dim]")
    console.print(f"[dim]{spec.description}[/dim]")
    console.print()

    table = Table(show_header=True, header_style="bold cyan")
    table.add_column("Step")
    table.add_column("Action")
    table.add_column("Status")
    table.add_column("Summary")
    for step in result.steps:
        table.add_row(step.id, step.action, step.status, step.summary)
    console.print(table)
    console.print()


def run_workflow_spec(spec: WorkflowSpec, *, execute: bool, **kwargs) -> None:
    """Execute or dry-run a workflow spec from CLI kwargs."""
    values = _workflow_values_from_kwargs(spec, kwargs)
    result = execute_workflow(spec, values, execute=execute)
    _render_workflow_result(spec, result)


@click.group("workflow")
def workflow_group():
    """Manage and run YAML-defined workflows."""


@workflow_group.command("list")
def workflow_list():
    """List discovered workflows."""
    workflows = discover_workflows()
    if not workflows:
        console.print("[dim]No workflows found.[/dim]")
        return

    table = Table(show_header=True, header_style="bold cyan")
    table.add_column("Name")
    table.add_column("Command")
    table.add_column("Source")
    table.add_column("Description")
    for spec in workflows.values():
        table.add_row(spec.name, spec.command_name, str(spec.source_path), spec.description)
    console.print(table)


@workflow_group.command("show")
@click.argument("name")
def workflow_show(name: str):
    """Show workflow metadata."""
    spec = get_workflow(name)
    if not spec:
        raise click.ClickException(f"Workflow not found: {name}")

    console.print()
    console.print(f"[bold]{spec.name}[/bold]  [dim]{spec.command_name}[/dim]")
    console.print(spec.description)
    console.print(f"[dim]{spec.source_path}[/dim]")
    console.print()
    for argument in spec.arguments:
        required = "required" if argument.required else "optional"
        console.print(f"  --{argument.name}  [dim]{argument.type} · {required}[/dim]  {argument.help}")
    console.print()


@workflow_group.command("run")
@click.argument("name")
@click.option("--set", "pairs", multiple=True, help="Set workflow values as key=value pairs")
@click.option("--execute", is_flag=True, help="Execute HTTP steps instead of dry-running them")
def workflow_run(name: str, pairs: tuple[str, ...], execute: bool):
    """Run a workflow by name."""
    spec = get_workflow(name)
    if not spec:
        raise click.ClickException(f"Workflow not found: {name}")

    values: dict[str, Any] = {}
    for pair in pairs:
        if "=" not in pair:
            raise click.ClickException(f"Invalid --set value: {pair}. Expected key=value.")
        key, value = pair.split("=", 1)
        values[key] = value

    result = execute_workflow(spec, values, execute=execute)
    _render_workflow_result(spec, result)


def make_workflow_command(spec: WorkflowSpec) -> click.Command:
    """Convert a workflow spec into a root CLI command."""
    params: list[click.Parameter] = []

    for argument in reversed(spec.arguments):
        click_type = _click_type(argument)
        option = click.Option(
            param_decls=[f"--{argument.name.replace('_', '-')}"],
            required=argument.required and not argument.is_flag,
            help=argument.help,
            is_flag=argument.is_flag,
            type=click_type,
            default=None,
            show_default=False,
        )
        params.insert(0, option)

    params.append(
        click.Option(
            param_decls=["--execute"],
            is_flag=True,
            default=(spec.default_mode == "execute"),
            help="Execute HTTP steps instead of dry-running them.",
        )
    )

    def callback(**kwargs):
        execute = bool(kwargs.pop("execute", False))
        run_workflow_spec(spec, execute=execute, **kwargs)

    return click.Command(
        name=spec.command_name,
        params=params,
        callback=callback,
        help=spec.description,
        short_help=spec.description,
    )


def register_workflow_commands(root: click.Group, project_root: Path | None = None) -> None:
    """Register the workflow group and root-level workflow aliases."""
    root.add_command(workflow_group, "workflow")
    workflows = discover_workflows(project_root=project_root)
    existing = set(root.commands)
    for spec in workflows.values():
        if spec.command_name in existing:
            continue
        root.add_command(make_workflow_command(spec), spec.command_name)
        existing.add(spec.command_name)

