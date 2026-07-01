"""KAG-style tool reasoning — helps LLMs understand tool relationships.

Problem: LLMs are "stupid" about tool selection because:
1. Tool descriptions are flat text — no structure showing how tools connect
2. The agent sees 15 keyword-matched tools but has no map of dependencies,
   sequencing, or data flow between them
3. Tool names don't always match what the user asks for (semantic gap)

KAG solution (adapted from arXiv:2409.13731):
1. Represent tools as a structured knowledge graph (not flat descriptions)
2. Decompose user requests into logical forms: retrieve → evaluate → rank → report
3. Return a RECOMMENDED TOOL SEQUENCE, not just a flat list of names
4. Include data flow: what each tool takes as input from prior tools

This tool is called by the agent BEFORE deciding which tools to use.
It returns a structured plan showing:
- Which tools to call, in what order
- What data flows between them
- What prerequisites exist
- What the expected output shape is

The agent then executes the plan, calling each tool in sequence.

## Architecture

ToolGraph: nodes = tools, edges = data-flow relationships
LogicalForm: decomposed user request → sequence of tool calls
KnowledgeBoundary: whether the LLM can answer directly or needs tools
"""
from __future__ import annotations

import json
import re
from typing import Any

from app.tools.base import Tool, ToolRegistry


# ── Tool relationship graph ─────────────────────────────────────────
#
# Defines how tools connect to each other through data flow.
# This is the KAG "mutual-indexing" layer — it tells the LLM
# "if you use tool A, its output feeds into tool B as input."
#
# Each relationship is: (source_tool, output_field) → (target_tool, input_field)
# This lets the reasoning engine build a data-flow pipeline.

TOOL_GRAPH = {
    # ── Materials search → evaluation chain ─────────────────────────
    "search_materials": {
        "outputs": ["results", "formula", "elements", "id"],
        "feeds_into": [
            {
                "tool": "alpha_predict",
                "description": "Evaluate search results with physics + ML models",
                "input_mapping": "results[].formula → formula",
                "when": "User wants to evaluate or rank search results",
            },
            {
                "tool": "dataset",
                "description": "Save search results as a named dataset",
                "input_mapping": "results → results",
                "when": "User wants to save or analyze results",
            },
            {
                "tool": "gfn_evaluate",
                "description": "Quick physics descriptors for search results",
                "input_mapping": "results[].formula → formula",
                "when": "User wants fast physics-only screening",
            },
        ],
    },

    # ── Alloy generation → evaluation chain ─────────────────────────
    "alloy_sample": {
        "outputs": ["top_alloys", "composition"],
        "feeds_into": [
            {
                "tool": "alpha_predict",
                "description": "Evaluate generated alloys with formation energy, elastic moduli",
                "input_mapping": "top_alloys[].composition → formula",
                "when": "User wants to screen generated candidates",
            },
            {
                "tool": "alpha_discover",
                "description": "Run full discovery loop with GFlowNet + active learning",
                "input_mapping": "elements → elements",
                "when": "User wants autonomous multi-round discovery",
            },
            {
                "tool": "gfn_evaluate",
                "description": "Physics descriptors for generated alloys",
                "input_mapping": "top_alloys[].formula → formula",
                "when": "Quick stability check needed",
            },
        ],
    },

    "alloy_discover": {
        "outputs": ["top_alloys", "composition"],
        "feeds_into": [
            {
                "tool": "alpha_predict",
                "description": "Deep evaluation of discovered candidates",
                "input_mapping": "top_alloys[].composition → formula",
                "when": "User wants full multi-model evaluation of results",
            },
        ],
    },

    # ── Alpha predict → downstream chains ───────────────────────────
    "alpha_predict": {
        "outputs": ["verifiers", "consensus", "formation_energy", "stress_GPa"],
        "feeds_into": [
            {
                "tool": "dataset",
                "description": "Save evaluation results as structured data",
                "input_mapping": "verifiers → results",
                "when": "User wants to store or export results",
            },
            {
                "tool": "recall",
                "description": "Search past evaluations for comparison",
                "input_mapping": "",
                "when": "User asks 'how does this compare to previous results'",
            },
        ],
    },

    # ── Alpha discover → reporting chains ───────────────────────────
    "alpha_discover": {
        "outputs": ["pareto_set", "history", "visualizations"],
        "feeds_into": [
            {
                "tool": "alpha_predict",
                "description": "Deep-evaluate Pareto-optimal candidates",
                "input_mapping": "pareto_set[].formula → formula",
                "when": "User wants to verify discovery results with specific models",
            },
        ],
    },

    # ── Knowledge graph → reasoning chains ──────────────────────────
    "knowledge": {
        "outputs": ["results", "entities", "relationships"],
        "feeds_into": [
            {
                "tool": "alpha_predict",
                "description": "Evaluate materials found in knowledge graph",
                "input_mapping": "results[].formula → formula",
                "when": "User wants to evaluate KG search results",
            },
        ],
    },

    # ── Compute → status chains ────────────────────────────────────
    "compute_submit": {
        "outputs": ["job_id"],
        "feeds_into": [
            {
                "tool": "compute",
                "description": "Poll job status",
                "input_mapping": "job_id → job_id",
                "when": "Always after compute_submit",
            },
        ],
    },
}


