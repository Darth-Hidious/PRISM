"""AgentCore: the provider-agnostic TAOR loop."""
import json
from typing import Dict, Generator, List, Optional
from app.agent.backends.base import Backend
from app.agent.events import (
    AgentResponse, ToolCallEvent,
    TextDelta, ToolCallStart, ToolCallResult, TurnComplete,
)
from app.tools.base import ToolRegistry


DEFAULT_SYSTEM_PROMPT = """You are PRISM, an AI research assistant for materials science.

You have access to tools for searching materials databases (OPTIMADE, Materials Project),
predicting material properties, visualizing results, and exporting data to CSV.
Use these tools to help researchers find, analyze, and understand materials.

You also have higher-level skills that orchestrate multi-step workflows:
- acquire_materials: search and collect data from multiple sources
- predict_properties: predict material properties using ML models
- visualize_dataset: generate plots for dataset columns
- generate_report: compile a Markdown/PDF report
- select_materials: filter and rank candidates by criteria
- materials_discovery: end-to-end pipeline (acquire → predict → visualize → report)
- plan_simulations: generate simulation job plans for candidates

For complex requests, prefer using skills over individual tools.

When a user asks a question:
1. Think about what tools and data you need
2. Use the appropriate tools or skills to gather information
3. Synthesize the results into a clear answer

When you collect tabular data, consider using export_results_csv to save it for the user.

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

    def process_stream(self, message: str) -> Generator:
        """Process a user message through the TAOR loop, yielding stream events."""
        self.history.append({"role": "user", "content": message})
        tool_defs = self.tools.to_anthropic_format()

        for _iteration in range(self.max_iterations):
            for event in self.backend.complete_stream(messages=self.history, tools=tool_defs, system_prompt=self.system_prompt):
                if isinstance(event, (TextDelta, ToolCallStart)):
                    yield event
                # TurnComplete: check if we need tool execution
                if isinstance(event, TurnComplete):
                    break

            response = self.backend._last_stream_response
            if response is None:
                return

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
                    yield ToolCallResult(
                        call_id=tc.call_id,
                        tool_name=tc.tool_name,
                        result=result,
                        summary=self._summarize_tool_result(tc.tool_name, result),
                    )
            else:
                if response.text:
                    self.history.append({"role": "assistant", "content": response.text})
                yield TurnComplete(text=response.text, has_more=False)
                return

        yield TurnComplete(text=f"Reached max iterations ({self.max_iterations}). Stopping.", has_more=False)

    @staticmethod
    def _summarize_tool_result(tool_name: str, result: dict) -> str:
        """One-line summary of a tool result."""
        if "error" in result:
            return f"{tool_name}: error — {result['error'][:60]}"
        if "count" in result:
            return f"{tool_name}: {result['count']} results"
        if "results" in result and isinstance(result["results"], list):
            return f"{tool_name}: {len(result['results'])} results"
        if "filename" in result:
            return f"{tool_name}: saved to {result['filename']}"
        return f"{tool_name}: completed"

    def reset(self):
        """Clear conversation history."""
        self.history.clear()
