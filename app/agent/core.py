"""AgentCore: the provider-agnostic TAOR loop.

Integrates:
- Execution registry for frozen tool dispatch
- Hook system (pre/post tool use) following the PreToolUse/PostToolUse pattern
- Permission context (immutable deny-list)
- Transcript with rolling-window compaction
- Cost tracking (MARC27 API when available, local fallback)
"""
import json
import time
from pathlib import Path
from typing import Callable, Dict, Generator, List, Optional
from app.agent.backends.base import Backend
from app.agent.events import (
    AgentResponse, ToolCallEvent, UsageInfo,
    TextDelta, ToolCallStart, ToolCallResult, TurnComplete,
    ToolApprovalRequest, ToolApprovalResponse,
)
from app.agent.hooks import HookRegistry, build_default_hooks
from app.agent.permissions import ToolPermissionContext
from app.agent.transcript import TranscriptStore, TranscriptEntry, TurnBudget, CostTracker
from app.agent.prompts import INTERACTIVE_SYSTEM_PROMPT
from app.tools.base import ToolRegistry

MAX_TOOL_RESULT_CHARS = 30_000


def _validate_tool_input(tool, args: dict) -> str | None:
    """Validate tool args against the tool's input_schema. Returns error string or None."""
    schema = getattr(tool, 'input_schema', None)
    if not schema or not isinstance(schema, dict):
        return None
    props = schema.get('properties', {})
    required = schema.get('required', [])
    for field in required:
        if field not in args:
            return f"missing required field '{field}'"
    for key, val in args.items():
        if key.startswith('_'):
            continue  # internal fields
        if key in props:
            expected_type = props[key].get('type', '')
            if expected_type == 'string' and not isinstance(val, str):
                return f"'{key}' must be a string, got {type(val).__name__}"
            if expected_type == 'integer' and not isinstance(val, int):
                return f"'{key}' must be an integer, got {type(val).__name__}"
            if expected_type == 'number' and not isinstance(val, (int, float)):
                return f"'{key}' must be a number, got {type(val).__name__}"
            if expected_type == 'array' and not isinstance(val, list):
                return f"'{key}' must be an array, got {type(val).__name__}"
    return None


def _load_system_prompt(settings_path: str = "") -> str:
    if settings_path:
        p = Path(settings_path).expanduser()
        if p.exists():
            return p.read_text().strip()
    return INTERACTIVE_SYSTEM_PROMPT


