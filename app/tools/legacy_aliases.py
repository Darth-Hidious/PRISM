"""Legacy tool-name aliases for SFT-trained agents.

The PRISM agent's SFT corpus references tool names that don't match the
post-refactor production registry. Without aliases, the agent emits
``prism_mace_screening`` calls that fail with "unknown tool" errors mid-run,
breaking long-horizon research trajectories.

This module registers thin wrapper Tools that forward to the canonical
primitives. Each wrapper is honest:

  * If a real underlying primitive exists → forward args (after light
    schema mapping where SFT shape differs from primitive shape).
  * If no underlying primitive exists yet → return a structured
    "not_implemented_in_this_build" error pointing at the canonical
    tool the agent should use instead. Better than silently degrading;
    the agent learns and picks a different path.

The aliases live behind their pre-refactor names to keep SFT-tutored
trajectories working. The canonical primitives are still registered
under their canonical names — both work, only the metadata differs.

ESA-grade reminder: every alias documents in its description exactly
which underlying primitive(s) it dispatches to, so audit/replay can
trace any agent action back to a single canonical tool execution.
"""

from __future__ import annotations

import json
import logging
import re
from typing import Any

from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _has(registry: ToolRegistry, name: str) -> bool:
    """ToolRegistry.get raises KeyError on miss — use this for existence checks."""
    return name in registry._tools  # noqa: SLF001 — registry has no public has()


def _get_or_none(registry: ToolRegistry, name: str) -> Tool | None:
    """ToolRegistry.get raises KeyError on miss — this returns None instead."""
    return registry._tools.get(name)  # noqa: SLF001


def _not_implemented(canonical_tool: str, reason: str) -> dict[str, Any]:
    """Honest error returned when an alias has no underlying primitive."""
    return {
        "error": "not_implemented_in_this_build",
        "alias_canonical_tool": canonical_tool,
        "reason": reason,
        "agent_hint": (
            f"This SFT-era name is not implemented as a backend in this build. "
            f"Use `{canonical_tool}` directly, or fall back to a different "
            f"approach (literature search, knowledge graph traversal)."
        ),
    }


def _forward(registry: ToolRegistry, target: str, args: dict[str, Any]) -> Any:
    """Look up `target` in registry and invoke it with args."""
    tool = _get_or_none(registry, target)
    if tool is None:
        return {
            "error": "alias_target_missing",
            "alias_canonical_tool": target,
            "reason": f"Underlying primitive `{target}` is not registered in this build.",
        }
    try:
        return tool.func(**args)
    except TypeError as e:
        return {
            "error": "alias_arg_mismatch",
            "alias_canonical_tool": target,
            "reason": str(e),
        }
    except Exception as e:
        logger.exception("alias dispatch to %s failed", target)
        return {
            "error": "alias_dispatch_failed",
            "alias_canonical_tool": target,
            "reason": f"{type(e).__name__}: {e}",
        }


# ---------------------------------------------------------------------------
# Multi-primitive orchestration aliases
# ---------------------------------------------------------------------------

_FORMULA_RE = re.compile(r"([A-Z][a-z]?)(\d*\.?\d*)")


def _parse_formula(formula: str, target_n_atoms: int = 100) -> dict[str, int]:
    """Parse a composition string to {element: integer_atom_count} summing to target_n_atoms.

    Accepts:
      - 'Nb25Mo25Ta25W25'  → {Nb: 25, Mo: 25, Ta: 25, W: 25}  (already at 100)
      - 'NbMoTaW'          → {Nb: 25, Mo: 25, Ta: 25, W: 25}  (equimolar, normalized to 100)
      - 'Nb0.5Mo0.5'       → {Nb: 50, Mo: 50}                  (atomic fractions, normalized)

    Raises ValueError on malformed input.
    """
    pairs = [(el, num) for el, num in _FORMULA_RE.findall(formula) if el]
    if not pairs:
        raise ValueError(f"could not parse formula: {formula!r}")

    # If no numbers given, treat as equimolar.
    if all(num == "" for _, num in pairs):
        per = target_n_atoms // len(pairs)
        remainder = target_n_atoms - per * len(pairs)
        result = {el: per for el, _ in pairs}
        if remainder:
            # Distribute remainder to first elements (deterministic).
            for el, _ in pairs[:remainder]:
                result[el] += 1
        return result

    # Mixed/numeric path: convert to floats, then scale to integer sum = target_n_atoms.
    raw = {el: float(num) if num else 1.0 for el, num in pairs}
    total = sum(raw.values())
    scaled = {el: v * target_n_atoms / total for el, v in raw.items()}
    # Round + correct rounding drift.
    rounded = {el: int(round(v)) for el, v in scaled.items()}
    drift = target_n_atoms - sum(rounded.values())
    if drift:
        # Apply drift to the element with the largest fractional remainder.
        order = sorted(scaled.items(), key=lambda kv: -(kv[1] - int(kv[1])))
        for el, _ in order[: abs(drift)]:
            rounded[el] += 1 if drift > 0 else -1
    return rounded


