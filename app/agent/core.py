"""AgentCore: the provider-agnostic TAOR loop."""
import json
from typing import Callable, Dict, Generator, List, Optional
from app.agent.backends.base import Backend
from app.agent.events import (
    AgentResponse, ToolCallEvent, UsageInfo,
    TextDelta, ToolCallStart, ToolCallResult, TurnComplete,
    ToolApprovalRequest, ToolApprovalResponse,
)
from app.tools.base import ToolRegistry


DEFAULT_SYSTEM_PROMPT = """You are PRISM, an AI research assistant for materials science.

You have access to tools for searching materials databases (OPTIMADE, Materials Project, OMAT24),
predicting material properties, visualizing results, exporting data to CSV,
searching scientific literature (arXiv, Semantic Scholar), searching patents (Lens.org),
performing CALPHAD thermodynamic calculations (phase diagrams, equilibrium, Gibbs energy),
and validating data quality (outlier detection, constraint checking, completeness scoring).
Use these tools to help researchers find, analyze, and understand materials.

You also have higher-level skills that orchestrate multi-step workflows:
- acquire_materials: search and collect data from multiple sources
- predict_properties: predict material properties using ML models
- visualize_dataset: generate plots for dataset columns
- generate_report: compile a Markdown/HTML/PDF report with correlations and quality info
- select_materials: filter and rank candidates by criteria
- materials_discovery: end-to-end pipeline (acquire → predict → visualize → report)
- plan_simulations: generate simulation job plans (auto-routes CALPHAD vs DFT vs MD)
- analyze_phases: analyze phase stability using CALPHAD thermodynamic databases
- validate_dataset: detect outliers, check physical constraints, score completeness
- review_dataset: comprehensive data quality review with structured findings

For complex requests, prefer using skills over individual tools.

PLANNING: For complex multi-step goals, FIRST output a structured plan inside
<plan> and </plan> tags before executing any tools. The plan should list numbered
steps with the tools or skills you intend to use. The user will review the plan
before you proceed. For simple single-tool questions, skip planning and answer
directly.

When a user asks a question:
1. Think about what tools and data you need
2. For multi-step goals, output a <plan>...</plan> block first
3. Use the appropriate tools or skills to gather information
4. Synthesize the results into a clear answer

When you collect tabular data, consider using export_results_csv to save it for the user.

Be precise with scientific data. Cite sources when possible."""


