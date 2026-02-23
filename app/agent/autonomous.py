"""Autonomous mode: run agent to completion on a goal."""
from typing import Optional
from app.agent.backends.base import Backend
from app.agent.core import AgentCore
from app.tools.base import ToolRegistry
from app.tools.data import create_data_tools
from app.tools.system import create_system_tools
from app.tools.visualization import create_visualization_tools


AUTONOMOUS_SYSTEM_PROMPT = """You are PRISM, an autonomous materials science research agent.

You have been given a research goal. Use your tools to investigate, gather data,
analyze results, and produce a comprehensive answer.

Available tool categories:
- Data: Search OPTIMADE databases, query Materials Project
- Visualization: Create plots and comparisons
- System: Read/write files, search the web

Work step by step:
1. Break down the research goal
2. Use tools to gather relevant data
3. Analyze and synthesize findings
4. Present a clear, well-structured answer with citations

Be thorough but efficient. Cite data sources."""


def run_autonomous(goal: str, backend: Backend, system_prompt: Optional[str] = None,
                   tools: Optional[ToolRegistry] = None, max_iterations: int = 30) -> str:
    if tools is None:
        tools = ToolRegistry()
        create_system_tools(tools)
        create_data_tools(tools)
        create_visualization_tools(tools)
    agent = AgentCore(backend=backend, tools=tools, system_prompt=system_prompt or AUTONOMOUS_SYSTEM_PROMPT, max_iterations=max_iterations)
    return agent.process(goal)
