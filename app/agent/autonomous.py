"""Autonomous mode: run agent to completion on a goal."""
from typing import Generator, Optional
from app.agent.backends.base import Backend
from app.agent.core import AgentCore
from app.agent.events import TextDelta, ToolCallStart, ToolCallResult, TurnComplete
from app.tools.base import ToolRegistry
from app.tools.data import create_data_tools
from app.tools.system import create_system_tools
from app.tools.visualization import create_visualization_tools
from app.tools.prediction import create_prediction_tools
from app.simulation.bridge import check_pyiron_available


AUTONOMOUS_SYSTEM_PROMPT = """You are PRISM, an autonomous materials science research agent.

You have been given a research goal. Use your tools to investigate, gather data,
analyze results, and produce a comprehensive answer.

Available tool categories:
- Data: Search OPTIMADE databases, query Materials Project, export results to CSV
- Visualization: Create plots and comparisons
- System: Read/write files, search the web

Work step by step:
1. Break down the research goal
2. Use tools to gather relevant data
3. Analyze and synthesize findings
4. Present a clear, well-structured answer with citations

When you collect tabular data, consider using export_results_csv to save it for the user.

Be thorough but efficient. Cite data sources."""


def _make_tools(tools: Optional[ToolRegistry] = None, enable_mcp: bool = True) -> ToolRegistry:
    if tools is not None:
        return tools
    registry = ToolRegistry()
    create_system_tools(registry)
    create_data_tools(registry)
    create_visualization_tools(registry)
    create_prediction_tools(registry)
    if check_pyiron_available():
        from app.tools.simulation import create_simulation_tools
        create_simulation_tools(registry)
    if enable_mcp:
        try:
            from app.mcp_client import discover_and_register_mcp_tools
            discover_and_register_mcp_tools(registry)
        except Exception:
            pass
    return registry


def run_autonomous(goal: str, backend: Backend, system_prompt: Optional[str] = None,
                   tools: Optional[ToolRegistry] = None, max_iterations: int = 30,
                   enable_mcp: bool = True) -> str:
    tools = _make_tools(tools, enable_mcp=enable_mcp)
    agent = AgentCore(backend=backend, tools=tools, system_prompt=system_prompt or AUTONOMOUS_SYSTEM_PROMPT, max_iterations=max_iterations)
    return agent.process(goal)


def run_autonomous_stream(goal: str, backend: Backend, system_prompt: Optional[str] = None,
                          tools: Optional[ToolRegistry] = None, max_iterations: int = 30,
                          enable_mcp: bool = True) -> Generator:
    """Run agent autonomously on a goal, yielding stream events."""
    tools = _make_tools(tools, enable_mcp=enable_mcp)
    agent = AgentCore(backend=backend, tools=tools, system_prompt=system_prompt or AUTONOMOUS_SYSTEM_PROMPT, max_iterations=max_iterations)
    yield from agent.process_stream(goal)
