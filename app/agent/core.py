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

MAX_TOOL_RESULT_CHARS = 30_000


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

You can execute Python code for data analysis using the execute_python tool.
The user's full Python environment is available (pandas, numpy, matplotlib,
pymatgen, ASE, scikit-learn, pycalphad, etc.). Use this for data manipulation,
filtering, plotting, and custom calculations. Use print() to show output.
Use plt.savefig("filename.png") to save plots.

Be precise with scientific data. Cite sources when possible.

When a tool result is too large to fit in context, it will be stored and you'll receive
a preview + result_id. Use the peek_result tool to examine specific sections:
  peek_result(result_id="<id>", offset=0, limit=5000)
You can also use export_results_csv to save the full result to a file for the user."""


class AgentCore:
    """Provider-agnostic agent that runs a Think-Act-Observe-Repeat loop."""

    def __init__(self, backend: Backend, tools: ToolRegistry, system_prompt: Optional[str] = None,
                 max_iterations: int = 0, approval_callback: Optional[Callable] = None,
                 auto_approve: bool = True):
        from app.config.settings_schema import get_settings
        settings = get_settings()

        self.backend = backend
        self.tools = tools
        base_prompt = system_prompt if system_prompt is not None else DEFAULT_SYSTEM_PROMPT
        self.system_prompt = self._inject_capabilities(base_prompt)
        # Settings < explicit arg (0 means "use settings default")
        self.max_iterations = max_iterations if max_iterations > 0 else settings.agent.max_iterations
        self.history: List[Dict] = []
        self.approval_callback = approval_callback
        self.auto_approve = auto_approve
        self.scratchpad = None  # Set by caller (AgentREPL / autonomous) if desired
        self._total_usage = UsageInfo()
        self._result_store: dict = {}  # call_id -> full serialized result (RLM pattern)
        self._recent_calls: list = []

    @staticmethod
    def _inject_capabilities(prompt: str) -> str:
        """Append a live capability summary to the system prompt."""
        try:
            from app.tools.capabilities import capabilities_summary
            summary = capabilities_summary()
            if summary:
                return prompt + "\n\n--- AVAILABLE RESOURCES ---\n" + summary
        except Exception:
            pass
        return prompt

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

    def _check_doom_loop(self, tool_name: str, tool_args: dict, result) -> Optional[str]:
        """Detect if the agent is stuck calling the same failing tool repeatedly.
        Returns a warning message if same tool+args has failed 3 times, else None.
        """
        sig = (tool_name, json.dumps(tool_args, sort_keys=True))
        is_error = isinstance(result, dict) and "error" in result
        self._recent_calls.append((sig, is_error))
        if len(self._recent_calls) > 10:
            self._recent_calls.pop(0)

        # Check last 3 calls with same signature
        matching = [(s, err) for s, err in self._recent_calls if s == sig]
        if len(matching) >= 3 and all(err for _, err in matching[-3:]):
            return (
                f"DOOM LOOP DETECTED: {tool_name} has failed 3 times with the same arguments. "
                "Try a different approach, different arguments, or ask the user for help."
            )
        return None

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

    _PEEK_RESULT_TOOL_DEF = {
        "name": "peek_result",
        "description": "Examine a section of a stored large tool result. Use when a tool result was too large and was stored with a result_id.",
        "input_schema": {
            "type": "object",
            "properties": {
                "result_id": {"type": "string", "description": "The result_id from the stored result"},
                "offset": {"type": "integer", "description": "Character offset to start reading from (default 0)"},
                "limit": {"type": "integer", "description": "Number of characters to read (default 5000)"},
            },
            "required": ["result_id"],
        },
    }

    def _process_tool_result(self, result, call_id: str):
        """Process a tool result. If it exceeds MAX_TOOL_RESULT_CHARS, store it
        in the ResultStore and return a summary with peek_result instructions.
        Inspired by the RLM paradigm (Zhang et al., 2025).
        """
        try:
            serialized = json.dumps(result) if isinstance(result, dict) else json.dumps(str(result))
        except (TypeError, ValueError):
            serialized = str(result)

        if len(serialized) <= MAX_TOOL_RESULT_CHARS:
            return result if isinstance(result, dict) else {"value": result}

        # Store full result for peek access
        self._result_store[call_id] = serialized

        return {
            "_stored": True,
            "_result_id": call_id,
            "_total_size": len(serialized),
            "preview": serialized[:2000],
            "notice": (
                f"Result too large ({len(serialized):,} chars). Stored as '{call_id}'. "
                "Use the peek_result tool to examine specific sections: "
                f"peek_result(result_id='{call_id}', offset=0, limit=5000). "
                "You can also use export_results_csv to save the full result to a file."
            ),
        }

    def _peek_result(self, result_id: str, offset: int = 0, limit: int = 5000) -> dict:
        """Peek at a stored tool result. Returns a character slice."""
        if result_id not in self._result_store:
            return {"error": f"No stored result with id '{result_id}'"}
        data = self._result_store[result_id]
        chunk = data[offset:offset + limit]
        return {
            "chunk": chunk,
            "offset": offset,
            "limit": limit,
            "total_size": len(data),
            "has_more": offset + limit < len(data),
        }

    def _get_tool_defs(self) -> list:
        """Get tool definitions, including peek_result if there are stored results."""
        defs = self.tools.to_anthropic_format()
        if self._result_store:
            defs = list(defs) + [self._PEEK_RESULT_TOOL_DEF]
        return defs

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

        for _iteration in range(self.max_iterations):
            tool_defs = self._get_tool_defs()
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
                    # Handle internal peek_result tool
                    if tc.tool_name == "peek_result":
                        result = self._peek_result(**tc.tool_args)
                    else:
                        tool = self.tools.get(tc.tool_name)
                        if not self._should_approve(tool, tc):
                            result = {"skipped": f"Tool {tc.tool_name} was not approved by user."}
                        else:
                            try:
                                result = self._execute_tool(tool, tc.tool_args)
                            except Exception as e:
                                result = {"error": str(e)}
                    self._log_to_scratchpad(tc.tool_name, tc.tool_args, result)
                    self._post_tool_hook(tc.tool_name, result)
                    doom_msg = self._check_doom_loop(tc.tool_name, tc.tool_args, result)
                    processed = self._process_tool_result(result, call_id=tc.call_id)
                    self.history.append({"role": "tool_result", "tool_call_id": tc.call_id, "result": processed})
                    if doom_msg:
                        self.history.append({"role": "system", "content": doom_msg})
            else:
                if response.text:
                    self.history.append({"role": "assistant", "content": response.text})
                return response.text or ""

        return f"Reached max iterations ({self.max_iterations}). Stopping."

    def process_stream(self, message: str) -> Generator:
        """Process a user message through the TAOR loop, yielding stream events."""
        self.history.append({"role": "user", "content": message})

        for _iteration in range(self.max_iterations):
            tool_defs = self._get_tool_defs()
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
                    # Handle internal peek_result tool
                    if tc.tool_name == "peek_result":
                        result = self._peek_result(**tc.tool_args)
                    else:
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
                                doom_msg = self._check_doom_loop(tc.tool_name, tc.tool_args, result)
                                processed = self._process_tool_result(result, call_id=tc.call_id)
                                self.history.append({"role": "tool_result", "tool_call_id": tc.call_id, "result": processed})
                                if doom_msg:
                                    self.history.append({"role": "system", "content": doom_msg})
                                yield ToolCallResult(call_id=tc.call_id, tool_name=tc.tool_name, result=processed, summary=f"{tc.tool_name}: skipped (not approved)")
                                continue
                        try:
                            result = self._execute_tool(tool, tc.tool_args)
                        except Exception as e:
                            result = {"error": str(e)}
                    self._log_to_scratchpad(tc.tool_name, tc.tool_args, result)
                    self._post_tool_hook(tc.tool_name, result)
                    doom_msg = self._check_doom_loop(tc.tool_name, tc.tool_args, result)
                    processed = self._process_tool_result(result, call_id=tc.call_id)
                    self.history.append({"role": "tool_result", "tool_call_id": tc.call_id, "result": processed})
                    if doom_msg:
                        self.history.append({"role": "system", "content": doom_msg})
                    yield ToolCallResult(
                        call_id=tc.call_id,
                        tool_name=tc.tool_name,
                        result=processed,
                        summary=self._summarize_tool_result(tc.tool_name, processed),
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
        self._result_store.clear()
        self._recent_calls.clear()