def _prism_mace_screening_factory(registry: ToolRegistry):
    """Thin alias from SFT-era `prism_mace_screening` to a single MACE primitive.

    Per the architecture directive: this is a NAME alias, not an orchestrator.
    It forwards a single composition to `mace_relax_structure`. For multi-step
    workflows (relax + elastic + phase analysis + report) use the
    `alloy_discovery` Skill instead — that's where orchestration lives.

    Accepts the SFT-shaped args ({compositions, target_properties}) and
    silently uses the first composition. If multiple are passed, returns a
    structured hint pointing the agent at the `alloy_discovery` skill.
    """

    def _impl(compositions: list[str] | str = None, target_properties: list[str] | str = None, **kwargs):
        if compositions is None:
            return {"error": "missing_arg", "reason": "compositions is required"}

        comps = compositions if isinstance(compositions, list) else [c.strip() for c in compositions.split(",")]
        if not comps:
            return {"error": "missing_arg", "reason": "compositions is empty"}

        if len(comps) > 1:
            return {
                "error": "alias_scope_exceeded",
                "alias_canonical_tool": "alloy_discovery (skill)",
                "reason": (
                    f"prism_mace_screening is a single-composition alias for mace_relax_structure. "
                    f"You passed {len(comps)} compositions. For multi-composition screening with "
                    f"relax + elastic + phase analysis + report orchestration, call the "
                    f"`alloy_discovery` skill instead."
                ),
            }

        try:
            atoms = _parse_formula(comps[0], target_n_atoms=100)
        except ValueError as e:
            return {"error": "formula_parse_failed", "reason": str(e), "composition": comps[0]}

        return _forward(
            registry,
            "mace_relax_structure",
            {
                "composition": {"atoms": atoms},
                "phase": "bcc",
                "n_atoms": sum(atoms.values()),
                "options": {},
            },
        )

    return _impl


# ---------------------------------------------------------------------------
# Direct aliases (forward to one underlying primitive)
# ---------------------------------------------------------------------------

def _alias(registry: ToolRegistry, target_name: str, arg_map: dict[str, str] = None):
    """Build a forwarder. arg_map renames SFT arg names → primitive arg names."""

    def _impl(**kwargs):
        if arg_map:
            kwargs = {arg_map.get(k, k): v for k, v in kwargs.items()}
        return _forward(registry, target_name, kwargs)

    return _impl


# ---------------------------------------------------------------------------
# Registration entry point
# ---------------------------------------------------------------------------