class AgentCore:
    """Provider-agnostic agent that runs a Think-Act-Observe-Repeat loop."""

    def __init__(self, backend: Backend, tools: ToolRegistry, system_prompt: Optional[str] = None,
                 max_iterations: int = 20, approval_callback: Optional[Callable] = None,
                 auto_approve: bool = True):
        self.backend = backend
        self.tools = tools
        self.system_prompt = system_prompt if system_prompt is not None else DEFAULT_SYSTEM_PROMPT
        self.max_iterations = max_iterations
        self.history: List[Dict] = []
        self.approval_callback = approval_callback
        self.auto_approve = auto_approve
        self.scratchpad = None  # Set by caller (AgentREPL / autonomous) if desired
        self._total_usage = UsageInfo()

    def _execute_tool(self, tool, tool_args: dict) -> dict:
        """Execute a tool, injecting scratchpad for show_scratchpad."""
        if tool.name == "show_scratchpad":
            tool_args = dict(tool_args)
            tool_args["_scratchpad"] = self.scratchpad
        return tool.execute(**tool_args)

    def _should_approve(self, tool, tc) -> bool:
        """Check if a tool call should proceed (approval gate)."""
        if not tool.requires_approval:
            return True
        if self.auto_approve:
            return True
        if self.approval_callback:
            return self.approval_callback(tool.name, tc.tool_args)
        return True

    def _calculate_cost(self, usage: UsageInfo) -> float:
        """Calculate estimated cost in USD from usage and backend's model config."""
        config = getattr(self.backend, "model_config", None)
        if not config:
            return 0.0
        cost = (usage.input_tokens * config.input_price_per_mtok / 1_000_000
                + usage.output_tokens * config.output_price_per_mtok / 1_000_000)
        if usage.cache_read_tokens:
            cost += usage.cache_read_tokens * config.input_price_per_mtok * 0.1 / 1_000_000
        return cost

    def _log_to_scratchpad(self, tool_name: str, tool_args: dict, result: dict):
        """Log a tool execution to the scratchpad if available."""
        if self.scratchpad is not None:
            summary = self._summarize_tool_result(tool_name, result)
            self.scratchpad.log("tool_call", tool_name=tool_name, summary=summary, data={"args": tool_args, "result_keys": list(result.keys()) if isinstance(result, dict) else None})

    def _post_tool_hook(self, tool_name: str, result: dict):
        """Inject feedback into agent context after specific tools."""
        feedback_tools = {
            "validate_dataset", "review_dataset",
            "calculate_phase_diagram", "calculate_equilibrium", "analyze_phases",
        }
        if tool_name not in feedback_tools:
            return
        if not isinstance(result, dict) or "error" in result:
            return

        # Build a concise feedback summary
        if tool_name == "validate_dataset":
            summary = result.get("summary", "")
            msg = f"[Validation feedback] {summary}"
        elif tool_name == "review_dataset":
            prompt = result.get("review_prompt", "")
            score = result.get("quality_score", "")
            msg = f"[Review feedback] Quality score: {score}. {prompt[:300]}"
        else:
            # CALPHAD tools
            msg = f"[CALPHAD feedback] {tool_name} completed. "
            if "phases" in result:
                msg += f"Phases found: {result['phases']}. "
            if "summary" in result:
                msg += result["summary"][:300]

        self.history.append({"role": "system", "content": msg})

    def process(self, message: str) -> str:
        """Process a user message through the TAOR loop. Returns final text."""
        self.history.append({"role": "user", "content": message})
        tool_defs = self.tools.to_anthropic_format()

        for _iteration in range(self.max_iterations):
            response = self.backend.complete(messages=self.history, tools=tool_defs, system_prompt=self.system_prompt)
            if response.usage:
                self._total_usage = self._total_usage + response.usage

            if response.has_tool_calls:
                self.history.append({
                    "role": "tool_calls",
                    "text": response.text,
                    "calls": [{"id": tc.call_id, "name": tc.tool_name, "args": tc.tool_args} for tc in response.tool_calls],
                })
                for tc in response.tool_calls:
                    tool = self.tools.get(tc.tool_name)
                    if not self._should_approve(tool, tc):
                        result = {"skipped": f"Tool {tc.tool_name} was not approved by user."}
                    else:
                        try:
                            result = self._execute_tool(tool, tc.tool_args)
                        except Exception as e:
                            result = {"error": str(e)}
                    self.history.append({"role": "tool_result", "tool_call_id": tc.call_id, "result": result})
                    self._log_to_scratchpad(tc.tool_name, tc.tool_args, result)
                    self._post_tool_hook(tc.tool_name, result)
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
            if response.usage:
                self._total_usage = self._total_usage + response.usage

            if response.has_tool_calls:
                self.history.append({
                    "role": "tool_calls",
                    "text": response.text,
                    "calls": [{"id": tc.call_id, "name": tc.tool_name, "args": tc.tool_args} for tc in response.tool_calls],
                })
                for tc in response.tool_calls:
                    tool = self.tools.get(tc.tool_name)
                    # Approval gate
                    if tool.requires_approval and not self.auto_approve:
                        yield ToolApprovalRequest(tool_name=tc.tool_name, tool_args=tc.tool_args, call_id=tc.call_id)
                        if self.approval_callback:
                            approved = self.approval_callback(tc.tool_name, tc.tool_args)
                        else:
                            approved = False
                        if not approved:
                            result = {"skipped": f"Tool {tc.tool_name} was not approved by user."}
                            self.history.append({"role": "tool_result", "tool_call_id": tc.call_id, "result": result})
                            yield ToolCallResult(call_id=tc.call_id, tool_name=tc.tool_name, result=result, summary=f"{tc.tool_name}: skipped (not approved)")
                            continue
                    try:
                        result = self._execute_tool(tool, tc.tool_args)
                    except Exception as e:
                        result = {"error": str(e)}
                    self.history.append({"role": "tool_result", "tool_call_id": tc.call_id, "result": result})
                    self._log_to_scratchpad(tc.tool_name, tc.tool_args, result)
                    self._post_tool_hook(tc.tool_name, result)
                    yield ToolCallResult(
                        call_id=tc.call_id,
                        tool_name=tc.tool_name,
                        result=result,
                        summary=self._summarize_tool_result(tc.tool_name, result),
                    )
            else:
                if response.text:
                    self.history.append({"role": "assistant", "content": response.text})
                yield TurnComplete(
                    text=response.text, has_more=False,
                    usage=response.usage if response else None,
                    total_usage=self._total_usage,
                    estimated_cost=self._calculate_cost(self._total_usage),
                )
                return

        yield TurnComplete(
            text=f"Reached max iterations ({self.max_iterations}). Stopping.",
            has_more=False,
            total_usage=self._total_usage,
            estimated_cost=self._calculate_cost(self._total_usage),
        )

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
        """Clear conversation history and usage tracking."""
        self.history.clear()
        self._total_usage = UsageInfo()
