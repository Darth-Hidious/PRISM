"""Slash-command handler implementations for the PRISM REPL.

Each handler is a plain function taking the REPL app as first arg.
Definitions (names, descriptions, aliases) live in registry.py;
rendering / UI lives in cli/tui/; handler *logic* lives here.
"""

import os
from typing import Optional

from app.agent.scratchpad import Scratchpad
from app.cli.slash.registry import REPL_COMMANDS, COMMAND_ALIASES
from app.cli.tui.theme import WARNING, SUCCESS, DIM


def _dot(ok: bool) -> str:
    return "[green]\u25cf[/green]" if ok else "[dim]\u25cb[/dim]"


# ── Dispatch ──────────────────────────────────────────────────────────

def handle_command(app, cmd: str) -> bool:
    """Route a slash-command string to the right handler.

    Returns True when the REPL should exit.
    """
    parts = cmd.strip().split(maxsplit=1)
    base_cmd = COMMAND_ALIASES.get(parts[0].lower(), parts[0].lower())
    arg = parts[1].strip() if len(parts) > 1 else ""

    if base_cmd in ("/exit", "/quit"):
        app.prompt_save_on_exit()
        app.console.print("[dim]Goodbye.[/dim]")
        return True
    elif base_cmd == "/clear":
        app.agent.reset()
        app.scratchpad = Scratchpad()
        app.agent.scratchpad = app.scratchpad
        app.console.print("[dim]Cleared.[/dim]")
    elif base_cmd == "/help":
        handle_help(app)
    elif base_cmd == "/history":
        app.console.print(f"[dim]{len(app.agent.history)} messages[/dim]")
    elif base_cmd == "/tools":
        handle_tools(app)
    elif base_cmd == "/skills":
        handle_skill(app, arg if arg else None)
    elif base_cmd == "/mcp":
        handle_mcp_status(app)
    elif base_cmd == "/save":
        app.memory.set_history(app.agent.history)
        if app.scratchpad:
            app.memory.set_scratchpad_entries(app.scratchpad.to_dict())
        sid = app.memory.save()
        app.console.print(f"[dim]Saved: {sid}[/dim]")
    elif base_cmd == "/export":
        handle_export(app, arg if arg else None)
    elif base_cmd == "/sessions":
        handle_sessions(app)
    elif base_cmd == "/load":
        if not arg:
            app.console.print("[dim]Usage: /load SESSION_ID[/dim]")
        else:
            handle_load(app, arg)
    elif base_cmd == "/plan":
        if not arg:
            app.console.print("[dim]Usage: /plan <goal>[/dim]")
        else:
            handle_plan(app, arg)
    elif base_cmd == "/scratchpad":
        handle_scratchpad(app)
    elif base_cmd == "/approve-all":
        app.agent.auto_approve = True
        app._auto_approve = True
        app.console.print("[yellow]Auto-approve on.[/yellow]")
    elif base_cmd == "/status":
        handle_status(app)
    elif base_cmd == "/login":
        handle_login(app)
    else:
        app.console.print(
            f"[dim]Unknown: {base_cmd}  \u2014  /help for commands[/dim]"
        )
    return False


# ── Individual handlers ───────────────────────────────────────────────

def handle_help(app):
    app.console.print()
    for name, desc in REPL_COMMANDS.items():
        if name == "/quit":
            continue
        app.console.print(f"  [bold]{name:<16}[/bold] [dim]{desc}[/dim]")
    app.console.print()


def handle_tools(app):
    app.console.print()
    tools = app.agent.tools.list_tools()
    for tool in tools:
        name_style = f"bold {WARNING}" if tool.requires_approval else "bold"
        flag = f" [{WARNING}]\u2605[/{WARNING}]" if tool.requires_approval else ""
        app.console.print(
            f"  [{name_style}]{tool.name:<28}[/{name_style}] "
            f"[dim]{tool.description[:55]}[/dim]{flag}"
        )
    app.console.print(
        f"\n  [dim]{len(tools)} tools[/dim]  "
        f"[{WARNING}]\u2605[/{WARNING}] [dim]= requires approval[/dim]"
    )
    app.console.print()


