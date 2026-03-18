"""Autonomous mode: run agent to completion on a goal."""
from typing import Generator, Optional
from app.agent.backends.base import Backend
from app.agent.core import AgentCore
from app.agent.events import TextDelta, ToolCallStart, ToolCallResult, TurnComplete
from app.agent.prompts import AUTONOMOUS_SYSTEM_PROMPT
from app.agent.scratchpad import Scratchpad
from app.tools.base import ToolRegistry


def _make_tools(tools: Optional[ToolRegistry] = None, enable_mcp: bool = True) -> ToolRegistry:
    if tools is not None:
        return tools
    from app.plugins.bootstrap import build_full_registry
    tool_reg, _provider_reg, _agent_reg = build_full_registry(enable_mcp=enable_mcp)
    return tool_reg


def run_autonomous(goal: str, backend: Backend, system_prompt: Optional[str] = None,
                   tools: Optional[ToolRegistry] = None, max_iterations: int = 30,
                   enable_mcp: bool = True, confirm: bool = False) -> str:
    tools = _make_tools(tools, enable_mcp=enable_mcp)
    agent = AgentCore(
        backend=backend, tools=tools,
        system_prompt=system_prompt or AUTONOMOUS_SYSTEM_PROMPT,
        max_iterations=max_iterations,
        auto_approve=not confirm,
    )
    agent.scratchpad = Scratchpad()
    return agent.process(goal)


def run_autonomous_stream(goal: str, backend: Backend, system_prompt: Optional[str] = None,
                          tools: Optional[ToolRegistry] = None, max_iterations: int = 30,
                          enable_mcp: bool = True, confirm: bool = False) -> Generator:
    """Run agent autonomously on a goal, yielding stream events."""
    tools = _make_tools(tools, enable_mcp=enable_mcp)
    agent = AgentCore(
        backend=backend, tools=tools,
        system_prompt=system_prompt or AUTONOMOUS_SYSTEM_PROMPT,
        max_iterations=max_iterations,
        auto_approve=not confirm,
    )
    agent.scratchpad = Scratchpad()
    yield from agent.process_stream(goal)
