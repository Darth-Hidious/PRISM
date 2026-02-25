"""Run CLI command: autonomous agent mode."""
import click
from rich.console import Console
from rich.panel import Panel


@click.command("run")
@click.argument("goal")
@click.option("--agent", default=None, help="Use a named agent config from the registry")
@click.option("--provider", default=None, help="LLM provider (anthropic/openai/openrouter)")
@click.option("--model", default=None, help="Model name override")
@click.option("--confirm", is_flag=True, help="Require confirmation for expensive tools")
@click.option("--dangerously-accept-all", "accept_all", is_flag=True, help="Auto-approve all tool calls")
@click.pass_context
def run_goal(ctx, goal, agent, provider, model, confirm, accept_all):
    """Run PRISM agent autonomously on a research goal."""
    from rich.live import Live
    from rich.markdown import Markdown
    from rich.text import Text
    from app.agent.events import TextDelta, ToolCallStart, ToolCallResult, TurnComplete
    from app.agent.factory import create_backend
    from app.agent.autonomous import run_autonomous_stream
    from app.plugins.bootstrap import build_full_registry

    no_mcp = ctx.obj.get("no_mcp", False) if ctx.obj else False
    run_console = Console()

    # Build registries
    tool_reg, _provider_reg, agent_reg = build_full_registry(enable_mcp=not no_mcp)

    # Resolve agent config if specified
    system_prompt = None
    if agent:
        agent_config = agent_reg.get(agent)
        if not agent_config:
            run_console.print(f"[red]Unknown agent: {agent}[/red]")
            run_console.print(f"[dim]Available: {', '.join(c.id for c in agent_reg.get_all())}[/dim]")
            return
        if not agent_config.enabled:
            run_console.print(f"[red]Agent '{agent}' is not enabled.[/red]")
            return
        system_prompt = agent_config.system_prompt or None
        run_console.print(Panel.fit(
            f"[bold]Agent:[/bold] {agent_config.name}\n[bold]Goal:[/bold] {goal}",
            border_style="cyan",
        ))
    else:
        run_console.print(Panel.fit(f"[bold]Goal:[/bold] {goal}", border_style="cyan"))

    try:
        backend = create_backend(provider=provider, model=model)
        accumulated_text = ""

        def _flush_text(live):
            """Flush accumulated text permanently above the live area."""
            nonlocal accumulated_text
            if accumulated_text.strip():
                live.update("")
                run_console.print(Markdown(accumulated_text))
            else:
                live.update("")
            accumulated_text = ""

        with Live("", console=run_console, refresh_per_second=15, vertical_overflow="visible") as live:
            effective_confirm = confirm and not accept_all
            for event in run_autonomous_stream(
                goal=goal, backend=backend, tools=tool_reg,
                system_prompt=system_prompt,
                enable_mcp=not no_mcp, confirm=effective_confirm,
            ):
                if isinstance(event, TextDelta):
                    accumulated_text += event.text
                    live.update(Text(accumulated_text))
                elif isinstance(event, ToolCallStart):
                    _flush_text(live)
                    run_console.print(Panel(
                        f"[dim]Calling...[/dim]",
                        title=f"[bold yellow]{event.tool_name}[/bold yellow]",
                        border_style="yellow",
                        expand=False,
                    ))
                elif isinstance(event, ToolCallResult):
                    run_console.print(Panel(
                        f"[green]{event.summary}[/green]",
                        title=f"[bold green]{event.tool_name}[/bold green]",
                        border_style="green",
                        expand=False,
                    ))
                elif isinstance(event, TurnComplete):
                    _flush_text(live)
                    if event.estimated_cost is not None:
                        run_console.print(
                            f"[dim]tokens: {event.total_usage.input_tokens:,}in "
                            f"+ {event.total_usage.output_tokens:,}out "
                            f"| cost: ${event.estimated_cost:.4f}[/dim]"
                        )
    except ValueError as e:
        run_console.print(f"[red]Error: {e}[/red]")
    except Exception as e:
        run_console.print(f"[red]Agent error: {e}[/red]")