def handle_status(app):
    from app import __version__
    from app.cli.tui.welcome import detect_capabilities
    caps = detect_capabilities()

    app.console.print()
    app.console.print(f"[bold]PRISM[/bold] v{__version__}")
    app.console.print()

    provider = "not configured"
    if os.getenv("MARC27_TOKEN"):
        provider = "MARC27"
    elif os.getenv("ANTHROPIC_API_KEY"):
        provider = "Anthropic (Claude)"
    elif os.getenv("OPENAI_API_KEY"):
        provider = "OpenAI"
    elif os.getenv("OPENROUTER_API_KEY"):
        provider = "OpenRouter"
    app.console.print(
        f"  LLM          {_dot(provider != 'not configured')} {provider}"
    )

    labels = {"ML": "ML", "pyiron": "pyiron", "CALPHAD": "CALPHAD"}
    for key, ok in caps.items():
        label = labels.get(key, key)
        status = "[green]ready[/green]" if ok else "[dim]not installed[/dim]"
        app.console.print(f"  {label:<12}   {_dot(ok)} {status}")

    tool_count = len(app.agent.tools.list_tools())
    try:
        from app.skills.registry import load_builtin_skills
        skill_count = len(load_builtin_skills().list_skills())
    except Exception:
        skill_count = 0
    app.console.print(f"\n  [dim]{tool_count} tools \u00b7 {skill_count} skills[/dim]")

    missing = [n for n, a in caps.items() if not a]
    if missing:
        app.console.print(
            f"  [dim]pip install \"prism-platform[all]\" for {', '.join(missing)}[/dim]"
        )
    app.console.print()


def handle_login(app):
    from app.config.preferences import PRISM_DIR
    from rich.prompt import Prompt

    app.console.print()
    app.console.print("[bold]MARC27 Login[/bold]")
    app.console.print(
        "[dim]Connect your MARC27 account for managed LLM access.[/dim]"
    )
    app.console.print()

    token = os.getenv("MARC27_TOKEN")
    if token:
        app.console.print(
            f"  Already logged in. [dim](token: {token[:8]}...)[/dim]"
        )
        app.console.print("  [dim]To logout: unset MARC27_TOKEN[/dim]")
        app.console.print()
        return

    app.console.print(
        "  [dim]1.[/dim] Go to [bold]https://marc27.com/account/tokens[/bold]"
    )
    app.console.print("  [dim]2.[/dim] Create a PRISM API token")
    app.console.print("  [dim]3.[/dim] Paste it below")
    app.console.print()

    try:
        token_input = Prompt.ask("  Token", password=True)
    except (EOFError, KeyboardInterrupt):
        app.console.print("\n[dim]Cancelled.[/dim]")
        return

    if not token_input.strip():
        app.console.print("[dim]No token entered.[/dim]")
        return

    token_path = PRISM_DIR / "marc27_token"
    PRISM_DIR.mkdir(parents=True, exist_ok=True)
    token_path.write_text(token_input.strip())
    token_path.chmod(0o600)
    os.environ["MARC27_TOKEN"] = token_input.strip()

    app.console.print("[green]Logged in to MARC27.[/green]")
    app.console.print("[dim]Token saved to ~/.prism/marc27_token[/dim]")
    app.console.print()