class AgentCore:
    """Provider-agnostic agent that runs a Think-Act-Observe-Repeat loop.

    The turn loop follows this pattern (matching the ConversationRuntime pattern):
    1. Push user message to transcript
    2. Loop:
       a. Stream/call LLM
       b. For each tool call:
          - Run pre-tool hooks → can abort/modify input
          - Check permissions (deny-list + approval gate)
          - Execute tool
          - Run post-tool hooks → can modify output
          - Record cost event
          - Push result to transcript
       c. If no tool calls → break
    3. Auto-compact transcript if threshold exceeded
    4. Return TurnComplete with usage/cost
    """

    def __init__(self, backend: Backend, tools: ToolRegistry, system_prompt: Optional[str] = None,
                 max_iterations: int = 0, approval_callback: Optional[Callable] = None,
                 auto_approve: bool = True):
        from app.config.settings_schema import get_settings
        settings = get_settings()

        self.backend = backend
        self.tools = tools
        base_prompt = system_prompt if system_prompt is not None else _load_system_prompt(
            settings.agent.system_prompt_file
        )
        self.system_prompt = self._inject_capabilities(base_prompt)
        self.max_iterations = max_iterations if max_iterations > 0 else settings.agent.max_iterations
        self.history: List[Dict] = []
        self.approval_callback = approval_callback
        self.auto_approve = auto_approve
        self.scratchpad = None

        # Usage tracking
        self._total_usage = UsageInfo()
        self._result_store: dict = {}
        self._recent_calls: list = []

        # New in v2: hooks, permissions, transcript, cost
        from app.agent.permissions import PermissionMode
        self.hooks = build_default_hooks()
        self.permissions = ToolPermissionContext.default()
        self.permission_mode = PermissionMode.WORKSPACE_WRITE
        if auto_approve:
            self.permissions = ToolPermissionContext.accept_all()
            self.permission_mode = PermissionMode.FULL_ACCESS
        self.transcript = TranscriptStore(budget=TurnBudget(
            max_turns=self.max_iterations,
            compact_after_turns=max(self.max_iterations - 5, 15),
        ))
        self.cost = CostTracker()

    @staticmethod
    def _inject_capabilities(prompt: str) -> str:
        try:
            from app.tools.capabilities import capabilities_summary
            summary = capabilities_summary()
            if summary:
                return prompt + "\n\n--- AVAILABLE RESOURCES ---\n" + summary
        except Exception:
            pass
        return prompt

    # ── Tool execution with hooks ───────────────────────────────────

    def _execute_tool_with_hooks(self, tool_name: str, tool_args: dict, call_id: str) -> dict:
        """Execute a tool through the full hook + permission pipeline.

        Flow (matches Rust ConversationRuntime.run_turn):
        1. Pre-hook → can abort, modify input, override permission
        2. Permission check → deny-list + approval gate
        3. Execute tool
        4. Post-hook → can modify output
        5. Record cost + log
        """
        t0 = time.monotonic()

        # Step 1: Pre-tool hooks
        pre_result = self.hooks.fire_before(tool_name, tool_args)
        if pre_result.abort:
            return {"error": f"Blocked by hook: {pre_result.reason}", "_hook_blocked": True}

        # Apply modified inputs from hook
        effective_args = tool_args.copy()
        if pre_result.modified_inputs:
            effective_args.update(pre_result.modified_inputs)

        # Step 2: Permission check
        if self.permissions.blocks(tool_name):
            return {"error": f"Tool '{tool_name}' is blocked by permission policy.", "_permission_denied": True}

        # Step 3: Approval gate (for tools that need user confirmation)
        tool = self.tools.get(tool_name)
        if tool is None:
            return {"error": f"Unknown tool: {tool_name}"}

        if tool.requires_approval and not self.permissions.auto_approves(tool_name):
            if not self.auto_approve:
                if self.approval_callback:
                    approved = self.approval_callback(tool_name, effective_args)
                    if not approved:
                        return {"skipped": f"Tool {tool_name} was not approved by user."}
                else:
                    return {"skipped": f"Tool {tool_name} requires approval but no callback set."}

        # Step 3.5: Validate inputs against schema
        validation_error = _validate_tool_input(tool, effective_args)
        if validation_error:
            return {"error": f"Invalid input for {tool_name}: {validation_error}"}

        # Step 4: Execute
        try:
            if tool_name == "show_scratchpad":
                effective_args["_scratchpad"] = self.scratchpad
            result = tool.execute(**effective_args)
        except Exception as e:
            result = {"error": str(e)}

        elapsed_ms = (time.monotonic() - t0) * 1000
        is_error = isinstance(result, dict) and "error" in result

        # Step 5: Post-tool hooks
        result = self.hooks.fire_after(tool_name, effective_args, result, elapsed_ms)

        # Step 6: Cost event
        self.cost.record(f"tool:{tool_name}", 0, 0)  # tool calls don't have token cost

        # Step 7: Transcript entry
        self.transcript.append(TranscriptEntry(
            role='tool',
            content=self._summarize_tool_result(tool_name, result),
            tool_name=tool_name,
        ))

        # Step 8: Scratchpad + doom loop
        self._log_to_scratchpad(tool_name, effective_args, result)
        self._post_tool_hook(tool_name, result)

        return result

    # ── Internal peek_result tool ───────────────────────────────────

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

    def _peek_result(self, result_id: str, offset: int = 0, limit: int = 5000) -> dict:
        if result_id not in self._result_store:
            return {"error": f"No stored result with id '{result_id}'"}
        data = self._result_store[result_id]
        chunk = data[offset:offset + limit]
        return {"chunk": chunk, "offset": offset, "limit": limit,
                "total_size": len(data), "has_more": offset + limit < len(data)}

    def _process_tool_result(self, result, call_id: str):
        try:
            serialized = json.dumps(result) if isinstance(result, dict) else json.dumps(str(result))
        except (TypeError, ValueError):
            serialized = str(result)
        if len(serialized) <= MAX_TOOL_RESULT_CHARS:
            return result if isinstance(result, dict) else {"value": result}
        self._result_store[call_id] = serialized
        return {
            "_stored": True, "_result_id": call_id, "_total_size": len(serialized),
            "preview": serialized[:2000],
            "notice": (f"Result too large ({len(serialized):,} chars). Stored as '{call_id}'. "
                       f"Use peek_result(result_id='{call_id}', offset=0, limit=5000) to examine."),
        }

    def _get_tool_defs(self) -> list:
        defs = self.tools.to_anthropic_format()
        if self._result_store:
            defs = list(defs) + [self._PEEK_RESULT_TOOL_DEF]
        return defs

    # ── Logging and feedback ────────────────────────────────────────

    def _log_to_scratchpad(self, tool_name: str, tool_args: dict, result: dict):
        if self.scratchpad is not None:
            summary = self._summarize_tool_result(tool_name, result)
            self.scratchpad.log("tool_call", tool_name=tool_name, summary=summary,
                               data={"args": tool_args, "result_keys": list(result.keys()) if isinstance(result, dict) else None})

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
        if tool_name == "validate_dataset":
            msg = f"[Validation feedback] {result.get('summary', '')}"
        elif tool_name == "review_dataset":
            msg = f"[Review feedback] Quality score: {result.get('quality_score', '')}. {result.get('review_prompt', '')[:300]}"
        else:
            msg = f"[CALPHAD feedback] {tool_name} completed. "
            if "phases" in result:
                msg += f"Phases found: {result['phases']}. "
            if "summary" in result:
                msg += result["summary"][:300]
        self.history.append({"role": "system", "content": msg})

    def _check_doom_loop(self, tool_name: str, tool_args: dict, result) -> Optional[str]:
        sig = (tool_name, json.dumps(tool_args, sort_keys=True))
        is_error = isinstance(result, dict) and "error" in result
        self._recent_calls.append((sig, is_error))
        if len(self._recent_calls) > 10:
            self._recent_calls.pop(0)
        matching = [(s, err) for s, err in self._recent_calls if s == sig]
        if len(matching) >= 3 and all(err for _, err in matching[-3:]):
            return (f"DOOM LOOP DETECTED: {tool_name} has failed 3 times with the same arguments. "
                    "Try a different approach, different arguments, or ask the user for help.")
        return None

    def _calculate_cost(self, usage: UsageInfo) -> float:
        """Calculate estimated cost. Uses MARC27 API usage when available,
        falls back to local model pricing."""
        # Check if MARC27 platform tracks our usage (authoritative source)
        config = getattr(self.backend, "model_config", None)
        if not config:
            return 0.0
        cost = (usage.input_tokens * config.input_price_per_mtok / 1_000_000
                + usage.output_tokens * config.output_price_per_mtok / 1_000_000)
        if usage.cache_read_tokens:
            cost += usage.cache_read_tokens * config.input_price_per_mtok * 0.1 / 1_000_000
        return cost

    def _should_approve(self, tool, tc) -> bool:
        if not tool.requires_approval:
            return True
        if self.auto_approve or self.permissions.auto_approves(tool.name):
            return True
        if self.approval_callback:
            return self.approval_callback(tool.name, tc.tool_args)
        return True

    def _maybe_compact(self):
        """Auto-compact transcript if threshold exceeded."""
        if self.transcript.should_compact():
            summary = self.transcript.compact(keep_last=6)
            if summary:
                self.history.append({"role": "system",
                                     "content": f"[Context compacted] {summary}"})

    # ── Main loops ──────────────────────────────────────────────────

    def process(self, message: str) -> str:
        """Process a user message through the TAOR loop. Returns final text."""
        self.history.append({"role": "user", "content": message})
        self.transcript.append(TranscriptEntry(role='user', content=message))

        for _iteration in range(self.max_iterations):
            tool_defs = self._get_tool_defs()
            response = self.backend.complete(messages=self.history, tools=tool_defs, system_prompt=self.system_prompt)
            if response.usage:
                self._total_usage = self._total_usage + response.usage
                self.cost.record("llm_turn", response.usage.input_tokens, response.usage.output_tokens)

            if response.has_tool_calls:
                self.history.append({
                    "role": "tool_calls", "text": response.text,
                    "calls": [{"id": tc.call_id, "name": tc.tool_name, "args": tc.tool_args} for tc in response.tool_calls],
                })
                for tc in response.tool_calls:
                    if tc.tool_name == "peek_result":
                        result = self._peek_result(**tc.tool_args)
                    else:
                        result = self._execute_tool_with_hooks(tc.tool_name, tc.tool_args, tc.call_id)

                    doom_msg = self._check_doom_loop(tc.tool_name, tc.tool_args, result)
                    processed = self._process_tool_result(result, call_id=tc.call_id)
                    self.history.append({"role": "tool_result", "tool_call_id": tc.call_id, "result": processed})
                    if doom_msg:
                        self.history.append({"role": "system", "content": doom_msg})
            else:
                if response.text:
                    self.history.append({"role": "assistant", "content": response.text})
                    self.transcript.append(TranscriptEntry(role='assistant', content=response.text))
                self._maybe_compact()
                return response.text or ""

        return f"Reached max iterations ({self.max_iterations}). Stopping."

    def process_stream(self, message: str) -> Generator:
        """Process a user message through the TAOR loop, yielding stream events."""
        self.history.append({"role": "user", "content": message})
        self.transcript.append(TranscriptEntry(role='user', content=message))

        for _iteration in range(self.max_iterations):
            # Check budget
            warning = self.transcript.budget_warning()
            if warning:
                yield TextDelta(text=f"\n[{warning}]\n")
            if self.transcript.budget_exhausted():
                yield TurnComplete(
                    text="Turn budget exhausted.", has_more=False,
                    total_usage=self._total_usage,
                    estimated_cost=self._calculate_cost(self._total_usage),
                )
                return

            tool_defs = self._get_tool_defs()
            for event in self.backend.complete_stream(messages=self.history, tools=tool_defs, system_prompt=self.system_prompt):
                if isinstance(event, (TextDelta, ToolCallStart)):
                    yield event
                if isinstance(event, TurnComplete):
                    break

            response = self.backend._last_stream_response
            if response is None:
                return
            if response.usage:
                self._total_usage = self._total_usage + response.usage
                self.cost.record("llm_turn", response.usage.input_tokens, response.usage.output_tokens)

            if response.has_tool_calls:
                self.history.append({
                    "role": "tool_calls", "text": response.text,
                    "calls": [{"id": tc.call_id, "name": tc.tool_name, "args": tc.tool_args} for tc in response.tool_calls],
                })
                for tc in response.tool_calls:
                    if tc.tool_name == "peek_result":
                        result = self._peek_result(**tc.tool_args)
                    else:
                        # Approval gate (yields event for frontend)
                        tool = self.tools.get(tc.tool_name)
                        if (tool and tool.requires_approval
                                and not self.auto_approve
                                and not self.permissions.auto_approves(tc.tool_name)):
                            yield ToolApprovalRequest(tool_name=tc.tool_name, tool_args=tc.tool_args, call_id=tc.call_id)
                            if self.approval_callback:
                                approved = self.approval_callback(tc.tool_name, tc.tool_args)
                            else:
                                approved = False
                            if not approved:
                                result = {"skipped": f"Tool {tc.tool_name} was not approved by user."}
                                processed = self._process_tool_result(result, call_id=tc.call_id)
                                self.history.append({"role": "tool_result", "tool_call_id": tc.call_id, "result": processed})
                                yield ToolCallResult(call_id=tc.call_id, tool_name=tc.tool_name, result=processed,
                                                     summary=f"{tc.tool_name}: skipped (not approved)")
                                continue

                        result = self._execute_tool_with_hooks(tc.tool_name, tc.tool_args, tc.call_id)

                    doom_msg = self._check_doom_loop(tc.tool_name, tc.tool_args, result)
                    processed = self._process_tool_result(result, call_id=tc.call_id)
                    self.history.append({"role": "tool_result", "tool_call_id": tc.call_id, "result": processed})
                    if doom_msg:
                        self.history.append({"role": "system", "content": doom_msg})
                    yield ToolCallResult(
                        call_id=tc.call_id, tool_name=tc.tool_name, result=processed,
                        summary=self._summarize_tool_result(tc.tool_name, processed),
                    )
            else:
                if response.text:
                    self.history.append({"role": "assistant", "content": response.text})
                    self.transcript.append(TranscriptEntry(role='assistant', content=response.text))

                self._maybe_compact()

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
        self.history.clear()
        self._total_usage = UsageInfo()
        self._result_store.clear()
        self._recent_calls.clear()
        self.transcript = TranscriptStore(budget=self.transcript.budget)
        self.cost = CostTracker()
