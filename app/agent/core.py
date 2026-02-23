"""AgentCore: the provider-agnostic TAOR loop."""
from typing import Dict, List, Optional
from app.agent.backends.base import Backend
from app.agent.events import AgentResponse, ToolCallEvent
from app.tools.base import ToolRegistry


DEFAULT_SYSTEM_PROMPT = """You are PRISM, an AI research assistant for materials science.

You have access to tools for searching materials databases (OPTIMADE, Materials Project),
predicting material properties, and visualizing results. Use these tools to help
researchers find, analyze, and understand materials.

When a user asks a question:
1. Think about what tools and data you need
2. Use the appropriate tools to gather information
3. Synthesize the results into a clear answer

Be precise with scientific data. Cite sources when possible."""


class AgentCore:
    """Provider-agnostic agent that runs a Think-Act-Observe-Repeat loop."""

    def __init__(self, backend: Backend, tools: ToolRegistry, system_prompt: Optional[str] = None, max_iterations: int = 20):
        self.backend = backend
        self.tools = tools
        self.system_prompt = system_prompt if system_prompt is not None else DEFAULT_SYSTEM_PROMPT
        self.max_iterations = max_iterations
        self.history: List[Dict] = []

    def process(self, message: str) -> str:
        """Process a user message through the TAOR loop. Returns final text."""
        self.history.append({"role": "user", "content": message})
        tool_defs = self.tools.to_anthropic_format()

        for _iteration in range(self.max_iterations):
            response = self.backend.complete(messages=self.history, tools=tool_defs, system_prompt=self.system_prompt)

            if response.has_tool_calls:
                self.history.append({
                    "role": "tool_calls",
                    "text": response.text,
                    "calls": [{"id": tc.call_id, "name": tc.tool_name, "args": tc.tool_args} for tc in response.tool_calls],
                })
                for tc in response.tool_calls:
                    tool = self.tools.get(tc.tool_name)
                    try:
                        result = tool.execute(**tc.tool_args)
                    except Exception as e:
                        result = {"error": str(e)}
                    self.history.append({"role": "tool_result", "tool_call_id": tc.call_id, "result": result})
            else:
                if response.text:
                    self.history.append({"role": "assistant", "content": response.text})
                return response.text or ""

        return f"Reached max iterations ({self.max_iterations}). Stopping."

    def reset(self):
        """Clear conversation history."""
        self.history.clear()
