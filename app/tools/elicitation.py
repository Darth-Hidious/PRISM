"""Informed-autonomy elicitation gate.

PRISM is an *informed*-autonomy harness, not a fire-and-forget one. Before
any simulation/heavy-compute tool runs, an informed human must have
confirmed the four inputs that make the run scientifically meaningful:

  * structure   — the lattice / phase / cell assumptions
  * dataset     — provenance of any input data (or an explicit "none")
  * params      — the simulation parameters the human pinned
  * compute     — the compute envelope (WHERE it runs, what cap)

This module is the *mechanism*, not a prompt rule. A simulation tool that
sets ``requires_elicitation=True`` physically cannot return numbers until a
matching :class:`ResearchSpec` has been confirmed through the EXISTING
forge approval gate (``confirm_research_spec`` is ``requires_approval=True``,
so the harness prompts the human before it runs). No confirmed spec →
``Tool.execute`` returns a structured "elicitation required" payload and
the tool body never executes — so no fabricated numbers, and no heavy
compute silently burning the dev CPU.

Generalisable by construction: nothing here knows about a specific
chemistry, model, or fixture. The scope of a spec is whatever element
system the human declared; the gate just checks containment. Any tool can
opt in via the flag — this is not an alloy feature.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import Any

from app.tools.base import Tool, ToolRegistry

# Compute targets that mean "this would run on the developer's laptop CPU".
# Heavy compute on these is a hard refusal — it must route to hf-jobs /
# mesh / a real GPU. This is the "why the fuck would you run it on the
# CPU?" rule, encoded.
_DEV_CPU_TARGETS = {"local", "cpu", "dev", "laptop", "localhost", "inline"}


@dataclass
class ResearchSpec:
    """The four elicited inputs an informed human confirmed.

    ``system`` is the set of element symbols (or free-form domain tags for
    non-compositional work) this spec authorises. ``confirmed_by`` is
    free text the agent fills in from what it elicited — it is NEVER a
    role enum and NEVER asked as "what is your role"; identity is
    emergent. A spec only becomes authoritative once it has passed
    through ``confirm_research_spec`` (approval-gated).
    """

    system: list[str]
    structure: str
    dataset: str
    params: dict[str, Any]
    compute_envelope: dict[str, Any]
    confirmed_by: str
    rationale: str = ""
    confirmed_at: float = field(default_factory=time.time)

    def covers(self, elements: set[str]) -> bool:
        """True if every element in the call is authorised by this spec.

        An empty call-scope (non-compositional tool) is covered by any
        confirmed spec — the human still confirmed structure/params/compute.
        """
        declared = {e for e in self.system}
        return elements.issubset(declared) if elements else True

    def compute_target(self) -> str:
        return str(self.compute_envelope.get("target", "")).strip().lower()

    def is_dev_cpu(self) -> bool:
        return self.compute_target() in _DEV_CPU_TARGETS


class ResearchSpecLedger:
    """Process-scoped registry of confirmed specs.

    Lifetime = the MCP-server process = the research session. Drafts and
    confirmed specs are separate: a draft is what the agent proposed; a
    confirmed spec is one the human approved through the existing gate.
    """

    def __init__(self) -> None:
        self._draft: ResearchSpec | None = None
        self._confirmed: list[ResearchSpec] = []

    def set_draft(self, spec: ResearchSpec) -> None:
        self._draft = spec

    def draft(self) -> ResearchSpec | None:
        return self._draft

    def confirm_draft(self) -> ResearchSpec:
        if self._draft is None:
            raise ValueError(
                "no draft research spec to confirm — call propose_research_spec first"
            )
        self._confirmed.append(self._draft)
        confirmed = self._draft
        self._draft = None
        return confirmed

    def matching(self, elements: set[str]) -> ResearchSpec | None:
        """Most-recently-confirmed spec that covers the element scope."""
        for spec in reversed(self._confirmed):
            if spec.covers(elements):
                return spec
        return None

    def reset(self) -> None:
        self._draft = None
        self._confirmed.clear()


# Process-scoped singleton. Tests reset it via ``_LEDGER.reset()``.
_LEDGER = ResearchSpecLedger()


def get_ledger() -> ResearchSpecLedger:
    return _LEDGER


# ---------------------------------------------------------------------------
# Structural scope extraction — chemistry-agnostic
# ---------------------------------------------------------------------------

def extract_element_scope(kwargs: dict[str, Any]) -> set[str]:
    """Pull the element system out of a simulation call's kwargs.

    Structural, not chemistry-aware: it walks the kwarg *shapes* that
    PRISM simulation tools already use to carry a composition. Unknown
    shapes yield an empty scope (covered by any confirmed spec — the human
    still confirmed structure/params/compute for the session).
    """
    elements: set[str] = set()

    def _ingest(value: Any) -> None:
        if isinstance(value, str):
            elements.add(value)
        elif isinstance(value, (list, tuple, set)):
            for v in value:
                if isinstance(v, str):
                    elements.add(v)
        elif isinstance(value, dict):
            elements.update(str(k) for k in value)

    if "base_elements" in kwargs:
        _ingest(kwargs["base_elements"])
    if "system" in kwargs:
        _ingest(kwargs["system"])

    comp = kwargs.get("composition")
    if isinstance(comp, dict):
        if isinstance(comp.get("atoms"), dict):
            _ingest(comp["atoms"])
        if "elements" in comp:
            _ingest(comp["elements"])

    structure = kwargs.get("structure")
    if isinstance(structure, dict):
        inner = structure.get("composition")
        if isinstance(inner, dict) and isinstance(inner.get("atoms"), dict):
            _ingest(inner["atoms"])

    for grid in kwargs.get("candidate_grid", []) or []:
        if isinstance(grid, dict):
            _ingest(grid)

    # Strip obvious non-symbols (numbers-as-strings, empties).
    return {e for e in elements if e and not e.replace(".", "").isdigit()}


def check_elicitation(tool_name: str, kwargs: dict[str, Any]) -> dict[str, Any] | None:
    """Return ``None`` if the call may proceed, else a refusal payload.

    The refusal tells the agent exactly what to elicit and on which
    substrate (``follow_up`` → ``propose_research_spec`` →
    ``confirm_research_spec``). It never fabricates defaults.
    """
    scope = extract_element_scope(kwargs)
    spec = _LEDGER.matching(scope)

    if spec is None:
        return {
            "elicitation_required": True,
            "tool": tool_name,
            "element_scope": sorted(scope),
            "missing": ["structure", "dataset", "params", "compute"],
            "why": (
                "PRISM is an informed-autonomy harness. No informed human has "
                "confirmed the inputs for this simulation scope, so it will not "
                "run (running anyway would mean fabricated or unfounded numbers)."
            ),
            "do_this": [
                "Use the follow_up tool to ASK the informed human for: the "
                "structure/phase assumptions, the input dataset (or 'none'), "
                "the simulation parameters, and the compute envelope "
                "(WHERE it runs — must be hf-jobs / mesh / GPU, never the dev CPU).",
                "Call propose_research_spec with what you elicited.",
                "Call confirm_research_spec — the harness will prompt the "
                "human to approve it (this IS the informed-consent gate).",
                "Then retry this simulation.",
            ],
            "compute_rule": (
                "Heavy compute MUST target hf-jobs / mesh / a real GPU. "
                f"Dev-CPU targets {sorted(_DEV_CPU_TARGETS)} are hard-rejected."
            ),
        }

    if spec.is_dev_cpu():
        return {
            "elicitation_required": True,
            "tool": tool_name,
            "blocked_reason": "compute_envelope_targets_dev_cpu",
            "confirmed_target": spec.compute_target(),
            "why": (
                "A spec is confirmed but its compute envelope targets the dev "
                "CPU. Heavy simulation never runs on the developer laptop."
            ),
            "do_this": [
                "Re-elicit the compute envelope (hf-jobs / mesh / GPU) via "
                "follow_up, propose_research_spec, then confirm_research_spec.",
            ],
        }

    return None


# ---------------------------------------------------------------------------
# The two tools — drafted by the agent, confirmed by the human
# ---------------------------------------------------------------------------

def _propose_research_spec(**kwargs: Any) -> dict[str, Any]:
    """Record the agent's draft of the elicited inputs (no compute, no gate).

    The agent calls this AFTER eliciting structure/dataset/params/compute
    from the informed human via ``follow_up``. It does not authorise
    anything on its own — ``confirm_research_spec`` does.
    """
    compute = kwargs.get("compute_envelope") or {}
    target = str(compute.get("target", "")).strip().lower()
    draft = ResearchSpec(
        system=list(kwargs.get("system", [])),
        structure=str(kwargs.get("structure", "")),
        dataset=str(kwargs.get("dataset", "")),
        params=dict(kwargs.get("params", {})),
        compute_envelope=dict(compute),
        confirmed_by=str(kwargs.get("elicited_from", "")),
        rationale=str(kwargs.get("rationale", "")),
    )
    _LEDGER.set_draft(draft)

    warnings: list[str] = []
    if target in _DEV_CPU_TARGETS:
        warnings.append(
            f"compute_envelope.target={target!r} is a dev-CPU target and will "
            "be REJECTED at confirm time. Re-elicit a hf-jobs/mesh/GPU target."
        )
    for fld in ("system", "structure", "params", "compute_envelope"):
        if not getattr(draft, fld):
            warnings.append(f"{fld} is empty — elicit it before confirming.")

    return {
        "draft_recorded": True,
        "draft": {
            "system": draft.system,
            "structure": draft.structure,
            "dataset": draft.dataset,
            "params": draft.params,
            "compute_envelope": draft.compute_envelope,
            "elicited_from": draft.confirmed_by,
            "rationale": draft.rationale,
        },
        "warnings": warnings,
        "next": (
            "Call confirm_research_spec to put this through the human "
            "approval gate. Do NOT confirm a spec the human did not actually "
            "approve in the follow_up exchange."
        ),
    }


def _confirm_research_spec(**kwargs: Any) -> dict[str, Any]:
    """Promote the draft to a confirmed, authoritative spec.

    ``requires_approval=True`` on this tool means the forge harness prompts
    the human BEFORE this body runs — that prompt is the informed-consent
    gate. Reaching this code means the human already approved.
    """
    draft = _LEDGER.draft()
    if draft is None:
        return {
            "confirmed": False,
            "error": "no draft to confirm — call propose_research_spec first",
        }
    if draft.is_dev_cpu():
        return {
            "confirmed": False,
            "error": (
                f"compute envelope targets dev CPU ({draft.compute_target()!r}); "
                "heavy compute must route to hf-jobs / mesh / GPU. Re-propose "
                "with a real compute target."
            ),
        }
    confirmed = _LEDGER.confirm_draft()
    return {
        "confirmed": True,
        "authorised_system": confirmed.system,
        "compute_target": confirmed.compute_target(),
        "confirmed_by": confirmed.confirmed_by,
        "note": (
            "Simulation tools for this element scope are now unlocked for "
            "this session. Other scopes still require their own confirmed spec."
        ),
    }


_PROPOSE_SCHEMA = {
    "type": "object",
    "properties": {
        "system": {
            "type": "array",
            "items": {"type": "string"},
            "description": "Element symbols (or domain tags) this spec authorises, "
            "e.g. ['Cu','Ni','Si']. Elicit from the informed human.",
        },
        "structure": {
            "type": "string",
            "description": "Structure/phase/cell assumptions the human pinned "
            "(e.g. 'FCC solid solution, 100-atom SQS, a from Vegard').",
        },
        "dataset": {
            "type": "string",
            "description": "Provenance of any input data, or the literal 'none'.",
        },
        "params": {
            "type": "object",
            "description": "Simulation parameters the human pinned "
            "(fmax, steps, strain, T, etc.).",
        },
        "compute_envelope": {
            "type": "object",
            "description": "WHERE it runs + caps. Must include 'target' — one of "
            "hf-jobs / mesh / a GPU flavor. Dev-CPU targets are rejected.",
        },
        "elicited_from": {
            "type": "string",
            "description": "Free text: who provided these inputs, in their own "
            "terms. Emergent — never a role enum, never asked as 'your role'.",
        },
        "rationale": {
            "type": "string",
            "description": "Why these inputs are scientifically appropriate.",
        },
    },
    "required": ["system", "structure", "dataset", "params", "compute_envelope"],
    "additionalProperties": False,
}


def create_elicitation_tools(registry: ToolRegistry) -> None:
    """Register the two elicitation tools. Neither sets requires_elicitation
    (they are how you SATISFY the gate, not gated themselves)."""

    registry.register(Tool(
        name="propose_research_spec",
        description=(
            "Record a DRAFT of the structure/dataset/params/compute inputs you "
            "elicited from an informed human (via follow_up) before running any "
            "simulation. No compute. Returns warnings if inputs are missing or "
            "the compute target is a dev CPU. Follow with confirm_research_spec."
        ),
        input_schema=_PROPOSE_SCHEMA,
        func=_propose_research_spec,
        requires_approval=False,
        source="builtin",
        source_detail="app.tools.elicitation",
    ))

    registry.register(Tool(
        name="confirm_research_spec",
        description=(
            "Promote the drafted research spec to an authoritative one. "
            "requires_approval=True: the harness prompts the informed human to "
            "approve BEFORE this runs — that approval IS the informed-consent "
            "gate. Only after this do simulation tools for the spec's element "
            "scope unlock for the session."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "acknowledgement": {
                    "type": "string",
                    "description": "Short restatement of what the human approved "
                    "(for the audit trail).",
                },
            },
            "additionalProperties": False,
        },
        func=_confirm_research_spec,
        requires_approval=True,
        source="builtin",
        source_detail="app.tools.elicitation",
    ))
