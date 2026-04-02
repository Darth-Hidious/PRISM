"""Execution registry — frozen, scored dispatch for tools and commands.

- Immutable registry assembled once at session start
- Token-based routing scores prompts against tool metadata
- Case-insensitive lookup, deterministic scoring
- Permission filtering applied at assembly time (not per-call)
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Callable, Optional

from app.agent.permissions import ToolPermissionContext


@dataclass(frozen=True)
class RoutedMatch:
    """A scored match from prompt routing."""
    kind: str          # 'command' or 'tool'
    name: str
    source_hint: str   # module/category the tool belongs to
    score: int         # higher = better match


@dataclass(frozen=True)
class RegisteredTool:
    """A tool in the execution registry — frozen, safe to share."""
    name: str
    description: str
    category: str       # 'search', 'compute', 'knowledge', 'code', 'data', 'system'
    requires_approval: bool
    execute: Callable    # the actual function


@dataclass(frozen=True)
class RegisteredCommand:
    """A slash command — frozen, safe to share."""
    name: str
    description: str
    execute: Callable


class ExecutionRegistry:
    """Immutable-after-build registry of tools and commands.

    Assembled once at session start with permission filtering.
    Provides token-based routing for prompt → tool matching.
    """

    def __init__(
        self,
        tools: tuple[RegisteredTool, ...],
        commands: tuple[RegisteredCommand, ...],
    ):
        self._tools = tools
        self._commands = commands
        # Build case-insensitive lookup maps
        self._tool_map = {t.name.lower(): t for t in tools}
        self._cmd_map = {c.name.lower(): c for c in commands}

    @property
    def tools(self) -> tuple[RegisteredTool, ...]:
        return self._tools

    @property
    def commands(self) -> tuple[RegisteredCommand, ...]:
        return self._commands

    def tool(self, name: str) -> Optional[RegisteredTool]:
        """Case-insensitive tool lookup."""
        return self._tool_map.get(name.lower())

    def command(self, name: str) -> Optional[RegisteredCommand]:
        """Case-insensitive command lookup."""
        return self._cmd_map.get(name.lower())

    def route(self, prompt: str, limit: int = 5) -> list[RoutedMatch]:
        """Score tools and commands against a prompt using token matching.

        Deterministic, no LLM needed. Returns top-K matches sorted by score.
        """
        tokens = set(prompt.lower().split())
        matches: list[RoutedMatch] = []

        for tool in self._tools:
            score = self._score(tokens, tool.name, tool.description, tool.category)
            if score > 0:
                matches.append(RoutedMatch(
                    kind='tool', name=tool.name,
                    source_hint=tool.category, score=score,
                ))

        for cmd in self._commands:
            score = self._score(tokens, cmd.name, cmd.description, "command")
            if score > 0:
                matches.append(RoutedMatch(
                    kind='command', name=cmd.name,
                    source_hint='command', score=score,
                ))

        matches.sort(key=lambda m: m.score, reverse=True)
        return matches[:limit]

    @staticmethod
    def _score(tokens: set[str], name: str, description: str, category: str) -> int:
        """Score a tool/command against prompt tokens."""
        target_words = set()
        target_words.update(name.lower().replace('_', ' ').split())
        target_words.update(description.lower().split()[:20])  # first 20 words
        target_words.add(category.lower())
        return len(tokens & target_words)

    @staticmethod
    def from_tool_registry(
        tool_registry,
        permission_ctx: ToolPermissionContext | None = None,
    ) -> 'ExecutionRegistry':
        """Build from PRISM's existing ToolRegistry, filtering by permissions."""
        ctx = permission_ctx or ToolPermissionContext.default()
        tools = []
        for t in tool_registry.list_tools():
            if ctx.blocks(t.name):
                continue
            tools.append(RegisteredTool(
                name=t.name,
                description=t.description,
                category=_categorize(t.name),
                requires_approval=t.requires_approval and not ctx.auto_approves(t.name),
                execute=t.func,
            ))

        # Slash commands are hardcoded for now
        commands = _build_commands()

        return ExecutionRegistry(
            tools=tuple(tools),
            commands=tuple(commands),
        )


def _categorize(tool_name: str) -> str:
    """Assign a category to a tool based on its name."""
    name = tool_name.lower()
    if any(k in name for k in ('search', 'query', 'literature', 'patent')):
        return 'search'
    if any(k in name for k in ('knowledge', 'semantic', 'corpora', 'graph')):
        return 'knowledge'
    if any(k in name for k in ('compute', 'gpu', 'job', 'submit')):
        return 'compute'
    if any(k in name for k in ('predict', 'model', 'train')):
        return 'ml'
    if any(k in name for k in ('execute', 'python', 'code', 'bash')):
        return 'code'
    if any(k in name for k in ('web', 'read', 'write', 'file')):
        return 'io'
    if any(k in name for k in ('plot', 'visual', 'export')):
        return 'viz'
    if any(k in name for k in ('calphad', 'phase', 'equilibrium', 'gibbs')):
        return 'calphad'
    if any(k in name for k in ('sim', 'structure', 'potential', 'hpc')):
        return 'simulation'
    return 'system'


def _build_commands() -> tuple[RegisteredCommand, ...]:
    """Build the slash command registry."""
    return (
        RegisteredCommand(name='help', description='Show available commands', execute=lambda: None),
        RegisteredCommand(name='status', description='Show session status', execute=lambda: None),
        RegisteredCommand(name='clear', description='Clear conversation history', execute=lambda: None),
        RegisteredCommand(name='compact', description='Compress conversation context', execute=lambda: None),
        RegisteredCommand(name='cost', description='Show token usage and cost', execute=lambda: None),
        RegisteredCommand(name='approve-all', description='Auto-approve all tool calls', execute=lambda: None),
        RegisteredCommand(name='model', description='Switch LLM model', execute=lambda: None),
        RegisteredCommand(name='session', description='List or load sessions', execute=lambda: None),
    )
