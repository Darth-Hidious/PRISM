"""Generate docs/TOOL_CATALOG.md from the live registry + hand annotations.

Run from the repo root:

    PRISM_DISABLE_MEMORY=1 python3 scripts/gen_tool_catalog.py

When adding a tool, add its (category, status, note) entry to ANN below —
the script refuses to run with unannotated tools so the catalog can't
silently go stale.
"""
import re
from datetime import date

from app.plugins.bootstrap import build_full_registry

# category, status, status-note (hand-annotated from the 2026-07 unification audit)
ANN = {
    "file":                        ("system",      "working", ""),
    "show_scratchpad":             ("system",      "working", ""),
    "execute_python":              ("system",      "working", ""),
    "execute_bash":                ("system",      "working", ""),
    "bash_task":                   ("system",      "working", ""),
    "stop_bash_task":              ("system",      "working", ""),
    "tool_reasoning":              ("system",      "working", ""),
    "session_context":             ("system",      "working", ""),
    "agent_capabilities":          ("platform",    "needs-login", ""),
    "policy_evaluate":             ("platform",    "needs-login", ""),
    "usage_status":                ("platform",    "needs-login", ""),
    "billing_balance":             ("platform",    "needs-login", ""),
    "knowledge_write":             ("knowledge",   "needs-login", ""),
    "research":                    ("research",    "needs-login", "money-spending, approval-gated"),
    "start_background_research":   ("research",    "needs-login", "money-spending, approval-gated"),
    "check_background_research":   ("research",    "needs-login", ""),
    "list_background_research":    ("research",    "needs-login", ""),
    "cancel_background_research":  ("research",    "needs-login", ""),
    "compute":                     ("compute",     "needs-login", ""),
    "compute_submit":              ("compute",     "needs-login", "money-spending, approval-gated"),
    "platform_jobs":               ("platform",    "needs-login", ""),
    "platform_jobs_submit":        ("platform",    "needs-login", "money-spending, approval-gated"),
    "platform_workflows":          ("platform",    "needs-login", ""),
    "platform_workflows_run":      ("platform",    "needs-login", "money-spending, approval-gated"),
    "mcp_services":                ("platform",    "needs-login", ""),
    "mcp_services_invoke":         ("platform",    "needs-login", "approval-gated"),
    "mesh_peers":                  ("mesh",        "needs-login", ""),
    "mesh_health":                 ("mesh",        "needs-login", ""),
    "mesh_subscriptions":          ("mesh",        "needs-login", ""),
    "mesh_publish":                ("mesh",        "needs-login", "approval-gated"),
    "mesh_subscribe":              ("mesh",        "needs-login", "approval-gated"),
    "mesh_unsubscribe":            ("mesh",        "needs-login", "approval-gated"),
    "query_materials_project":     ("data",        "working", "needs MP_API_KEY or `prism login` (proxy); else points to materials_search"),
    "materials_search":            ("data",        "working", "keyless OPTIMADE federation"),
    "dataset":                     ("data",        "working", ""),
    "plot":                        ("data",        "working", ""),
    "list_predictable_properties": ("ml",          "working", ""),
    "predict":                     ("ml",          "working", "formula target needs a trained model — run model_train once"),
    "model_train":                 ("ml",          "working", "MP fetch needs MP key or login; local datasets always work"),
    "list_models":                 ("ml",          "working", ""),
    "prior_art_search":            ("research",    "working", "patents need LENS_API_TOKEN"),
    "web":                         ("research",    "working", "Firecrawl key optional; DuckDuckGo fallback"),
    "labs":                        ("labs",        "stub",    "browse/info real; submit NOT live (all services coming_soon)"),
    "structure":                   ("simulation",  "needs-deps", "sidecar pyiron blocked on HDF5"),
    "sim_run":                     ("simulation",  "needs-deps", "sidecar pyiron blocked on HDF5"),
    "sim_job":                     ("simulation",  "needs-deps", "sidecar pyiron blocked on HDF5"),
    "list_potentials":             ("simulation",  "needs-deps", "sidecar pyiron blocked on HDF5"),
    "check_hpc_queue":             ("simulation",  "needs-deps", "sidecar pyiron blocked on HDF5"),
    "run_convergence_test":        ("simulation",  "needs-deps", "sidecar pyiron blocked on HDF5"),
    "run_workflow":                ("simulation",  "needs-deps", "sidecar pyiron blocked on HDF5"),
    "calphad":                     ("simulation",  "working", "via py3.12 sidecar; needs TDB files in ~/.prism/databases"),
    "calphad_compute":             ("simulation",  "working", "via py3.12 sidecar; needs TDB files; approval-gated"),
    "mace_relax_structure":        ("simulation",  "working", "approval-gated; platform backend needs login"),
    "mace_md_equilibrate":         ("simulation",  "working", "approval-gated; platform backend needs login"),
    "mace_phonon_harmonic":        ("simulation",  "working", "approval-gated; platform backend needs login"),
    "mace_compute_elastic":        ("simulation",  "working", "approval-gated; platform backend needs login"),
    "mace_compute_dilute_solute":  ("simulation",  "working", "approval-gated; platform backend needs login"),
    "mace_estimate_cost":          ("simulation",  "working", ""),
    "mace_get_job":                ("simulation",  "working", ""),
    "mace_list_jobs":              ("simulation",  "working", ""),
    "mace_cancel_job":             ("simulation",  "working", ""),
    "mace_get_cached_structure":   ("simulation",  "working", ""),
    "structure_import":            ("simulation",  "working", ""),
    "acquire_materials":           ("skills",      "working", "approval-gated"),
    "predict_properties":          ("skills",      "working", "approval-gated"),
    "generate_report":             ("skills",      "working", ""),
    "select_materials":            ("skills",      "working", ""),
    "materials_discovery":         ("skills",      "working", "approval-gated"),
    "plan_simulations":            ("skills",      "working", "approval-gated"),
    "analyze_phases":              ("skills",      "working", "approval-gated"),
}