# ── Logical form patterns ───────────────────────────────────────────
#
# KAG decomposes user requests into "logical forms" — typed sub-problems.
# Each pattern matches a class of user intent and maps to a tool sequence.
# This replaces keyword matching with intent-based tool selection.

LOGICAL_FORMS = [
    {
        "name": "discover_materials",
        "description": "User wants to find new materials/compositions",
        "patterns": [
            r"\b(find|discover|search|explore|generate|propose|design)\b.*\b(material|alloy|composition|compound|candidate)\b",
            r"\b(high.entropy|refractory|superalloy|HEA|RHEA)\b",
            r"\b(W.?Mo.?Ta.?Nb|Fe.?Cr.?Ni|Ti.?Al)\b",
        ],
        "tool_sequence": [
            {"tool": "alpha_discover", "role": "primary",
             "reason": "Autonomous discovery with GFlowNet + active learning"},
            {"tool": "alloy_sample", "role": "alternative",
             "reason": "Quick MCMC sampling without full AL loop"},
            {"tool": "search_materials", "role": "complement",
             "reason": "Search existing databases for known materials"},
        ],
        "data_flow": "search_materials → alpha_predict → alpha_discover",
    },
    {
        "name": "evaluate_composition",
        "description": "User wants to evaluate specific compositions",
        "patterns": [
            r"\b(evaluate|analyze|assess|check|compute|calculate)\b.*\b(composition|alloy|formula|property|energy|modulus)\b",
            r"\b(formation.energy|elastic|bulk.modulus|shear.modulus|density|stability)\b",
            r"\b(what.*(energy|modulus|density|property|delta|entropy))\b",
            r"[A-Z][a-z]?\d+\.?\d*(\s+[A-Z][a-z]?\d+\.?\d*)+",  # "W0.25 Mo0.25..." pattern
            r"\b(evaluate|check|analyze)\s+[A-Z]",  # "Evaluate W0.25..."
        ],
        "tool_sequence": [
            {"tool": "alpha_predict", "role": "primary",
             "reason": "Multi-fidelity evaluation: physics + M3GNet + MACE-MH-1"},
            {"tool": "gfn_evaluate", "role": "alternative",
             "reason": "Quick physics-only descriptors"},
        ],
        "data_flow": "alpha_predict → dataset (optional save)",
    },
    {
        "name": "search_existing",
        "description": "User wants to search databases for known materials",
        "patterns": [
            r"\b(search|find|lookup|query)\b.*\b(database|materials.project|optimade|literature)\b",
            r"\b(what.*known|existing|reported|published)\b",
        ],
        "tool_sequence": [
            {"tool": "search_materials", "role": "primary",
             "reason": "Federated OPTIMADE search across 20+ providers"},
            {"tool": "knowledge", "role": "complement",
             "reason": "Search MARC27 knowledge graph"},
            {"tool": "alpha_predict", "role": "followup",
             "reason": "Evaluate search results"},
        ],
        "data_flow": "search_materials → alpha_predict",
    },
    {
        "name": "run_compute",
        "description": "User wants to submit compute jobs",
        "patterns": [
            r"\b(submit|run|deploy|launch)\b.*\b(job|compute|simulation|DFT|VASP)\b",
            r"\b(GPU|A100|H100|cluster)\b",
        ],
        "tool_sequence": [
            {"tool": "compute", "role": "precheck",
             "reason": "Check available GPUs and estimate cost"},
            {"tool": "compute_submit", "role": "primary",
             "reason": "Submit the job (approval required)"},
            {"tool": "compute", "role": "followup",
             "reason": "Poll job status"},
        ],
        "data_flow": "compute(list_gpus) → compute_submit → compute(status)",
    },
    {
        "name": "mesh_operations",
        "description": "User wants mesh/network operations",
        "patterns": [
            r"\b(mesh|peer|node|federat|network|discover.peers)\b",
        ],
        "tool_sequence": [
            {"tool": "mesh_health", "role": "precheck",
             "reason": "Check if mesh is online"},
            {"tool": "mesh_peers", "role": "primary",
             "reason": "List available peers"},
            {"tool": "mesh_publish", "role": "action",
             "reason": "Publish dataset (approval required)"},
        ],
        "data_flow": "mesh_health → mesh_peers → mesh_publish/subscribe",
    },
    {
        "name": "recall_memory",
        "description": "User refers to previous results",
        "patterns": [
            r"\b(previous|earlier|before|last|yesterday|what did we)\b",
            r"\b(recall|remember|show me.*results)\b",
        ],
        "tool_sequence": [
            {"tool": "recall", "role": "primary",
             "reason": "Hybrid search over past tool outputs"},
            {"tool": "list_artifacts", "role": "complement",
             "reason": "List by metadata (tool, session, time)"},
            {"tool": "fetch_artifact", "role": "followup",
             "reason": "Get full content of a recalled artifact"},
        ],
        "data_flow": "recall → fetch_artifact",
    },
]


