"""Run CLI command: autonomous agent mode."""
import time
import click
from rich.console import Console
from rich.live import Live
from rich.markdown import Markdown
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
    from app.agent.events import TextDelta, ToolCallStart, ToolCallResult, TurnComplete
    from app.agent.factory import create_backend
    from app.agent.autonomous import run_autonomous_stream
    from app.plugins.bootstrap import build_full_registry
    from app.cli.tui.cards import render_tool_result, render_cost_line
    from app.cli.tui.spinner import Spinner

    no_mcp = ctx.obj.get("no_mcp", False) if ctx.obj else False
    run_console = Console(highlight=False)

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
        spinner = Spinner(console=run_console)
        accumulated_text = ""
        session_cost = 0.0
        tool_start_time = None

        with Live("", console=run_console, refresh_per_second=15,
                  vertical_overflow="visible") as live:
            effective_confirm = confirm and not accept_all
            for event in run_autonomous_stream(
                goal=goal, backend=backend, tools=tool_reg,
                system_prompt=system_prompt,
                enable_mcp=not no_mcp, confirm=effective_confirm,
            ):
                if isinstance(event, TextDelta):
                    accumulated_text += event.text
                    live.update(Markdown(accumulated_text))

                elif isinstance(event, ToolCallStart):
                    live.update("")
                    if accumulated_text.strip():
                        run_console.print(Markdown(accumulated_text))
                    accumulated_text = ""
                    tool_start_time = time.monotonic()
                    verb = spinner.verb_for_tool(event.tool_name)
                    spinner.start(verb)

                elif isinstance(event, ToolCallResult):
                    spinner.stop()
                    elapsed_ms = 0.0
                    if tool_start_time:
                        elapsed_ms = (time.monotonic() - tool_start_time) * 1000
                        tool_start_time = None
                    result = event.result if isinstance(event.result, dict) else {}
                    render_tool_result(
                        run_console, event.tool_name, event.summary, elapsed_ms, result,
                    )

                elif isinstance(event, TurnComplete):
                    spinner.stop()
                    live.update("")
                    if accumulated_text.strip():
                        run_console.print(Markdown(accumulated_text))
                    accumulated_text = ""
                    tool_start_time = None
                    usage = event.usage or event.total_usage
                    if usage:
                        turn_cost = event.estimated_cost
                        if turn_cost is not None:
                            session_cost += turn_cost
                        render_cost_line(run_console, usage, turn_cost, session_cost)

    except ValueError as e:
        run_console.print(f"[red]Error: {e}[/red]")
    except Exception as e:
        run_console.print(f"[red]Agent error: {e}[/red]")
