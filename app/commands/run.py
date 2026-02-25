"""Run CLI command: autonomous agent mode."""
import click
from rich.console import Console
from rich.panel import Panel


@click.command("run")
@click.argument("goal")
@click.option("--provider", default=None, help="LLM provider (anthropic/openai/openrouter)")
@click.option("--model", default=None, help="Model name override")
@click.option("--confirm", is_flag=True, help="Require confirmation for expensive tools")
@click.option("--dangerously-accept-all", "accept_all", is_flag=True, help="Auto-approve all tool calls")
@click.pass_context
def run_goal(ctx, goal, provider, model, confirm, accept_all):
    """Run PRISM agent autonomously on a research goal."""
    from rich.live import Live
    from rich.markdown import Markdown
    from rich.text import Text
    from app.agent.events import TextDelta, ToolCallStart, ToolCallResult, TurnComplete
    from app.agent.factory import create_backend
    from app.agent.autonomous import run_autonomous_stream

    no_mcp = ctx.obj.get("no_mcp", False) if ctx.obj else False
    run_console = Console()
    try:
        backend = create_backend(provider=provider, model=model)
        run_console.print(Panel.fit(f"[bold]Goal:[/bold] {goal}", border_style="cyan"))
        accumulated_text = ""
        with Live("", console=run_console, refresh_per_second=15, vertical_overflow="visible") as live:
            effective_confirm = confirm and not accept_all
            for event in run_autonomous_stream(goal=goal, backend=backend, enable_mcp=not no_mcp, confirm=effective_confirm):
                if isinstance(event, TextDelta):
                    accumulated_text += event.text
                    live.update(Text(accumulated_text))
                elif isinstance(event, ToolCallStart):
                    live.update("")
                    run_console.print(Panel(
                        f"[dim]Calling...[/dim]",
                        title=f"[bold yellow]{event.tool_name}[/bold yellow]",
                        border_style="yellow",
                        expand=False,
                    ))
                    accumulated_text = ""
                elif isinstance(event, ToolCallResult):
                    run_console.print(Panel(
                        f"[green]{event.summary}[/green]",
                        title=f"[bold green]{event.tool_name}[/bold green]",
                        border_style="green",
                        expand=False,
                    ))
                elif isinstance(event, TurnComplete):
                    live.update("")
        if accumulated_text:
            run_console.print()
            run_console.print(Markdown(accumulated_text))
    except ValueError as e:
        run_console.print(f"[red]Error: {e}[/red]")
    except Exception as e:
        run_console.print(f"[red]Agent error: {e}[/red]")