# ── Knowledge boundary (KAG concept) ────────────────────────────────
#
# Determines whether the LLM can answer directly or needs tools.
# Simple questions (greetings, explanations, definitions) don't need tools.

KNOWLEDGE_BOUNDARY_PATTERNS = [
    # These patterns indicate the user wants a direct answer, not tools
    r"^(hi|hello|hey|thanks|bye)\b",
    r"\b(help|how do I|how to)\b.*(?!\b(tool|alloy|material|compute)\b)",
]


def _classify_intent(query: str) -> dict:
    """Classify user intent using KAG-style logical form matching.

    Returns:
        {
            "intent": str or None,
            "needs_tools": bool,
            "recommended_tools": [...],
            "data_flow": str,
            "reasoning": str,
        }
    """
    query_lower = query.lower().strip()

    # Knowledge boundary: can the LLM answer directly?
    for pattern in KNOWLEDGE_BOUNDARY_PATTERNS:
        if re.search(pattern, query_lower):
            return {
                "intent": "direct_answer",
                "needs_tools": False,
                "recommended_tools": [],
                "data_flow": "",
                "reasoning": "This is a conversational or explanatory request. "
                             "Answer directly without tools.",
            }

    # Match logical forms
    for form in LOGICAL_FORMS:
        for pattern in form["patterns"]:
            if re.search(pattern, query, re.IGNORECASE):
                return {
                    "intent": form["name"],
                    "needs_tools": True,
                    "recommended_tools": form["tool_sequence"],
                    "data_flow": form["data_flow"],
                    "reasoning": f"Matched intent '{form['name']}': {form['description']}",
                }

    # Fallback: keyword-based tool suggestions
    suggestions = _keyword_tool_suggestions(query_lower)
    return {
        "intent": "unknown",
        "needs_tools": len(suggestions) > 0,
        "recommended_tools": suggestions,
        "data_flow": "",
        "reasoning": "No logical form matched. Falling back to keyword suggestions.",
    }


def _keyword_tool_suggestions(query: str) -> list[dict]:
    """Fallback: suggest tools by keyword matching (enhanced).

    This is better than the Rust-side keyword matching because it
    includes the full tool descriptions and data flow context.
    """
    suggestions = []
    query_words = set(query.split())

    # Domain keyword → tool mapping
    keyword_map = {
        "alloy": ["alpha_predict", "alloy_sample", "alpha_discover", "gfn_evaluate"],
        "composition": ["alpha_predict", "gfn_evaluate", "alloy_sample"],
        "energy": ["alpha_predict", "alpha_discover"],
        "formation": ["alpha_predict"],
        "elastic": ["alpha_predict"],
        "modulus": ["alpha_predict"],
        "density": ["alpha_predict", "gfn_evaluate"],
        "stability": ["alpha_predict", "alpha_discover"],
        "search": ["search_materials", "knowledge"],
        "discover": ["alpha_discover", "alloy_sample", "alloy_discover"],
        "compute": ["compute", "compute_submit"],
        "mesh": ["mesh_health", "mesh_peers", "mesh_subscriptions"],
        "dataset": ["dataset", "list_artifacts"],
        "workflow": ["workflow"],
        "model": ["models_list", "alpha_predict"],
        "battery": ["alpha_discover", "alpha_predict"],
        "catalyst": ["alpha_discover", "alpha_predict"],
    }

    seen = set()
    for word in query_words:
        for tool_name in keyword_map.get(word, []):
            if tool_name not in seen:
                seen.add(tool_name)
                suggestions.append({
                    "tool": tool_name,
                    "role": "suggested",
                    "reason": f"Keyword '{word}' matched",
                })

    return suggestions[:10]