SOURCE_MAP_NOTE = {
    "science-sidecar": "sidecar",
    "search_engine.federated": "local",
}


def one_liner(desc: str) -> str:
    """First sentence of the description, cleaned for a markdown table."""
    d = " ".join((desc or "").split())
    # cut at the first sentence end followed by a space+capital, or first bullet
    d = d.split("•")[0].strip()
    m = re.match(r"(.+?\.)\s+[A-Z‘“`(']", d)
    if m:
        d = m.group(1)
    if len(d) > 160:
        d = d[:157].rstrip() + "..."
    return d.replace("|", "\\|")


def source_of(t) -> str:
    detail = t.source_detail or ""
    if detail in SOURCE_MAP_NOTE:
        return SOURCE_MAP_NOTE[detail]
    cat = ANN.get(t.name, ("", "", ""))[0]
    if cat in ("platform", "knowledge", "research", "compute", "mesh") and ANN[t.name][1] == "needs-login":
        return "platform"
    if t.source == "mcp":
        return "MCP"
    return "local"


def main() -> None:
    registry, _, _ = build_full_registry(enable_mcp=False, enable_plugins=False)
    tools = list(registry.list_tools())

    missing = [t.name for t in tools if t.name not in ANN]
    if missing:
        raise SystemExit(f"unannotated tools: {missing}")

    lines = [
        "# PRISM Tool Catalog",
        "",
        f"All tools shipped with the PRISM agent, as registered by "
        f"`app/plugins/bootstrap.build_full_registry()` (generated {date.today()}, "
        f"{len(tools)} tools; external MCP servers add more at runtime and are "
        "not listed here).",
        "",
        "Regenerate with `PRISM_DISABLE_MEMORY=1 python3 scripts/gen_tool_catalog.py`",
        "(from the repo root), or verify the count with:",
        "",
        "```bash",
        "PRISM_DISABLE_MEMORY=1 python3 -c \"from app.plugins.bootstrap import build_full_registry; \\",
        "  r,_,_ = build_full_registry(enable_mcp=False, enable_plugins=False); print(len(list(r.list_tools())))\"",
        "```",
        "",
        "Status legend: **working** = runs today (notes list per-tool auth/data caveats) · "
        "**needs-login** = requires `prism login` to the MARC27 platform · "
        "**needs-deps** = blocked on an environment dependency · "
        "**stub** = intentionally not live yet.",
        "",
        "| Tool | Category | Source | Status | Notes | Description |",
        "|---|---|---|---|---|---|",
    ]
    order = {"system": 0, "data": 1, "ml": 2, "simulation": 3, "skills": 4,
             "research": 5, "knowledge": 6, "compute": 7, "platform": 8,
             "mesh": 9, "labs": 10}
    tools.sort(key=lambda t: (order.get(ANN[t.name][0], 99), t.name))
    for t in tools:
        cat, status, note = ANN[t.name]
        lines.append(
            f"| `{t.name}` | {cat} | {source_of(t)} | {status} | {note} | {one_liner(t.description)} |"
        )
    lines += [
        "",
        "## Notes",
        "",
        "- **Approval gates**: tools marked approval-gated prompt the user before "
        "running because they spend compute/money or mutate shared state.",
        "- **Science sidecar**: `structure`/`sim_*`/`calphad*` execute in a "
        "separate Python 3.12 venv (`~/.prism/venv-sci`, auto-provisioned) because "
        "pyiron/pycalphad don't install on the main interpreter. pycalphad works "
        "there; pyiron is currently blocked on an HDF5 build failure.",
        "- **Labs**: the `labs` marketplace catalog is browsable, but job "
        "submission is not live for any service yet — the tool says so itself.",
        "- **MACE**: composition inputs are integer atom counts (10 supported "
        "elements: Al, Fe, Hf, Mo, Nb, Ta, Ti, V, W, Zr). Structures can also be "
        "supplied via `structure_import` → `cache_ref`.",
        "",
    ]
    out = "docs/TOOL_CATALOG.md"
    with open(out, "w") as f:
        f.write("\n".join(lines))
    print(f"wrote {out}: {len(tools)} tools")


if __name__ == "__main__":
    main()
