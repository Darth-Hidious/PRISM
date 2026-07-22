"""Tool base class and registry for provider-agnostic tool definitions.

Tool.execute is the single integration point for cross-cutting concerns
(currently: artifact recording for the stateful memory subsystem). Every
caller — `app/tool_server.py`, `app/mcp_server.py`, future callers — flows
through this method, so attaching behavior here is the only place that
catches every path. Earlier designs that monkey-patched at bootstrap broke
the MCP path because mcp_server captures `tool.execute` as a bound method
at registration time; bound methods cache `(func, instance)` and don't
see later class-level patches.
"""

import logging
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional

logger = logging.getLogger(__name__)


@dataclass
class Tool:
    """A single tool that can be called by the agent.

    Scientific-tool authoring contract (see docs/PRISM_TOOL_SURFACE_AUDIT.md §6):
    the optional ``output_schema`` / ``units`` / ``examples`` / ``validate``
    fields let a first-class scientific tool (PRISM Alpha → QE → pyiron chain)
    declare typed outputs, carry units in a machine-readable map, ship 1-2
    input→output exemplars (kills the arg-fill-error class), and gate on
    *scientific* validity (exit 0 ≠ converged). All four default to off so
    every existing tool is unaffected; new scientific tools SHOULD set them.
    """

    name: str
    description: str
    input_schema: dict
    func: Callable
    requires_approval: bool = False
    source: str = "builtin"
    source_detail: Optional[str] = None
    # Memory subsystem opt-out. Tools that ARE the memory subsystem
    # (search_artifacts, fetch_artifact, list_artifacts, show_scratchpad) MUST set
    # this False to avoid pointless self-indexing and infinite recursion.
    record_artifacts: bool = True
    # --- Scientific-tool authoring contract (all optional, audit TASK 0 F3) ---
    # Typed OUTPUT schema so playbook step-binding ($x) is unit-safe and claims
    # are machine-checkable (backlog A1). Mirrors input_schema shape.
    output_schema: Optional[dict] = None
    # EMMO/QUDT-aligned unit tags keyed by output field, e.g.
    # {"final_energy_eV": "EMMO:eV", "fmax": "EMMO:eV-per-angstrom"}. Tools that
    # already bake units into field names (the MACE surface: fmax_eV_per_A, T_K)
    # satisfy the spirit without this map; it is for fields whose names can't
    # carry the unit unambiguously.
    units: Optional[Dict[str, str]] = None
    # 1-2 real input→output exemplars. Anthropic "Tool Use Examples" shows this
    # measurably improves arg-filling for high-arity scientific tools (backlog
    # A3). Directly prevents the materials_search flat-vs-nested mis-fill class.
    examples: Optional[List[dict]] = None
    # Scientific validity gate: validate(output) -> "ok"|"warn"|"invalid".
    # Exit 0 is not enough (an unconverged SCF, a NaN tensor, a max-steps hit
    # all exit 0). When set, execute() attaches the verdict to the result so a
    # "successful-but-garbage" output is flagged before it feeds the next
    # playbook step or a claim (backlog D2).
    validate: Optional[Callable[[dict], str]] = None

    def execute(self, **kwargs) -> dict:
        """Execute the tool with given arguments.

        UNIFORM ERROR CONTRACT: tool functions must never leak exceptions
        to the model — the executors surface raw tracebacks,
        which the model can't act on. Any uncaught exception is converted
        here to an ``{"error": ...}`` dict (full traceback goes to the log,
        not to the model). Both executors (tool_server, mcp_server) call
        ``tool.execute``, so this is the single choke point.

        After the underlying function returns, hand the result to the
        memory recorder. If memory is disabled or the tool opts out, the
        result passes through unchanged. Recording failures are
        logged-and-swallowed inside the recorder — they never break tool
        execution.

        SCIENTIFIC VALIDITY GATE: if ``self.validate`` is set, the verdict
        (ok/warn/invalid) is attached to a dict result as
        ``scientific_validity``. An ``invalid`` verdict does NOT mask the
        output (the agent needs to see what went wrong) — it flags it so the
        agent and downstream playbook steps can branch on it rather than
        silently propagating a wrong number into an aerospace decision.
        """
        try:
            result = self.func(**kwargs)
        except TypeError as e:
            logger.exception("tool '%s' raised TypeError", self.name)
            out = {"error": f"TypeError: {e}", "tool": self.name}
            if "argument" in str(e):
                # Almost always the model passing args that don't match
                # the schema (unexpected / missing keyword). Say so.
                out["hint"] = (
                    "arguments did not match the tool's input_schema — "
                    "check parameter names and retry"
                )
            return out
        except Exception as e:
            logger.exception("tool '%s' failed", self.name)
            return {"error": f"{type(e).__name__}: {e}", "tool": self.name}
        # Attach the scientific-validity verdict when a gate is defined. Only
        # applies to dict results (the structured shape a gate can inspect); a
        # non-dict result passes through untouched.
        if self.validate is not None and isinstance(result, dict) and "error" not in result:
            try:
                verdict = self.validate(result)
            except Exception as e:  # a buggy gate must never break the tool
                logger.warning("tool '%s' validate() raised: %s", self.name, e)
                verdict = "warn"
            if verdict in ("ok", "warn", "invalid"):
                result["scientific_validity"] = verdict
            else:
                # A gate that returns something outside the contract is itself
                # defective — silently dropping it would hide the bug. Degrade
                # to "warn" (so the agent still gets a verdict) AND log loudly so
                # the gate author can fix it.
                logger.warning(
                    "tool '%s' validate() returned %r — must be one of "
                    "ok/warn/invalid; defaulting to 'warn'",
                    self.name,
                    verdict,
                )
                result["scientific_validity"] = "warn"
        if not self.record_artifacts:
            return result
        # Lazy import to avoid a circular dependency at module load. The
        # recorder is purely additive — if memory hasn't been configured,
        # it returns the original result unchanged.
        try:
            from app.tools.memory.recorder import record_if_enabled
        except ImportError:
            return result
        return record_if_enabled(tool_name=self.name, args=kwargs, result=result)


class ToolRegistry:
    """Registry of available tools, with format conversion for each backend."""

    def __init__(self):
        self._tools: Dict[str, Tool] = {}

    def register(self, tool: Tool) -> None:
        """Register a tool. Logs a warning if the name is already taken."""
        if tool.name in self._tools:
            logger.warning(
                "tool '%s' re-registered (overwrites %s from %s)",
                tool.name,
                self._tools[tool.name].source,
                self._tools[tool.name].source_detail or "unknown",
            )
        self._tools[tool.name] = tool

    def get(self, name: str) -> Tool:
        """Get a tool by name. Raises KeyError if not found."""
        return self._tools[name]

    def list_tools(self) -> List[Tool]:
        """Return all registered tools."""
        return list(self._tools.values())

    def to_anthropic_format(self) -> List[dict]:
        """Convert tools to Anthropic API format."""
        return [
            {
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            }
            for t in self._tools.values()
        ]

    def to_openai_format(self) -> List[dict]:
        """Convert tools to OpenAI API format."""
        return [
            {
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                },
            }
            for t in self._tools.values()
        ]