def create_legacy_aliases(registry: ToolRegistry) -> None:
    """Register the 10 SFT-trained legacy tool names.

    Idempotent — if an alias collides with a canonical tool of the same name,
    we skip rather than overwrite. Aliases are additive; they never replace.

    Call this AFTER all canonical tools are registered.
    """

    aliases_registered = 0

    # 1. prism_mace_screening — thin alias to mace_relax_structure
    if not _has(registry, "prism_mace_screening") and _has(registry, "mace_relax_structure"):
        registry.register(
            Tool(
                name="prism_mace_screening",
                description=(
                    "[LEGACY ALIAS — canonical: mace_relax_structure] Single-composition "
                    "structural relaxation via MACE-MH-1. Returns a JobHandle; poll via "
                    "mace_get_job. For multi-composition screening with relax + elastic + "
                    "phase analysis + report orchestration, use the `alloy_discovery` skill — "
                    "that's the canonical entry point for full-pipeline discovery."
                ),
                input_schema={
                    "type": "object",
                    "properties": {
                        "compositions": {
                            "oneOf": [
                                {"type": "array", "items": {"type": "string"}},
                                {"type": "string"},
                            ],
                            "description": "List of compositions as formula strings (e.g. 'Nb25Mo25Ta25W25').",
                        },
                        "target_properties": {
                            "oneOf": [
                                {"type": "array", "items": {"type": "string"}},
                                {"type": "string"},
                            ],
                            "description": "Properties to screen (yield_strength, bulk_modulus, shear_modulus, hardness, phase_stability, creep_resistance, thermal_conductivity).",
                        },
                    },
                    "required": ["compositions"],
                },
                func=_prism_mace_screening_factory(registry),
                requires_approval=True,
            )
        )
        aliases_registered += 1

    # 2. prism_calphad_equilibrium — alias to analyze_phases
    if not _has(registry, "prism_calphad_equilibrium") and _has(registry, "analyze_phases"):
        registry.register(
            Tool(
                name="prism_calphad_equilibrium",
                description=(
                    "[LEGACY ALIAS — canonical: analyze_phases] CALPHAD phase-equilibrium "
                    "calculation at a single temperature point. Forwards composition + "
                    "temperature + database arguments to analyze_phases."
                ),
                input_schema={
                    "type": "object",
                    "properties": {
                        "composition": {"type": "string"},
                        "temperature": {"type": "number"},
                        "database": {"type": "string", "default": "TCHEA"},
                    },
                    "required": ["composition", "temperature"],
                },
                func=_alias(registry, "analyze_phases"),
                requires_approval=False,
            )
        )
        aliases_registered += 1

    # 3. prism_phase_diagram_lookup — alias to analyze_phases (range mode)
    if not _has(registry, "prism_phase_diagram_lookup") and _has(registry, "analyze_phases"):
        registry.register(
            Tool(
                name="prism_phase_diagram_lookup",
                description=(
                    "[LEGACY ALIAS — canonical: analyze_phases] Phase-diagram sweep over a "
                    "temperature range. Forwards to analyze_phases."
                ),
                input_schema={
                    "type": "object",
                    "properties": {
                        "system": {"type": "string"},
                        "temperature_range": {"oneOf": [{"type": "array"}, {"type": "string"}]},
                    },
                    "required": ["system"],
                },
                func=_alias(registry, "analyze_phases", arg_map={"system": "composition"}),
                requires_approval=False,
            )
        )
        aliases_registered += 1

    # 4. prism_property_filter — alias to select_materials
    if not _has(registry, "prism_property_filter") and _has(registry, "select_materials"):
        registry.register(
            Tool(
                name="prism_property_filter",
                description=(
                    "[LEGACY ALIAS — canonical: select_materials] Filter materials by class "
                    "+ property thresholds. Forwards to select_materials."
                ),
                input_schema={
                    "type": "object",
                    "properties": {
                        "material_class": {"type": "string"},
                        "filters": {"oneOf": [{"type": "object"}, {"type": "string"}]},
                    },
                },
                func=_alias(registry, "select_materials"),
                requires_approval=False,
            )
        )
        aliases_registered += 1

    # 5. prism_literature_search — alias to prior_art_search
    if not _has(registry, "prism_literature_search") and _has(registry, "prior_art_search"):
        registry.register(
            Tool(
                name="prism_literature_search",
                description=(
                    "[LEGACY ALIAS — canonical: prior_art_search] Federated literature search "
                    "across arXiv + Semantic Scholar + patents. Forwards to prior_art_search."
                ),
                input_schema={
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "domains": {"oneOf": [{"type": "array"}, {"type": "string"}]},
                        "max_results": {"type": "integer", "default": 20},
                    },
                    "required": ["query"],
                },
                func=_alias(registry, "prior_art_search"),
                requires_approval=False,
            )
        )
        aliases_registered += 1

    # 6. prism_graph_traverse — alias to knowledge
    if not _has(registry, "prism_graph_traverse") and _has(registry, "knowledge"):
        registry.register(
            Tool(
                name="prism_graph_traverse",
                description=(
                    "[LEGACY ALIAS — canonical: knowledge] Traverse the MARC27 knowledge graph "
                    "from a starting node. Forwards to the unified knowledge tool with "
                    "operation=graph_traverse."
                ),
                input_schema={
                    "type": "object",
                    "properties": {
                        "start_node": {"type": "string"},
                        "relationship_type": {"type": "string"},
                        "depth": {"type": "integer", "default": 2},
                    },
                    "required": ["start_node"],
                },
                func=lambda **kwargs: _forward(
                    registry,
                    "knowledge",
                    {"operation": "graph_traverse", **kwargs},
                ),
                requires_approval=False,
            )
        )
        aliases_registered += 1

    # 7. prism_molecular_dynamics — alias to mace_md_equilibrate
    if not _has(registry, "prism_molecular_dynamics") and _has(registry, "mace_md_equilibrate"):
        registry.register(
            Tool(
                name="prism_molecular_dynamics",
                description=(
                    "[LEGACY ALIAS — canonical: mace_md_equilibrate] NVT/NPT MD via MACE-MH-1. "
                    "Forwards to mace_md_equilibrate. For pure NPT or non-MACE potentials, "
                    "use compute_submit with job_type=md."
                ),
                input_schema={
                    "type": "object",
                    "properties": {
                        "composition": {"type": "string"},
                        "temperature_K": {"type": "number"},
                        "duration_ps": {"type": "number"},
                        "ensemble": {"type": "string", "default": "NVT"},
                    },
                    "required": ["composition", "temperature_K"],
                },
                func=lambda composition=None, temperature_K=None, duration_ps=None, ensemble="NVT", **_: _forward(
                    registry,
                    "mace_md_equilibrate",
                    {
                        "structure": {"formula": composition, "phase": "BCC"},
                        "options": {
                            "temperature_K": temperature_K,
                            "duration_ps": duration_ps,
                            "ensemble": ensemble,
                        },
                    },
                ),
                requires_approval=True,
            )
        )
        aliases_registered += 1

    # 8. prism_embed_and_store — alias to knowledge_write
    if not _has(registry, "prism_embed_and_store") and _has(registry, "knowledge_write"):
        registry.register(
            Tool(
                name="prism_embed_and_store",
                description=(
                    "[LEGACY ALIAS — canonical: knowledge_write] Embed text/data into the "
                    "MARC27 vector store + knowledge graph. Forwards to knowledge_write."
                ),
                input_schema={
                    "type": "object",
                    "properties": {
                        "content": {"type": "string"},
                        "domain": {"type": "string"},
                        "metadata": {"type": "object"},
                    },
                    "required": ["content"],
                },
                func=_alias(registry, "knowledge_write"),
                requires_approval=True,
            )
        )
        aliases_registered += 1

    # 9. prism_dft_submit — honest not-implemented (compute_submit exists but
    #    job_type=dft path is platform-side, not a single primitive).
    if not _has(registry, "prism_dft_submit"):
        registry.register(
            Tool(
                name="prism_dft_submit",
                description=(
                    "[LEGACY ALIAS — canonical: compute_submit] Submit a DFT job to the MARC27 "
                    "Compute Broker. Forwards composition + calculation_type + functional to "
                    "compute_submit with job_type=dft. Returns a JobHandle."
                ),
                input_schema={
                    "type": "object",
                    "properties": {
                        "composition": {"type": "string"},
                        "calculation_type": {"type": "string"},
                        "functional": {"type": "string", "default": "PBE"},
                    },
                    "required": ["composition", "calculation_type"],
                },
                func=lambda composition=None, calculation_type=None, functional="PBE", **_: _forward(
                    registry,
                    "compute_submit",
                    {
                        "job_type": "dft",
                        "payload": {
                            "composition": composition,
                            "calculation_type": calculation_type,
                            "functional": functional,
                        },
                    },
                )
                if _has(registry, "compute_submit")
                else _not_implemented(
                    "compute_submit",
                    "compute_submit tool not registered in this build; check MARC27 platform credentials.",
                ),
                requires_approval=True,
            )
        )
        aliases_registered += 1

    # 10. prism_monte_carlo_sensitivity — no underlying primitive, honest stub
    if not _has(registry, "prism_monte_carlo_sensitivity"):
        registry.register(
            Tool(
                name="prism_monte_carlo_sensitivity",
                description=(
                    "[LEGACY ALIAS — NOT IMPLEMENTED IN THIS BUILD] Monte Carlo sensitivity "
                    "analysis (Sobol' indices / variance decomposition) over composition "
                    "perturbations. No backend wired yet. Use prism_mace_screening over a "
                    "perturbed composition grid for an approximation."
                ),
                input_schema={
                    "type": "object",
                    "properties": {
                        "composition": {"type": "string"},
                        "property_name": {"type": "string"},
                        "perturbation_pct": {"type": "number", "default": 5.0},
                        "n_samples": {"type": "integer", "default": 1000},
                    },
                    "required": ["composition", "property_name"],
                },
                func=lambda **_: _not_implemented(
                    "prism_mace_screening (over a perturbation grid)",
                    "Monte Carlo sensitivity primitive is not yet implemented. "
                    "Build a composition grid (e.g. ±perturbation_pct around the base) "
                    "and call prism_mace_screening over the grid to approximate.",
                ),
                requires_approval=False,
            )
        )
        aliases_registered += 1

    logger.info("Registered %d legacy SFT-trained tool-name aliases", aliases_registered)