def handle_skill(app, name: Optional[str] = None):
    try:
        from app.skills.registry import load_builtin_skills
        skills = load_builtin_skills()
    except Exception:
        app.console.print("[dim]No skills available.[/dim]")
        return

    app.console.print()
    if name:
        try:
            skill = skills.get(name)
        except KeyError:
            app.console.print(f"[dim]Skill not found: {name}[/dim]")
            return
        app.console.print(
            f"  [bold]{skill.name}[/bold]  [dim]{skill.category}[/dim]"
        )
        app.console.print(f"  {skill.description}")
        app.console.print()
        for i, step in enumerate(skill.steps, 1):
            opt = " [dim](optional)[/dim]" if step.optional else ""
            app.console.print(
                f"    {i}. {step.name} [dim]\u2014 {step.description}[/dim]{opt}"
            )
    else:
        for skill in skills.list_skills():
            app.console.print(
                f"  {skill.name:<25} [dim]{skill.description[:55]}[/dim]"
            )
    app.console.print()


def handle_plan(app, goal: str):
    prompt = f"The user wants to accomplish: {goal}\n\nAvailable PRISM skills:\n"
    try:
        from app.skills.registry import load_builtin_skills
        for skill in load_builtin_skills().list_skills():
            prompt += f"- {skill.name}: {skill.description}\n"
    except Exception:
        pass
    prompt += "\nWhich skill(s) should be used? Explain the recommended workflow."
    try:
        from app.cli.tui.stream import handle_streaming_response
        handle_streaming_response(
            app.console, app.agent, prompt, app.session, app.scratchpad,
        )
    except Exception as e:
        app.console.print(f"[red]Error: {e}[/red]")


def handle_scratchpad(app):
    if not app.scratchpad or not app.scratchpad.entries:
        app.console.print("[dim]Empty.[/dim]")
        return
    app.console.print()
    for i, entry in enumerate(app.scratchpad.entries, 1):
        tool = f" {entry.tool_name}" if entry.tool_name else ""
        app.console.print(
            f"  [dim]{i}.[/dim]{tool} {entry.summary} "
            f"[dim]{entry.timestamp}[/dim]"
        )
    app.console.print()


def handle_mcp_status(app):
    from app.mcp_client import load_mcp_config
    config = load_mcp_config()
    app.console.print()
    if not config.servers:
        app.console.print("  [dim]No MCP servers configured.[/dim]")
    else:
        for name in config.servers:
            app.console.print(f"  {name}")
    if app._mcp_tools:
        app.console.print(
            f"\n  [dim]{len(app._mcp_tools)} MCP tools loaded[/dim]"
        )
    app.console.print()


def handle_export(app, filename: Optional[str] = None):
    results = None
    for msg in reversed(app.agent.history):
        if msg.get("role") == "tool_result" and isinstance(
            msg.get("result"), dict
        ):
            r = msg["result"]
            if isinstance(r.get("results"), list) and r["results"]:
                results = r["results"]
                break
    if not results:
        app.console.print("[dim]No exportable results.[/dim]")
        return
    export_tool = app.agent.tools.get("export_results_csv")
    if export_tool is None:
        app.console.print("[dim]Export tool not available.[/dim]")
        return
    kwargs = {"results": results}
    if filename:
        kwargs["filename"] = filename
    out = export_tool.execute(**kwargs)
    if "error" in out:
        app.console.print(f"[red]{out['error']}[/red]")
    else:
        app.console.print(f"Exported {out['rows']} rows to {out['filename']}")


def handle_sessions(app):
    sessions = app.memory.list_sessions()
    if not sessions:
        app.console.print("[dim]No saved sessions.[/dim]")
        return
    app.console.print()
    for s in sessions[:20]:
        ts = s.get("timestamp", "")[:19]
        count = s.get("message_count", 0)
        app.console.print(
            f"  {s['session_id']}  [dim]{ts}  ({count} msgs)[/dim]"
        )
    app.console.print()


def handle_load(app, session_id: str):
    try:
        app.load_session(session_id)
        app.console.print(
            f"Loaded {session_id} ({len(app.agent.history)} messages)"
        )
    except FileNotFoundError:
        app.console.print(f"[red]Not found: {session_id}[/red]")
    except Exception as e:
        app.console.print(f"[red]Error: {e}[/red]")