def _build_tool_graph_summary(tool_names: list[str]) -> dict:
    """Build a data-flow subgraph for the specified tools.

    Shows how the recommended tools connect to each other.
    This is the KAG "mutual-indexing" — the LLM sees not just
    what tools exist, but how they relate.
    """
    nodes = []
    edges = []

    for name in tool_names:
        if name in TOOL_GRAPH:
            entry = TOOL_GRAPH[name]
            nodes.append({
                "tool": name,
                "outputs": entry["outputs"],
            })
            for target in entry.get("feeds_into", []):
                if target["tool"] in tool_names or len(tool_names) <= 3:
                    edges.append({
                        "from": name,
                        "to": target["tool"],
                        "description": target["description"],
                        "when": target["when"],
                    })

    return {"nodes": nodes, "edges": edges}


def _tool_reasoning(**kwargs) -> dict:
    """Main tool function — KAG-style reasoning about tool selection.

    Called by the agent BEFORE deciding which tools to use. Returns
    a structured recommendation showing:
    - What intent the user's request matches
    - Which tools to call, in what order
    - How data flows between them
    - Whether tools are needed at all

    The agent uses this output to plan its execution, then calls
    each recommended tool in sequence.
    """
    query = kwargs.get("query", "")
    if not query:
        return {"error": "`query` is required — pass the user's request."}

    # Classify intent
    classification = _classify_intent(query)

    # Build tool graph for recommended tools
    recommended_names = [t["tool"] for t in classification["recommended_tools"]
                         if isinstance(t, dict)]
    graph = _build_tool_graph_summary(recommended_names)

    # Build LLM-friendly explanation
    explanation_parts = [
        f"Intent: {classification['intent']}",
        f"Needs tools: {classification['needs_tools']}",
        f"Reasoning: {classification['reasoning']}",
    ]

    if classification["recommended_tools"]:
        explanation_parts.append("\nRecommended tool sequence:")
        for i, tool in enumerate(classification["recommended_tools"]):
            if isinstance(tool, dict):
                explanation_parts.append(
                    f"  {i+1}. {tool['tool']} ({tool.get('role', 'suggested')}): "
                    f"{tool.get('reason', '')}"
                )

    if classification["data_flow"]:
        explanation_parts.append(f"\nData flow: {classification['data_flow']}")

    if graph["edges"]:
        explanation_parts.append("\nTool relationships:")
        for edge in graph["edges"]:
            explanation_parts.append(
                f"  {edge['from']} → {edge['to']}: {edge['description']}"
            )

    return {
        "query": query,
        "classification": classification,
        "tool_graph": graph,
        "explanation": "\n".join(explanation_parts),
    }


# ── Tool description (keyword-rich for selection) ───────────────────

_TOOL_REASONING_DESCRIPTION = (
    "KAG-style tool reasoning and planning. Given the user's request, "
    "classifies intent using logical-form matching and returns a "
    "RECOMMENDED TOOL SEQUENCE with data-flow relationships. Call this "
    "BEFORE deciding which tools to use — it shows which tools to call, "
    "in what order, and how their outputs feed into each other.\n\n"
    "Use this tool when:\n"
    "  - The user's request is complex or multi-step\n"
    "  - You're unsure which tools to call\n"
    "  - The user asks for discovery, evaluation, search, compute, or "
    "mesh operations\n"
    "  - You need to understand how tools connect (data flow)\n\n"
    "Do NOT call this for:\n"
    "  - Simple greetings or explanations (answer directly)\n"
    "  - When you already know exactly which tool to call\n\n"
    "The tool returns: intent classification, recommended tools with "
    "roles (primary/alternative/complement/followup), data-flow graph, "
    "and a human-readable explanation. Read-only; no approval gate."
)


# ── Registration ─────────────────────────────────────────────────────


def create_tool_reasoning_tool(registry: ToolRegistry) -> None:
    """Register the KAG-style tool reasoning tool."""
    registry.register(Tool(
        name="tool_reasoning",
        description=_TOOL_REASONING_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": (
                        "The user's request verbatim. The reasoning engine "
                        "matches this against logical forms to determine "
                        "intent and recommend tools."
                    ),
                },
            },
            "required": ["query"],
            "additionalProperties": False,
        },
        func=_tool_reasoning,
        requires_approval=False,
        source="builtin",
        source_detail="KAG-style tool reasoning (arXiv:2409.13731)",
    ))