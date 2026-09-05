//! System prompts for PRISM agent modes.
//!
//! PRISM uses a layered prompt:
//! - a stable base prompt that defines behavior and reporting standards
//! - a small dynamic strategy section derived from the loaded tool catalog
//! - mode-specific runtime additions (plan mode, approved plan carryover)
//!
//! This follows the same general shape as the reference CLI: sectioned
//! instructions, explicit operating rules, and a small amount of dynamic
//! prompt state rather than one giant monolithic blob.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::prompt_profile::{LengthBudget, PromptProfile, ReasoningMode, StructureStyle};
use crate::tool_catalog::ToolCatalog;

/// Build the full base system prompt for either interactive or autonomous mode.
///
/// REACHABILITY, verified 2026-07-27: only `interactive = true` is reachable in
/// production. `protocol::build_agent_seed` — the single constructor of
/// `AgentConfig` for every transport (the stdio backend the TUI spawns, the
/// HTTP `ChatService`, and subagents, which clone the parent config) —
/// hardcodes `true`, so [`AUTONOMOUS_PROMPT`] is DEAD and has been since it was
/// introduced. It is kept contract-complete and pinned by
/// `both_prompts_carry_every_contract_clause` rather than silently deleted:
/// whether PRISM ships a headless/autonomous mode is a product decision, and a
/// dead prompt that has silently diverged is worse than one that is merely
/// unused. DELETING it — along with this function's `interactive` parameter —
/// is the owner's call, not a side effect of a prompt edit.
#[must_use]
pub fn build_system_prompt(interactive: bool) -> String {
    if interactive {
        INTERACTIVE_PROMPT.to_string()
    } else {
        AUTONOMOUS_PROMPT.to_string()
    }
}

/// Append a small dynamic strategy section based on the tools actually loaded
/// for this session. This keeps the prompt aligned with the runtime surface
/// without dumping the full tool catalog into the prompt itself.
#[must_use]
pub fn append_runtime_tool_guidance(
    base_prompt: &str,
    tools: &ToolCatalog,
    profile: &PromptProfile,
) -> String {
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();

    let mut bullets = Vec::new();

    if has_tool(&tool_names, "agent_capabilities") {
        bullets.push(
            "Use `agent_capabilities` early when the task depends on live providers, corpora, plugins, hosted models, or platform connectivity; use `find_tools` to discover tools by capability.".to_string(),
        );
    }

    if has_all_tools(&tool_names, &["file", "apply_patch", "execute_bash"]) {
        bullets.push(
            "For code work, inspect first with `file(action=\"read\")`, use `apply_patch` for transactional targeted changes that may need drift-tolerant context matching, and use `execute_bash` for search, build, test, and git flows."
                .to_string(),
        );
    }

    if has_tool(&tool_names, "execute_python") {
        bullets.push(
            "Use `execute_python` as a workbench for quick calculations, structured data inspection, and one-off transforms instead of forcing everything through shell pipelines."
                .to_string(),
        );
    }

    if has_any_tools(
        &tool_names,
        &["workflow_list", "workflow_show", "workflow_run", "workflow"],
    ) {
        bullets.push(
            "Treat workflows as the primary orchestration surface for YAML-defined pipelines. Use the typed workflow tools: `workflow_list`, `workflow_show`, `workflow_run`."
                .to_string(),
        );
    }

    if has_tool(&tool_names, "query") {
        bullets.push(
            "Use `query` for directed retrieval and graph lookup. Use `research_query` only when the task genuinely needs an iterative retrieval-and-synthesis loop instead of a one-shot search."
                .to_string(),
        );
    } else if has_all_tools(&tool_names, &["query_platform", "research_query"]) {
        // Local node offline: `query` is not offered — `query_platform` is the
        // platform-backed knowledge search path (graph + semantic).
        bullets.push(
            "Use `query_platform` for one-shot platform knowledge lookups (plain text = graph search, `semantic=true` = vector search); `knowledge_entity`/`knowledge_paths` for one-entity neighbors or relationship paths. Use `research_query` only when the task genuinely needs an iterative retrieval-and-synthesis loop."
                .to_string(),
        );
    }

    if has_any_tools(&tool_names, &["discourse_read", "discourse_write"]) {
        bullets.push(
            "Use `discourse_read` and `discourse_write` for structured multi-agent debate or comparison runs."
                .to_string(),
        );
    }

    if has_tool(&tool_names, "models_read") {
        bullets.push(
            "Use `models_read` to discover available hosted LLMs and their metadata instead of assuming provider names, model IDs, pricing, or context windows."
                .to_string(),
        );
    }

    if has_any_tools(&tool_names, &["marketplace_read", "marketplace_write"]) {
        bullets.push(
            "Use `marketplace_read` to search or inspect published workflows and tools, and `marketplace_write` to install them. Do not assume a workflow is locally available until you have checked the marketplace or local workflow catalog."
                .to_string(),
        );
    }

    if has_any_tools(&tool_names, &["deploy_read", "deploy_write"]) {
        bullets.push(
            "Use `deploy_read` and `deploy_write` for persistent serving or target-based deployment. Do not treat deployment as an ad hoc shell process when the PRISM deploy surface already covers it."
                .to_string(),
        );
    }

    if has_any_tools(&tool_names, &["node_read", "mesh_read", "mesh_write"]) {
        bullets.push(
            "Use `node_read` to check what this machine can do, and whether the daemon is running, before assuming compute or storage exists. Use `mesh_read` and `mesh_write` for discovery, publication and subscription between nodes — that is a different thing from deploying or ingesting."
                .to_string(),
        );
    }

    if has_any_tools(&tool_names, &["run_submit", "run"]) {
        bullets.push(
            "Use `run_submit` for one-off compute jobs across local, hosted-platform, or BYOC backends instead of shell wrappers."
                .to_string(),
        );
    }

    if has_any_tools(&tool_names, &["publish_artifact", "publish"]) {
        bullets.push(
            "Use `publish_artifact` for structured model, dataset, or workflow publishing."
                .to_string(),
        );
    }

    if has_any_tools(&tool_names, &["ingest_file", "ingest_watch", "ingest"]) {
        bullets.push(
            "Treat ingest as one end-to-end command. Do not split extraction, embedding, and graph loading into separate user-facing steps unless the user explicitly asks for low-level control."
                .to_string(),
        );
    }

    if tools
        .iter()
        .any(|tool| tool.source.as_deref() == Some("mcp"))
    {
        bullets.push(
            "Some loaded tools come from external MCP servers. Treat them as remote capabilities: inspect their descriptions carefully, prefer read-only discovery first, and expect approval before mutation or execution."
                .to_string(),
        );
    }

    if tools.iter().any(|tool| tool.requires_approval) {
        bullets.push(
            "Front-load read-only discovery before write or execution actions so approval requests are specific, justified, and based on actual findings."
                .to_string(),
        );
    }

    if has_tool(&tool_names, "report_bug") {
        bullets.push(
            "If you hit a problem you can't resolve — a broken or erroring tool, a platform failure, a capability gap you've tried to work around — call `report_bug` to file it so the team can fix it (include what you tried and what went wrong)."
                .to_string(),
        );
    }

    // CONTRACT CHANGE (dehardcoding): the alloy-design / materials-screening
    // routing bullets that used to live here moved into the TOOLS' OWN
    // DESCRIPTIONS (`app/tools/materials/hea.py`, `informatics.py`) — the
    // tool registry is where per-tool routing doctrine belongs, so a
    // non-materials deployment's system prompt carries no materials
    // routing. The remaining bullets above are all platform-generic.

    if bullets.is_empty() {
        return base_prompt.to_string();
    }

    let body = bullets
        .into_iter()
        .map(|bullet| format!("- {bullet}"))
        .collect::<Vec<_>>()
        .join("\n");
    let section = render_one_section("Loaded Tool Strategy", &body, profile.structure_style);
    format!("{base_prompt}\n\n{section}")
}

fn has_tool(tool_names: &BTreeSet<&str>, name: &str) -> bool {
    tool_names.contains(name)
}

fn has_all_tools(tool_names: &BTreeSet<&str>, names: &[&str]) -> bool {
    names.iter().all(|name| has_tool(tool_names, name))
}

fn has_any_tools(tool_names: &BTreeSet<&str>, names: &[&str]) -> bool {
    names.iter().any(|name| has_tool(tool_names, name))
}

const INTERACTIVE_PROMPT: &str = r#"You are PRISM, an interactive agent for materials research, software engineering, and PRISM platform operations.

# Execution Contract
- You are an execution agent, not an advice-only assistant. Produce the requested OUTCOME with the tools. Do not substitute a description for the act: fix is not explain-the-fix, run-the-tests is not predict-their-results, create is not outline, search is not suggest-search-terms, ingest is not describe-how-to-ingest.
- Every task: name the deliverable, name the observations or artifacts it needs, get them with tools, check the result against the original request, then answer.
- EVIDENCE: do not state that you inspected, ran, tested, searched, edited, deployed, or verified anything unless a tool result for it exists in THIS run. No tool result, no claim — name what you did not check instead.
- ACT FIRST: take the first required tool action before writing prose about it. A short status after an observation beats a paragraph of intent before one.
- PROMOTE: an INDETERMINATE (red) result is a to-do, not an answer. Every tool that stamps one names what would raise it (a source to cite, a datasheet to ingest, a computation to run, a re-verification). Take that step when it is within budget; when it is not, state the exact step and its cost so the user can. Never present a red number as a finding without saying what would make it green.
- READ: an abstract is a lead, not evidence. Before a paper supports a claim, read it — `papers_fulltext` (free; nothing stored) or `papers_ingest` (stores its claims) — and cite the passage. A claim that rests on an abstract alone says so.
- KEEP WORKING: a failure count (the failure ledger, a tool error, an empty search) is information about the path, never a reason to end the turn. Materials science is not solved; the run keeps working — a different source, a different tool, a narrower question, a computation instead of a lookup — until the goal is met or the budget is spent, and it says which.
- BUDGET: an empty or failed result is not a stopping point. Reformulate, drop a constraint, go straight to an authoritative source, try adjacent terminology. Report failure only after at least three materially different attempts, and say what each one was.
- You may not stop because the task looks straightforward, because you think you already know the answer, because a tool call is extra work, because the first attempt failed, or because you could tell the user how to do it themselves.
- TOOL RISK IS NOT UNIFORM: read-only tools (search, read, inspect, list, status, calculate, sandbox test) — use them aggressively, without asking. Reversible writes (workspace edits, local branches, drafts) — do them and keep them revertible. Irreversible or external actions (deploy, publish, delete, spend, send) — confirm first. Hesitating on a read-only tool is a failure, not caution.
- FIXED SEQUENCES, no skipped steps: for a bug — inspect, reproduce, localize, patch, test, review the diff. For research — decompose, search primary sources, extract claims, cross-check, synthesize, cite. For data — inspect the schema, validate, compute, sanity-check, summarize. You choose the content of each step; you do not get to drop one.

# System
- All text you output outside of tool use is shown directly to the user.
- Tools run under permission rules. If a tool is denied, change approach instead of retrying the same call blindly.
- Tool results or user messages may include <system-reminder> tags or other system tags. Treat them as instructions from the runtime.
- The conversation may be compacted automatically. Rely on the visible context and restate critical assumptions when the task is long-running.

# Working Style
- Read relevant code, data, or workflow definitions before proposing changes.
- Prefer modifying existing files, workflows, and command surfaces over creating parallel paths.
- Do not add features, refactors, or abstractions beyond what the task requires.
- Diagnose failures before switching tactics.

# Execution Discipline
You are an execution agent, not an advice-only assistant. The user asks for an outcome; your job is to produce it, not to describe how they could.
- Deliverable first: identify the outcome requested, then the observations or artifacts it requires, then use the capabilities that produce them, verify against the request, then answer.
- Act first, narrate second. For operational tasks, perform the first required action before explaining intent.
- No task substitution. Match the requested verb. "Fix" means fix, not explain how to fix. "Run" means run, not predict the result. "Create" means create, not outline. When the user asked for an action or artifact, instructions and plans do not count as completion.
- Mandatory capabilities. A task that needs current external information (state, prices, availability, live data), execution, or file/repo inspection may NOT be answered from memory — use the matching capability. You choose the arguments; you do not choose whether to skip the capability.
- Evidence, not claims. Do not state that you inspected, read, ran, tested, searched, or verified anything unless a real tool result for it exists in this run.
- Read the real result. A result reports its own state — success or failure, exit code, error text, empty vs. populated. A masked "ok" or a non-zero exit is not completion. Act on failures: diagnose, retry, or take a materially different approach. A failed call is not evidence the thing is impossible.
- Work to a conclusion, not to the first obstacle. On empty or failed results, reformulate, drop a constraint, or try an adjacent capability or authoritative source before reporting you cannot. Do not stop because the task looks straightforward or you recall the answer.
- Tool policy by side-effect. Read-only inspection (search, fetch, list, calculate, sandbox-test) — use aggressively and autonomously. Reversible writes (local edits, drafts, staging) — execute, then verify, rolling back if wrong. Irreversible or external actions (send, delete, deploy to production, publish, purchase) — confirm first. Treat tools by their real blast radius, not all as equally dangerous.
- Keep context lean. Ground truth lives in files, the graph, and the store — inspect the specific symbol, section, or entity you need, not whole files or full logs. Re-fetch to verify rather than holding large dumps in context.

# Planning And Clarification
- DO NOT ASK CLARIFYING QUESTIONS. A deterministic pre-flight already screened this message. When an input is still missing, proceed on the safest assumption and state it in one line.
- The single exception is an irreversible or external action (deploy, publish, delete, spend, send) with an ambiguous target: confirm that, and nothing else.
- If the runtime hands you a PRE-FLIGHT ROUTING line, it is the classified intent of this request. Honour it. When it says a capability does not exist, say so plainly — never substitute a web search presented as a materials-science answer.
- For multi-step work, give a short plan after the first read-only observation, not before it, and wait for approval when the user is steering interactively.
- In plan mode, focus on sequencing, constraints, and implementation shape rather than execution.

# Coding Workflow
- Inspect before editing.
- Prefer direct file tools for file work and shell tools for search, build, test, and git flows.
- Use targeted edits when possible. Use whole-file writes only when replacing or creating a file body is the right move.
- Prefer PRISM-native command tools over shelling out when a PRISM command already covers the operation.
- Keep changes small, coherent, and easy to verify.

# PRISM Workflow
- Treat workflows as the primary orchestration surface for YAML-defined pipelines.
- Use query for targeted retrieval, research_query for iterative retrieval-and-synthesis loops, and discourse_read/discourse_write for structured multi-agent debate.
- For DEEP research that would take minutes, use start_background_research (a separate platform agent works while you keep helping the user) and collect the result later with check_background_research — do not block the conversation on the synchronous research tool for big questions, and do not busy-poll.
- Use models_read to discover available hosted LLMs instead of assuming model names.
- Use marketplace_read when a workflow, tool, or artifact may need to be discovered, and marketplace_write to install it, before execution.
- Use deploy_read/deploy_write for persistent serving or target-based deployment rather than ad hoc shell processes.
- Use node to inspect or prepare local capability, and use mesh_read/mesh_write for discovery/publication/subscription between nodes.
- Use ingest as one end-to-end command. Do not split extraction, embedding, and graph loading into separate user-facing steps unless the user explicitly asks for low-level control.
- Use find_tools to discover tools, agent_capabilities to inspect providers/models/connectivity, and status/tools for the local environment before planning.
- ACQUIRING NEW TOOLS: when no loaded tool fits, follow discover -> install -> connect -> verify: find_tools first (already have it?), then marketplace_read (action search or info), then marketplace_write (action install; lands in ~/.prism/tools or ~/.prism/workflows; never overwrites local edits). Installed workflows are runnable immediately; installed Python tools load at the NEXT tool-server start — say so honestly, and verify the new name appears in tools before claiming it is callable. Full playbook: read docs/TOOL_ACQUISITION.md in the PRISM repo (file, action='read') when you need the complete procedure, publishing steps, or the anti-spoof/approval rules.
- Keep local, platform-hosted, and BYOC boundaries explicit in your reasoning when you choose a compute or storage path.

# Tool Use
- For greetings, chit-chat, and questions about things already visible in this conversation, respond with plain text. Do NOT call tools for simple chat.
- Call tools WITHOUT being asked when the answer depends on facts the user will act on and you cannot verify from memory: material properties, knowledge-graph contents, platform/job/deployment state, prices, availability.
- For explicit operations (deploy, ingest, run workflow, compute), use the matching tool.

# Knowing Your Limits
You may be running as a small local model. The harness compensates for that only if you follow these rules:
- Treat your parametric memory as a sketch, not a reference. The things you "remember" most confidently — numeric values, formulas, citations, API names — are exactly the things most likely to be wrong.
- You may not answer a scientific or platform question from memory. Retrieve first. If you must answer from memory anyway, label it: "from model memory, unverified".
- CITE OR ABSTAIN: a factual claim about materials, sources, or platform state carries the tool result it came from, or it does not go in the answer.
- EMPTY RETRIEVAL IS AN ANSWER. If the searches came back with nothing, say "not found in the knowledge graph" and stop there. Zero results never licenses filling the gap from memory.
- Say "I don't know" or "I could not verify this" plainly when tools fail or return nothing. An honest gap beats a fluent guess — this system is used for aerospace work where a wrong number is expensive.
- Never invent: tool names, tool output you did not receive, knowledge-graph entities, citations or DOIs, or more numeric precision than your source gave you.
- When arithmetic matters, run it with the python tool and show the code — do not do multi-step arithmetic in your head.
- State confidence with its basis: "the knowledge graph returned this from 3 sources" is different from "commonly reported as ~X, unverified" — make which one it is explicit.

# Result Quality
- Cite providers, data sources, and workflow boundaries when they materially affect the answer.
- Do not hallucinate materials properties, deployment state, job state, or command outcomes.
- If a platform capability appears unavailable or unhealthy, say so and adapt.
"#;

const AUTONOMOUS_PROMPT: &str = r#"You are PRISM, an autonomous agent for materials research, software engineering, and PRISM platform operations.

# Execution Contract
- You are an execution agent, not an advice-only assistant. Produce the requested OUTCOME with the tools. Do not substitute a description for the act: fix is not explain-the-fix, run-the-tests is not predict-their-results, create is not outline, search is not suggest-search-terms, ingest is not describe-how-to-ingest.
- Every task: name the deliverable, name the observations or artifacts it needs, get them with tools, check the result against the original request, then answer.
- EVIDENCE: do not state that you inspected, ran, tested, searched, edited, deployed, or verified anything unless a tool result for it exists in THIS run. No tool result, no claim — name what you did not check instead.
- ACT FIRST: take the first required tool action before writing prose about it. A short status after an observation beats a paragraph of intent before one.
- PROMOTE: an INDETERMINATE (red) result is a to-do, not an answer. Every tool that stamps one names what would raise it (a source to cite, a datasheet to ingest, a computation to run, a re-verification). Take that step when it is within budget; when it is not, state the exact step and its cost so the user can. Never present a red number as a finding without saying what would make it green.
- READ: an abstract is a lead, not evidence. Before a paper supports a claim, read it — `papers_fulltext` (free; nothing stored) or `papers_ingest` (stores its claims) — and cite the passage. A claim that rests on an abstract alone says so.
- KEEP WORKING: a failure count (the failure ledger, a tool error, an empty search) is information about the path, never a reason to end the turn. Materials science is not solved; the run keeps working — a different source, a different tool, a narrower question, a computation instead of a lookup — until the goal is met or the budget is spent, and it says which.
- BUDGET: an empty or failed result is not a stopping point. Reformulate, drop a constraint, go straight to an authoritative source, try adjacent terminology. Report failure only after at least three materially different attempts, and say what each one was.
- You may not stop because the task looks straightforward, because you think you already know the answer, because a tool call is extra work, because the first attempt failed, or because you could tell the user how to do it themselves.
- TOOL RISK IS NOT UNIFORM: read-only tools (search, read, inspect, list, status, calculate, sandbox test) — use them aggressively, without asking. Reversible writes (workspace edits, local branches, drafts) — do them and keep them revertible. Irreversible or external actions (deploy, publish, delete, spend, send) — confirm first. Hesitating on a read-only tool is a failure, not caution.
- FIXED SEQUENCES, no skipped steps: for a bug — inspect, reproduce, localize, patch, test, review the diff. For research — decompose, search primary sources, extract claims, cross-check, synthesize, cite. For data — inspect the schema, validate, compute, sanity-check, summarize. You choose the content of each step; you do not get to drop one.

# System
- All text you output outside of tool use becomes part of the run log or user-visible result.
- Tools run under permission and policy rules. If a tool is blocked, adapt instead of retrying the same call blindly.
- Tool results or user messages may include <system-reminder> tags or other system tags. Treat them as instructions from the runtime.
- The conversation may be compacted automatically. Preserve critical assumptions in your own reasoning as the task evolves.

# Working Style
- Read relevant code, data, or workflow definitions before changing them.
- Prefer modifying existing files, workflows, and command surfaces over creating parallel paths.
- Do not add features, refactors, or abstractions beyond what the task requires.
- Diagnose failures before switching tactics.

# Execution Discipline
You are an execution agent, not an advice-only assistant. The user asks for an outcome; your job is to produce it, not to describe how they could.
- Deliverable first: identify the outcome requested, then the observations or artifacts it requires, then use the capabilities that produce them, verify against the request, then answer.
- Act first, narrate second. For operational tasks, perform the first required action before explaining intent.
- No task substitution. Match the requested verb. "Fix" means fix, not explain how to fix. "Run" means run, not predict the result. "Create" means create, not outline. When the user asked for an action or artifact, instructions and plans do not count as completion.
- Mandatory capabilities. A task that needs current external information (state, prices, availability, live data), execution, or file/repo inspection may NOT be answered from memory — use the matching capability. You choose the arguments; you do not choose whether to skip the capability.
- Evidence, not claims. Do not state that you inspected, read, ran, tested, searched, or verified anything unless a real tool result for it exists in this run.
- Read the real result. A result reports its own state — success or failure, exit code, error text, empty vs. populated. A masked "ok" or a non-zero exit is not completion. Act on failures: diagnose, retry, or take a materially different approach. A failed call is not evidence the thing is impossible.
- Work to a conclusion, not to the first obstacle. On empty or failed results, reformulate, drop a constraint, or try an adjacent capability or authoritative source before reporting you cannot. Do not stop because the task looks straightforward or you recall the answer.
- Tool policy by side-effect. Read-only inspection (search, fetch, list, calculate, sandbox-test) — use aggressively and autonomously. Reversible writes (local edits, drafts, staging) — execute, then verify, rolling back if wrong. Irreversible or external actions (send, delete, deploy to production, publish, purchase) — confirm first. Treat tools by their real blast radius, not all as equally dangerous.
- Keep context lean. Ground truth lives in files, the graph, and the store — inspect the specific symbol, section, or entity you need, not whole files or full logs. Re-fetch to verify rather than holding large dumps in context.

# Planning And Execution
- For multi-step work, state a short plan after the first read-only observation, not before it.
- If the request is underspecified, make reasonable assumptions and state them explicitly before proceeding. There is nobody to ask on this path, and a deterministic pre-flight already screened the request.
- If the runtime hands you a PRE-FLIGHT ROUTING line, it is the classified intent of this request. Honour it. When it says a capability does not exist, say so plainly — never substitute a web search presented as a materials-science answer.
- In plan mode, focus on sequencing, constraints, and implementation shape rather than execution.

# Coding Workflow
- Inspect before editing.
- Prefer direct file tools for file work and shell tools for search, build, test, and git flows.
- Use targeted edits when possible. Use whole-file writes only when replacing or creating a file body is the right move.
- Prefer PRISM-native command tools over shelling out when a PRISM command already covers the operation.
- Keep changes small, coherent, and easy to verify.

# PRISM Workflow
- Treat workflows as the primary orchestration surface for YAML-defined pipelines.
- Use query for targeted retrieval, research_query for iterative retrieval-and-synthesis loops, and discourse_read/discourse_write for structured multi-agent debate.
- For DEEP research that would take minutes, use start_background_research (a separate platform agent works while you keep helping the user) and collect the result later with check_background_research — do not block the conversation on the synchronous research tool for big questions, and do not busy-poll.
- Use models_read to discover available hosted LLMs instead of assuming model names.
- Use marketplace_read when a workflow, tool, or artifact may need to be discovered, and marketplace_write to install it, before execution.
- Use deploy_read/deploy_write for persistent serving or target-based deployment rather than ad hoc shell processes.
- Use node to inspect or prepare local capability, and use mesh_read/mesh_write for discovery/publication/subscription between nodes.
- Use ingest as one end-to-end command. Do not split extraction, embedding, and graph loading into separate user-facing steps unless low-level control is explicitly required by the task.
- Use find_tools to discover tools, agent_capabilities to inspect providers/models/connectivity, and status/tools for the local environment before planning.
- ACQUIRING NEW TOOLS: when no loaded tool fits, follow discover -> install -> connect -> verify: find_tools first (already have it?), then marketplace_read (action search or info), then marketplace_write (action install; lands in ~/.prism/tools or ~/.prism/workflows; never overwrites local edits). Installed workflows are runnable immediately; installed Python tools load at the NEXT tool-server start — say so honestly, and verify the new name appears in tools before claiming it is callable. Full playbook: read docs/TOOL_ACQUISITION.md in the PRISM repo (file, action='read') when you need the complete procedure, publishing steps, or the anti-spoof/approval rules.
- Keep local, platform-hosted, and BYOC boundaries explicit in your reasoning when you choose a compute or storage path.

# Tool Use
- For greetings, chit-chat, and questions about things already visible in this conversation, respond with plain text. Do NOT call tools for simple chat.
- Call tools WITHOUT being asked when the answer depends on facts the user will act on and you cannot verify from memory: material properties, knowledge-graph contents, platform/job/deployment state, prices, availability.
- For explicit operations (deploy, ingest, run workflow, compute), use the matching tool.

# Knowing Your Limits
You may be running as a small local model. The harness compensates for that only if you follow these rules:
- Treat your parametric memory as a sketch, not a reference. The things you "remember" most confidently — numeric values, formulas, citations, API names — are exactly the things most likely to be wrong.
- You may not answer a scientific or platform question from memory. Retrieve first. If you must answer from memory anyway, label it: "from model memory, unverified".
- CITE OR ABSTAIN: a factual claim about materials, sources, or platform state carries the tool result it came from, or it does not go in the answer.
- EMPTY RETRIEVAL IS AN ANSWER. If the searches came back with nothing, say "not found in the knowledge graph" and stop there. Zero results never licenses filling the gap from memory.
- Say "I don't know" or "I could not verify this" plainly when tools fail or return nothing. An honest gap beats a fluent guess — this system is used for aerospace work where a wrong number is expensive.
- Never invent: tool names, tool output you did not receive, knowledge-graph entities, citations or DOIs, or more numeric precision than your source gave you.
- When arithmetic matters, run it with the python tool and show the code — do not do multi-step arithmetic in your head.
- State confidence with its basis: "the knowledge graph returned this from 3 sources" is different from "commonly reported as ~X, unverified" — make which one it is explicit.

# Result Quality
- Cite providers, data sources, and workflow boundaries when they materially affect the answer.
- Do not hallucinate materials properties, deployment state, job state, or command outcomes.
- If a platform capability appears unavailable or unhealthy, say so and adapt.
"#;

/// The default system prompt (interactive mode). Kept for backward
/// compatibility with code that referenced `SYSTEM_PROMPT`.
pub const SYSTEM_PROMPT: &str = INTERACTIVE_PROMPT;

// ---------------------------------------------------------------------------
// Profile-aware rendering — the "fluid mechanism".
//
// The canonical prompts above are authored in Markdown-header form. Every
// `PromptProfile` style is produced by *transforming* that single source, so:
//   - MarkdownHeaders + Full is byte-for-byte the canonical text (zero drift),
//   - XmlTags / PlainImperative rewrite only the section *delimiters* — the
//     section *bodies* are never edited, so no content is lost across styles,
//   - Compact drops a small set of nice-to-have sections.
// This keeps one source of truth and removes any transcription-drift risk.
// ---------------------------------------------------------------------------

/// Nice-to-have sections dropped under a `Compact` length budget. `Result
/// Quality` is the most redundant for weak/local models — its no-hallucination
/// guidance is already covered, more forcefully, by `Knowing Your Limits`.
const COMPACT_DROP_SECTIONS: &[&str] = &["Result Quality"];

/// A short chain-of-thought nudge appended only under `ReasoningMode::PromptedCoT`
/// (models without native thinking). Rendered in the profile's structure style.
/// The nudge shapes the ORDER of reasoning and deliberately sets no length
/// ceiling on it: the harness never caps how long a model may think (owner
/// rule — a reasoning budget is the operator's choice, not the harness's).
/// "Do not pad the answer" governs the answer, not the reasoning.
const COT_TITLE: &str = "Reasoning";
const COT_BODY: &str = "Think step by step before acting. State what the user needs, which tool fits, and what could go wrong, then take a single concrete action. Do not pad the answer.";

/// A parsed section of a canonical prompt. `title == None` is the pre-header
/// preamble (the identity line). `body` carries no trailing blank lines.
struct PromptBlock {
    title: Option<String>,
    body: String,
}

/// Split a canonical Markdown-header prompt into ordered blocks. The text
/// before the first `# ` header is the preamble; each `# Title` starts a new
/// block whose body runs until the next header, trailing blank lines trimmed.
fn split_into_blocks(canonical: &str) -> Vec<PromptBlock> {
    let mut blocks: Vec<PromptBlock> = Vec::new();
    let mut title: Option<String> = None;
    let mut body: Vec<&str> = Vec::new();

    let flush = |blocks: &mut Vec<PromptBlock>, title: &Option<String>, body: &[&str]| {
        let mut end = body.len();
        while end > 0 && body[end - 1].is_empty() {
            end -= 1;
        }
        // Skip an empty leading preamble (a prompt that opens with a header).
        if title.is_none() && end == 0 {
            return;
        }
        blocks.push(PromptBlock {
            title: title.clone(),
            body: body[..end].join("\n"),
        });
    };

    for line in canonical.lines() {
        if let Some(rest) = line.strip_prefix("# ") {
            flush(&mut blocks, &title, &body);
            title = Some(rest.to_string());
            body.clear();
        } else {
            body.push(line);
        }
    }
    flush(&mut blocks, &title, &body);
    blocks
}

/// Render one titled section in the given structure style.
fn render_one_section(title: &str, body: &str, style: StructureStyle) -> String {
    match style {
        StructureStyle::XmlTags => {
            let tag = xml_tag(title);
            format!("<{tag}>\n{body}\n</{tag}>")
        }
        StructureStyle::MarkdownHeaders => format!("# {title}\n{body}"),
        StructureStyle::PlainImperative => format!("{title}\n{body}"),
    }
}

/// `Working Style` -> `working_style`.
fn xml_tag(title: &str) -> String {
    title.to_ascii_lowercase().replace(' ', "_")
}

fn render_blocks(blocks: &[PromptBlock], profile: &PromptProfile) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        match &block.title {
            None => parts.push(block.body.clone()),
            Some(title) => {
                if profile.length_budget == LengthBudget::Compact
                    && COMPACT_DROP_SECTIONS.contains(&title.as_str())
                {
                    continue;
                }
                parts.push(render_one_section(
                    title,
                    &block.body,
                    profile.structure_style,
                ));
            }
        }
    }
    parts.join("\n\n")
}

/// Render a canonical prompt for a specific model profile — the single entry
/// point the "fluid mechanism" flows through.
#[must_use]
pub fn render_system_prompt(canonical: &str, profile: &PromptProfile) -> String {
    // Fast path: the default style + full budget IS the canonical text. Returns
    // it verbatim so the default agent path provably cannot drift.
    let mut out = if profile.structure_style == StructureStyle::MarkdownHeaders
        && profile.length_budget == LengthBudget::Full
    {
        canonical.to_string()
    } else {
        render_blocks(&split_into_blocks(canonical), profile)
    };

    if profile.reasoning_invocation == ReasoningMode::PromptedCoT {
        out.push_str("\n\n");
        out.push_str(&render_one_section(
            COT_TITLE,
            COT_BODY,
            profile.structure_style,
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Runtime instruction-file discovery.
//
// A lab encodes its own conventions — preferred units, ontology choices, which
// sources it trusts — in an `AGENTS.md` checked into the project, the same way
// this repo's own root `AGENTS.md` carries contributor tooling notes. At agent
// startup PRISM walks from the working directory UPWARD to the git root,
// collecting any `AGENTS.md` it finds, and folds the text into the system
// prompt as a trailing "Project Instructions" section. Nearest file last, so
// the most specific instructions win. An optional `~/.prism/AGENTS.md` carries
// host-wide defaults and is treated as the most general (first) section.
//
// Context is a budget: an enormous file is truncated, and the truncation is
// stated in the injected text rather than silently cut. Files PRISM cannot
// read (non-UTF-8, unreadable) are REPORTED as warnings, never swallowed — a
// lab that wrote instructions PRISM cannot parse must find that out. A missing
// file is the normal case and adds nothing.
// ---------------------------------------------------------------------------

/// Filename discovered at each level of the upward walk.
const INSTRUCTION_FILE_NAME: &str = "AGENTS.md";
/// Marker that bounds the upward walk: a directory containing a `.git` entry
/// (file or directory) is the repository root, and discovery stops there.
const GIT_DIR: &str = ".git";

/// Policy for runtime instruction-file discovery.
///
/// All size ceilings live here rather than at call sites so operators reason
/// about one declared policy and a hidden magic number cannot creep in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstructionPolicy {
    /// Maximum bytes read from a single instruction file. A file larger than
    /// this is truncated to fit, and the injected text says so — it is never
    /// silently cut.
    pub max_bytes_per_file: usize,
    /// Hard cap on the total bytes of instruction text injected across every
    /// discovered file. Honoured after per-file truncation; if the assembled
    /// body still exceeds it, the body is tail-truncated and a marker is added.
    pub max_total_bytes: usize,
    /// Whether `~/.prism/AGENTS.md` is consulted after the upward walk, as the
    /// most general (first) section. Off disables host-wide instructions.
    pub enable_home_lookup: bool,
}

impl Default for InstructionPolicy {
    fn default() -> Self {
        Self {
            max_bytes_per_file: 32_768,
            max_total_bytes: 65_536,
            enable_home_lookup: true,
        }
    }
}

/// The result of runtime instruction-file discovery.
///
/// `text` is the assembled body (most-general-first, nearest-last), ready to
/// fold into the system prompt. `warnings` carries non-fatal problems — an
/// unreadable or non-UTF-8 file — that the caller should surface, since a lab
/// that wrote instructions PRISM cannot read must find that out. Empty `text`
/// with no warnings is the normal "no instruction file present" case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstructionDiscovery {
    /// Assembled instruction text, or empty when nothing was found.
    pub text: String,
    /// Non-fatal read problems (non-UTF-8, I/O error) worth reporting.
    pub warnings: Vec<String>,
}

/// One file's read outcome: usable content (with a truncation flag) or a
/// warning string explaining why it could not be used.
enum FileRead {
    Content { text: String, truncated: bool },
    Warning(String),
}

/// Discover and read project instruction files from `cwd` upward to the git
/// root, then optionally `~/.prism/AGENTS.md`.
///
/// The walk visits `cwd`, then each parent, and stops AFTER the first ancestor
/// that contains a `.git` entry — so the repository root's own `AGENTS.md` is
/// included and nothing above it is read. A `~/.prism/AGENTS.md` (when enabled
/// and present) is the most general section and is listed FIRST.
///
/// `AGENTS.md` symlinks are refused outright (never followed): this is the
/// strict reading of "never follow symlinks out of the tree", and a symlinked
/// instruction file is unusual enough that refusing it is safer than
/// canonicalizing and re-checking containment.
///
/// Never panics and never returns an error: a missing file, a missing home
/// directory, or an unreadable ancestor is simply not an instruction file.
#[must_use]
pub fn discover_instruction_files(cwd: &Path, policy: &InstructionPolicy) -> InstructionDiscovery {
    // Collect candidates in walk order (nearest first). The output is reversed
    // later so the nearest, most-specific file comes last.
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut dir: &Path = cwd;
    loop {
        let candidate = dir.join(INSTRUCTION_FILE_NAME);
        if candidate.is_file() {
            candidates.push(candidate);
        }
        if dir.join(GIT_DIR).exists() {
            break; // repository root: include it, stop above it
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break, // filesystem root with no `.git` — walk to the top
        }
    }
    if policy.enable_home_lookup
        && let Some(home) = dirs::home_dir()
    {
        let home_file = home.join(".prism").join(INSTRUCTION_FILE_NAME);
        if home_file.is_file() {
            candidates.push(home_file);
        }
    }

    // Most-general-first, nearest-last: home (if any) then repo-root ... cwd.
    candidates.reverse();

    // Read every candidate so unreadable / non-UTF-8 files are always reported,
    // even ones later dropped by the total budget.
    let mut sections: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for path in &candidates {
        let display = display_path(path, cwd);
        match read_one_instruction_file(path, &display, policy.max_bytes_per_file) {
            Some(FileRead::Content { text, truncated }) => {
                sections.push(format_file_section(
                    &display,
                    &text,
                    truncated,
                    policy.max_bytes_per_file,
                ));
            }
            Some(FileRead::Warning(message)) => warnings.push(message),
            None => {} // absent, non-regular, or a refused symlink
        }
    }

    let mut text = sections.join("\n\n");
    if text.len() > policy.max_total_bytes {
        // Tail-truncate the assembled body to the total budget. This is the
        // last-resort ceiling; the realistic "enormous AGENTS.md" case is
        // handled per-file above with its own marker. A tail cut may drop the
        // most-specific file, which is documented and rare under sane sizes.
        let cut = text.floor_char_boundary(policy.max_total_bytes);
        text.truncate(cut);
        text.push_str(&format!(
            "\n\n[PRISM: combined project instructions exceed the total budget \
             of {budget} bytes and were truncated; later, more-specific sections \
             may be missing.]",
            budget = policy.max_total_bytes
        ));
    }

    InstructionDiscovery { text, warnings }
}

/// Fold discovered instruction text into a base system prompt.
///
/// When `discovery.text` is empty the base is returned byte-for-byte: the
/// common "no instruction file" case provably does not alter today's prompt.
/// Otherwise the text is appended as a trailing "Project Instructions" section.
#[must_use]
pub fn inject_project_instructions(base: &str, discovery: &InstructionDiscovery) -> String {
    if discovery.text.is_empty() {
        return base.to_string();
    }
    format!(
        "{base}\n\n# Project Instructions\n\n{preamble}\n\n{body}",
        preamble = INJECTION_PREAMBLE,
        body = discovery.text,
    )
}

/// One-line preamble explaining the section's ordering to the model.
const INJECTION_PREAMBLE: &str = "The following project instruction files were \
discovered from the working directory up to the git root, and optionally \
`~/.prism/AGENTS.md`. They are listed most-general-first; LATER sections are \
more specific and take precedence over earlier ones.";

/// Read one instruction file, honouring the per-file byte cap and refusing
/// symlinks. Returns `None` for an absent / non-regular / symlinked path,
/// `Warning` for a file that exists but cannot be parsed, and `Content`
/// (with a `truncated` flag) for a usable file.
fn read_one_instruction_file(path: &Path, display: &str, max_bytes: usize) -> Option<FileRead> {
    // `symlink_metadata` does not follow symlinks, so a symlinked AGENTS.md
    // is detected and refused here rather than resolved out of the tree.
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() {
        return None;
    }
    if !metadata.is_file() {
        return None;
    }

    // Cap the read at max_bytes + 1: the +1 byte lets us detect "oversized"
    // without reading a multi-gigabyte file into memory.
    let cap = max_bytes.saturating_add(1);
    let bytes = match read_capped(path, cap) {
        Ok(b) => b,
        Err(err) => {
            return Some(FileRead::Warning(format!(
                "could not read instruction file {display}: {err}"
            )));
        }
    };

    let oversized = bytes.len() > max_bytes;
    // Validate UTF-8 on the bytes we actually read. A non-UTF-8 file is
    // reported, never lossily coerced into the prompt.
    let validated = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => {
            return Some(FileRead::Warning(format!(
                "instruction file {display} is not valid UTF-8 and was skipped"
            )));
        }
    };
    let (text, truncated) = if oversized {
        let cut = validated.floor_char_boundary(max_bytes);
        (validated[..cut].to_string(), true)
    } else {
        (validated.to_string(), false)
    };
    Some(FileRead::Content { text, truncated })
}

/// Read up to `cap` bytes from `path`.
fn read_capped(path: &Path, cap: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let file = fs::File::open(path)?;
    let mut buf = Vec::with_capacity(cap.min(64 * 1024));
    file.take(cap as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Render one file's contribution as a titled, self-describing section.
fn format_file_section(display: &str, text: &str, truncated: bool, budget: usize) -> String {
    let mut section = format!("## {display}\n\n{text}");
    if truncated {
        section.push_str(&format!(
            "\n\n[PRISM: {display} exceeds the per-file budget of {budget} bytes \
             and was truncated; the remainder was not read.]"
        ));
    }
    section
}

/// Human-friendly path for a discovered file: `~/.prism/AGENTS.md` for the home
/// file, a path relative to `cwd` for in-tree files, and the absolute path
/// otherwise.
fn display_path(path: &Path, cwd: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && path.starts_with(&home)
    {
        return path
            .strip_prefix(&home)
            .map(|rest| format!("~/{}", rest.display()))
            .unwrap_or_else(|_| path.display().to_string());
    }
    if let Ok(rel) = path.strip_prefix(cwd) {
        if rel.as_os_str().is_empty() {
            return INSTRUCTION_FILE_NAME.to_string();
        }
        return rel.display().to_string();
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::PermissionMode;
    use crate::prompt_profile::{LengthBudget, MaxTokensPolicy, ToolSurface, profile_for_model};
    use crate::tool_catalog::{LoadedTool, ToolCatalog};
    use serde_json::json;

    fn markdown_full() -> PromptProfile {
        PromptProfile {
            structure_style: StructureStyle::MarkdownHeaders,
            length_budget: LengthBudget::Full,
            tool_surface: ToolSurface::All,
            reasoning_invocation: ReasoningMode::None,
            max_tokens_policy: MaxTokensPolicy::ModelMax,
        }
    }

    /// Non-negotiable: the default style + full budget reproduces today's prompt
    /// byte-for-byte, so the default agent path is provably unchanged.
    #[test]
    fn markdown_full_is_byte_for_byte_canonical() {
        assert_eq!(
            render_system_prompt(INTERACTIVE_PROMPT, &markdown_full()),
            INTERACTIVE_PROMPT
        );
    }

    /// The section decomposition is faithful: rebuilding Markdown from the parsed
    /// blocks reproduces the canonical text (modulo the literal's trailing newline).
    #[test]
    fn block_decomposition_roundtrips() {
        let blocks = split_into_blocks(INTERACTIVE_PROMPT);
        let rebuilt = render_blocks(&blocks, &markdown_full());
        assert_eq!(rebuilt, INTERACTIVE_PROMPT.trim_end_matches('\n'));
    }

    /// XML rendering wraps every section in tags and loses no body content.
    #[test]
    fn xml_render_wraps_sections_and_preserves_bodies() {
        let profile = profile_for_model("claude-opus-4-6");
        assert_eq!(profile.structure_style, StructureStyle::XmlTags);
        let xml = render_system_prompt(INTERACTIVE_PROMPT, &profile);
        assert!(xml.contains("<working_style>"));
        assert!(xml.contains("</working_style>"));
        assert!(xml.contains("<knowing_your_limits>"));
        assert!(!xml.contains("# System"));
        // Every non-header, non-blank line of the canonical prompt survives.
        for line in INTERACTIVE_PROMPT.lines() {
            if line.is_empty() || line.starts_with("# ") {
                continue;
            }
            assert!(xml.contains(line), "XML render dropped line: {line}");
        }
    }

    /// Plain rendering strips both header markers and tags but keeps body lines.
    #[test]
    fn plain_render_flattens_but_preserves_bodies() {
        let profile = profile_for_model("some-local-model-7b");
        assert_eq!(profile.structure_style, StructureStyle::PlainImperative);
        let plain = render_system_prompt(INTERACTIVE_PROMPT, &profile);
        assert!(!plain.contains("# System"));
        assert!(!plain.contains("<working_style>"));
        assert!(plain.contains("You are PRISM"));
    }

    /// Compact budget drops the designated nice-to-have section.
    #[test]
    fn compact_drops_result_quality() {
        let mut profile = markdown_full();
        profile.length_budget = LengthBudget::Compact;
        let compact = render_system_prompt(INTERACTIVE_PROMPT, &profile);
        assert!(!compact.contains("Result Quality"));
        // But keeps the safety-critical section.
        assert!(compact.contains("Knowing Your Limits"));
    }

    /// PromptedCoT appends a reasoning nudge; other modes do not. The nudge
    /// may shape the order of reasoning but never its length — "In one or two
    /// lines" was removed as a harness-imposed reasoning cap (a muzzle), and
    /// this pins it out.
    #[test]
    fn prompted_cot_appends_reasoning_section() {
        let unknown = profile_for_model("some-local-model-7b");
        assert_eq!(unknown.reasoning_invocation, ReasoningMode::PromptedCoT);
        let with_cot = render_system_prompt(INTERACTIVE_PROMPT, &unknown);
        assert!(with_cot.contains("Think step by step before acting"));
        assert!(
            !with_cot.contains("In one or two lines") && !with_cot.contains("Reason briefly"),
            "the CoT nudge reinstated a cap on reasoning length"
        );

        let no_cot = render_system_prompt(INTERACTIVE_PROMPT, &markdown_full());
        assert!(!no_cot.contains("Think step by step before acting"));
    }

    /// The static PRISM Workflow sections must speak the offered tool surface.
    /// After the read/write collapse the old typed names (models_list,
    /// marketplace_search, ...) and the `list_tools` RPC method are not
    /// callable by the model; a prompt naming them sends the model at tools it
    /// cannot see.
    #[test]
    fn static_prompts_name_only_offered_tools() {
        for (label, prompt) in [
            ("interactive", INTERACTIVE_PROMPT),
            ("autonomous", AUTONOMOUS_PROMPT),
        ] {
            for stale in [
                "marketplace_search",
                "marketplace_info",
                "marketplace_install",
                "models_list",
                "list_tools",
            ] {
                assert!(
                    !prompt.contains(stale),
                    "{label} prompt names `{stale}`, which is not offered to the model"
                );
            }
            for current in [
                "marketplace_read",
                "marketplace_write",
                "models_read",
                "research_query",
            ] {
                assert!(
                    prompt.contains(current),
                    "{label} prompt lost the offered tool name `{current}`"
                );
            }
        }
    }

    /// The load-bearing clauses of the Agent Execution Contract. If a prompt
    /// rewrite drops one of these, the contract stops being enforced and the
    /// tests stay green — exactly the drift class this pin exists to catch.
    /// Substrings are chosen to be the *distinctive* fragment of each rule, not
    /// whole sentences, so wording can be tuned without a false alarm.
    const CONTRACT_CLAUSES: &[(&str, &str)] = &[
        (
            "execution agent, not an advice-only assistant",
            "frames the agent as execution, not advice",
        ),
        (
            "fix is not explain-the-fix",
            "anti-task-substitution — the contract's biggest failure mode",
        ),
        (
            "run-the-tests is not predict-their-results",
            "anti-task-substitution — second canonical example",
        ),
        (
            "unless a tool result for it exists in THIS run",
            "evidence ledger — no claim without a tool trace in this run",
        ),
        ("ACT FIRST", "act-first-narrate-second"),
        (
            "three materially different attempts",
            "work budget — the >=3-attempts rule before reporting failure",
        ),
        (
            "TOOL RISK IS NOT UNIFORM",
            "read-only / reversible / irreversible tool policy",
        ),
        (
            "FIXED SEQUENCES, no skipped steps",
            "deterministic workflows — the model fills the steps, it does not drop them",
        ),
        (
            "Hesitating on a read-only tool is a failure",
            "prevents generalized timidity from the tool policy",
        ),
        (
            "You may not answer a scientific or platform question from memory",
            "retrieval discipline — no answering from memory",
        ),
        ("CITE OR ABSTAIN", "cite-or-abstain"),
        (
            "EMPTY RETRIEVAL IS AN ANSWER",
            "abstain on empty retrieval — never fabricate to fill the gap",
        ),
    ];

    /// Both canonical prompts carry every contract clause. The autonomous
    /// prompt is a separate literal, so this is the only thing stopping the two
    /// from drifting apart.
    #[test]
    fn both_prompts_promote_red_results_and_keep_working() {
        for prompt in [INTERACTIVE_PROMPT, AUTONOMOUS_PROMPT] {
            assert!(
                prompt.contains("- PROMOTE: an INDETERMINATE (red) result is a to-do"),
                "promotion directive"
            );
            assert!(
                prompt.contains("- KEEP WORKING: a failure count"),
                "{prompt} must tell the model to keep working"
            );
            assert!(
                prompt.contains("- READ: an abstract is a lead, not evidence"),
                "failures are a path, not an end"
            );
        }
    }

    #[test]
    fn both_prompts_carry_every_contract_clause() {
        for (label, prompt) in [
            ("interactive", INTERACTIVE_PROMPT),
            ("autonomous", AUTONOMOUS_PROMPT),
        ] {
            assert!(!prompt.trim().is_empty(), "{label} prompt is empty");
            for (clause, why) in CONTRACT_CLAUSES {
                assert!(
                    prompt.contains(clause),
                    "{label} prompt lost contract clause `{clause}` ({why})"
                );
            }
        }
    }

    /// The contract must survive every rendering path a live model can hit:
    /// all three structure styles and the Compact budget. A rule that is
    /// dropped for small local models is a rule those models do not have.
    #[test]
    fn contract_survives_every_render_profile() {
        let profiles = [
            profile_for_model("claude-opus-4-6"),     // XmlTags
            profile_for_model("some-local-model-7b"), // PlainImperative
            markdown_full(),
            PromptProfile {
                length_budget: LengthBudget::Compact,
                ..markdown_full()
            },
        ];
        for profile in profiles {
            let rendered = render_system_prompt(INTERACTIVE_PROMPT, &profile);
            for (clause, why) in CONTRACT_CLAUSES {
                assert!(
                    rendered.contains(clause),
                    "render profile {profile:?} dropped `{clause}` ({why})"
                );
            }
        }
    }

    /// Act-first and "plan before acting" are contradictory instructions. The
    /// rewrite resolved that in favour of act-first; this pins the resolution
    /// so a future edit cannot quietly reinstate the contradiction.
    #[test]
    fn planning_does_not_contradict_act_first() {
        for prompt in [INTERACTIVE_PROMPT, AUTONOMOUS_PROMPT] {
            assert!(
                !prompt.contains("plan before acting"),
                "prompt reinstated plan-before-acting, contradicting ACT FIRST"
            );
            assert!(prompt.contains("after the first read-only observation"));
        }
    }

    #[test]
    fn interactive_prompt_contains_interactive_guidance() {
        let prompt = build_system_prompt(true);
        assert!(prompt.contains("interactive agent"));
        assert!(prompt.contains("wait for approval"));
    }

    /// One clarification policy, not two. Asking is owned by the deterministic
    /// pre-flight (`crate::reprompt`); the prompt's job is to stop the model
    /// adding a SECOND round of questions on top of it. If this ever reverts to
    /// "ask one concrete question at a time", the two policies are back in
    /// conflict and experts get interrogated after the pre-flight let them
    /// through.
    #[test]
    fn neither_prompt_invites_the_model_to_ask_its_own_questions() {
        for interactive in [true, false] {
            let prompt = build_system_prompt(interactive);
            assert!(
                !prompt.contains("ask one concrete question at a time"),
                "prompt reinstated a second clarification policy (interactive={interactive})"
            );
            assert!(
                prompt.contains("PRE-FLIGHT ROUTING"),
                "prompt must tell the model what to do with the routing hint \
                 (interactive={interactive})"
            );
        }
        assert!(build_system_prompt(true).contains("DO NOT ASK CLARIFYING QUESTIONS"));
    }

    #[test]
    fn autonomous_prompt_contains_assumption_guidance() {
        let prompt = build_system_prompt(false);
        assert!(prompt.contains("autonomous agent"));
        assert!(prompt.contains("make reasonable assumptions"));
        assert!(!prompt.contains("wait for approval"));
    }

    /// Pin the Execution Contract so a future prompt rewrite can't silently
    /// drop the structural anti-laziness rules (owner directive 2026-07-24).
    ///
    /// The agent's failure mode these rules target is "talking about doing the
    /// work instead of doing it" — a plausible answer with no tool result
    /// behind it. That is invisible to a runtime gate because no tool ran; the
    /// only thing telling the model not to take that shortcut is this prompt
    /// text. Strip it and the model quietly reverts to advice-only answers
    /// with green tests. The fix is mode-independent, so the same markers must
    /// survive in BOTH the interactive and autonomous prompts.
    #[test]
    fn prompts_bake_execution_contract() {
        for interactive in [true, false] {
            let label = if interactive {
                "interactive"
            } else {
                "autonomous"
            };
            let prompt = build_system_prompt(interactive);
            // Mode-independent contract — must be identical across both prompts
            // so the two paths can never drift on the structural rules.
            for marker in [
                "# Execution Discipline",
                "execution agent, not an advice-only assistant",
                "Deliverable first",
                "Act first, narrate second",
                "No task substitution",
                "Mandatory capabilities",
                "Evidence, not claims",
                "Read the real result",
                "Work to a conclusion, not to the first obstacle",
                "Tool policy by side-effect",
                "Keep context lean",
            ] {
                assert!(
                    prompt.contains(marker),
                    "Execution Contract marker `{marker}` missing from the {label} \
                     prompt — the structural anti-laziness rules have regressed. \
                     If you intentionally removed them, update this test."
                );
            }
        }
    }

    #[test]
    fn runtime_guidance_mentions_loaded_workflows() {
        let mut catalog = ToolCatalog::default();
        catalog.extend(vec![
            LoadedTool {
                name: "workflow_run".to_string(),
                description: "Run a workflow".to_string(),
                input_schema: json!({ "type": "object" }),
                requires_approval: true,
                declared_free: false,
                permission_mode: PermissionMode::WorkspaceWrite,
                source: None,
                source_detail: None,
            },
            LoadedTool {
                name: "agent_capabilities".to_string(),
                description: "Inspect capabilities".to_string(),
                input_schema: json!({ "type": "object" }),
                requires_approval: false,
                declared_free: true,
                permission_mode: PermissionMode::ReadOnly,
                source: None,
                source_detail: None,
            },
        ]);

        let prompt = append_runtime_tool_guidance(SYSTEM_PROMPT, &catalog, &markdown_full());
        assert!(prompt.contains("# Loaded Tool Strategy"));
        assert!(prompt.contains("Treat workflows as the primary orchestration surface"));
        assert!(prompt.contains("agent_capabilities"));
    }

    /// Updated for the read/write collapse (mesh 9 -> 2, marketplace -> 2,
    /// node 4 -> 2, see `command_tools::COLLAPSED_INTO_ACTION_TOOLS`): the
    /// offered catalog carries `mesh_write` / `marketplace_read` / `node_read`,
    /// never the old typed names, so the guidance must key on — and speak —
    /// the collapsed surface.
    #[test]
    fn runtime_guidance_mentions_node_mesh_and_marketplace() {
        let mut catalog = ToolCatalog::default();
        catalog.extend(vec![
            LoadedTool {
                name: "node_read".to_string(),
                description: "Inspect node".to_string(),
                input_schema: json!({ "type": "object" }),
                requires_approval: false,
                declared_free: true,
                permission_mode: PermissionMode::ReadOnly,
                source: None,
                source_detail: None,
            },
            LoadedTool {
                name: "mesh_write".to_string(),
                description: "Publish to mesh".to_string(),
                input_schema: json!({ "type": "object" }),
                requires_approval: true,
                declared_free: false,
                permission_mode: PermissionMode::FullAccess,
                source: None,
                source_detail: None,
            },
            LoadedTool {
                name: "marketplace_read".to_string(),
                description: "Search marketplace".to_string(),
                input_schema: json!({ "type": "object" }),
                requires_approval: false,
                declared_free: true,
                permission_mode: PermissionMode::ReadOnly,
                source: None,
                source_detail: None,
            },
        ]);

        let prompt = append_runtime_tool_guidance(SYSTEM_PROMPT, &catalog, &markdown_full());
        assert!(prompt.contains("Use `node_read` to check what this machine can do"));
        assert!(prompt.contains("Use `marketplace_read` to search or inspect"));
    }

    /// The tool families were collapsed into read/write pairs
    /// (`command_tools::COLLAPSED_INTO_ACTION_TOOLS`); the hidden typed names
    /// never appear in an offered catalog. Guidance keyed on those names was
    /// dead — the discourse/models/deploy bullets stopped rendering entirely.
    /// This pins that each bullet fires from the collapsed names, and that the
    /// rendered guidance names no tool the model cannot see.
    #[test]
    fn runtime_guidance_keys_on_the_collapsed_read_write_surface() {
        let mut catalog = ToolCatalog::default();
        catalog.extend(
            [
                "models_read",
                "discourse_read",
                "discourse_write",
                "deploy_read",
                "deploy_write",
                "mesh_read",
                "mesh_write",
                "marketplace_read",
                "marketplace_write",
            ]
            .into_iter()
            .map(|name| LoadedTool {
                name: name.to_string(),
                description: name.to_string(),
                input_schema: json!({ "type": "object" }),
                requires_approval: false,
                declared_free: true,
                permission_mode: PermissionMode::ReadOnly,
                source: None,
                source_detail: None,
            })
            .collect::<Vec<_>>(),
        );

        let prompt = append_runtime_tool_guidance(SYSTEM_PROMPT, &catalog, &markdown_full());
        for offered in [
            "`models_read`",
            "`discourse_read` and `discourse_write`",
            "`deploy_read` and `deploy_write`",
            "`mesh_read` and `mesh_write`",
            "`marketplace_read`",
        ] {
            assert!(
                prompt.contains(offered),
                "guidance must name the offered tool: {offered}"
            );
        }
        let guidance = prompt
            .split("# Loaded Tool Strategy")
            .nth(1)
            .expect("guidance section rendered");
        for hidden in [
            "models_list",
            "discourse_create",
            "deploy_create",
            "mesh_publish",
            "marketplace_search",
            "marketplace_install",
        ] {
            assert!(
                !guidance.contains(hidden),
                "guidance names `{hidden}`, which is no longer offered"
            );
        }
    }

    #[test]
    fn runtime_guidance_stays_empty_for_empty_catalog() {
        let catalog = ToolCatalog::default();
        let prompt = append_runtime_tool_guidance(SYSTEM_PROMPT, &catalog, &markdown_full());
        assert_eq!(prompt, SYSTEM_PROMPT);
    }

    #[test]
    fn runtime_guidance_mentions_external_mcp_tools() {
        let mut catalog = ToolCatalog::default();
        catalog.extend(vec![LoadedTool {
            name: "atlas_lookup".to_string(),
            description: "Remote lookup".to_string(),
            input_schema: json!({ "type": "object" }),
            requires_approval: true,
            declared_free: false,
            permission_mode: PermissionMode::WorkspaceWrite,
            source: Some("mcp".to_string()),
            source_detail: Some("atlas".to_string()),
        }]);

        let prompt = append_runtime_tool_guidance(SYSTEM_PROMPT, &catalog, &markdown_full());
        assert!(prompt.contains("external MCP servers"));
    }

    #[test]
    fn runtime_guidance_mentions_report_bug_when_present() {
        // TASK 4: the agent gets a one-line neural hint to self-report
        // problems ONLY when report_bug is in the loaded catalog.
        let mut catalog = ToolCatalog::default();
        catalog.extend(vec![LoadedTool {
            name: "report_bug".to_string(),
            description: "File a bug report".to_string(),
            input_schema: json!({ "type": "object" }),
            requires_approval: true,
            declared_free: false,
            permission_mode: PermissionMode::FullAccess,
            source: None,
            source_detail: None,
        }]);

        let prompt = append_runtime_tool_guidance(SYSTEM_PROMPT, &catalog, &markdown_full());
        assert!(
            prompt.contains("report_bug"),
            "prompt must hint at report_bug when the tool is loaded"
        );
        // Lean — a single line, not a section.
        assert!(prompt.contains("call `report_bug`"));
    }

    #[test]
    fn runtime_guidance_omits_report_bug_when_absent() {
        // Without report_bug in the catalog, the hint must NOT appear — the
        // dynamic guidance stays aligned with the actual tool surface.
        let catalog = ToolCatalog::default();
        let prompt = append_runtime_tool_guidance(SYSTEM_PROMPT, &catalog, &markdown_full());
        assert!(!prompt.contains("report_bug"));
    }

    /// CONTRACT CHANGE (dehardcoding): this used to pin the HEA/alloy-design
    /// and materials-screening routing hints INSIDE the system prompt.
    /// That routing doctrine now lives in the tools' own descriptions
    /// (`app/tools/materials/hea.py`, `informatics.py`), which ride the
    /// tool surface itself — so a non-materials deployment's system prompt
    /// carries no materials routing. What must hold instead: the runtime
    /// guidance section stays SILENT about domain routing.
    #[test]
    fn runtime_guidance_carries_no_domain_routing_doctrine() {
        let mut catalog = ToolCatalog::default();
        catalog.extend(vec![
            LoadedTool {
                name: "hea_descriptors".to_string(),
                description: "HEA descriptors".to_string(),
                input_schema: json!({ "type": "object" }),
                requires_approval: false,
                declared_free: true,
                permission_mode: PermissionMode::WorkspaceWrite,
                source: None,
                source_detail: None,
            },
            LoadedTool {
                name: "phase_stability".to_string(),
                description: "Phase stability".to_string(),
                input_schema: json!({ "type": "object" }),
                requires_approval: false,
                declared_free: true,
                permission_mode: PermissionMode::ReadOnly,
                source: None,
                source_detail: None,
            },
            LoadedTool {
                name: "pareto_screen".to_string(),
                description: "Pareto screen".to_string(),
                input_schema: json!({ "type": "object" }),
                requires_approval: false,
                declared_free: true,
                permission_mode: PermissionMode::WorkspaceWrite,
                source: None,
                source_detail: None,
            },
        ]);
        let prompt = append_runtime_tool_guidance(SYSTEM_PROMPT, &catalog, &markdown_full());
        for banned in ["Thermo-Calc", "alloy-design", "multi-objective materials"] {
            assert!(
                !prompt.contains(banned),
                "domain routing {banned:?} leaked back into the runtime guidance — \
                 it belongs to the tools' own descriptions"
            );
        }
    }

    // ---- Runtime instruction-file discovery ------------------------------
    //
    // Every test pins a `.git` at the temp root so the upward walk cannot
    // escape into the shared system temp, and disables home lookup so the
    // developer's own `~/.prism/AGENTS.md` never leaks into results.

    fn test_policy() -> InstructionPolicy {
        InstructionPolicy {
            enable_home_lookup: false,
            ..InstructionPolicy::default()
        }
    }

    fn git_bound(dir: &std::path::Path) {
        std::fs::write(dir.join(".git"), "gitdir: nowhere").unwrap();
    }

    /// Non-negotiable: with no instruction file anywhere reachable, the prompt
    /// is byte-for-byte today's static base. Fails the instant discovery
    /// disturbs the empty case (e.g. appends an empty section header) or the
    /// static prompt text is edited.
    #[test]
    fn no_instructions_yields_byte_for_byte_prompt() {
        let dir = tempfile::TempDir::new().unwrap();
        git_bound(dir.path());

        let discovery = discover_instruction_files(dir.path(), &test_policy());
        assert!(discovery.text.is_empty());
        assert!(discovery.warnings.is_empty());

        let base = build_system_prompt(true);
        assert_eq!(inject_project_instructions(&base, &discovery), base);
    }

    /// A file sitting at the working directory is folded into the prompt.
    #[test]
    fn instruction_file_at_cwd_is_injected() {
        let dir = tempfile::TempDir::new().unwrap();
        git_bound(dir.path());
        std::fs::write(
            dir.path().join("AGENTS.md"),
            "Always cite MP ids verbatim.\n",
        )
        .unwrap();

        let d = discover_instruction_files(dir.path(), &test_policy());
        let prompt = inject_project_instructions(&build_system_prompt(true), &d);

        assert!(
            prompt.starts_with("You are PRISM"),
            "base prompt stays intact at the front"
        );
        assert!(prompt.contains("# Project Instructions"));
        assert!(prompt.contains("Always cite MP ids verbatim."));
    }

    /// Two levels: both files are included, nearest (most specific) LAST.
    #[test]
    fn two_levels_both_included_nearest_last() {
        let root = tempfile::TempDir::new().unwrap();
        git_bound(root.path());
        std::fs::write(root.path().join("AGENTS.md"), "GENERAL-ROOT\n").unwrap();
        let deep = root.path().join("sub");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("AGENTS.md"), "NEAREST-DEEP\n").unwrap();

        let d = discover_instruction_files(&deep, &test_policy());
        let prompt = inject_project_instructions(&build_system_prompt(true), &d);

        let general = prompt.find("GENERAL-ROOT").expect("general file present");
        let nearest = prompt.find("NEAREST-DEEP").expect("nearest file present");
        assert!(
            general < nearest,
            "general (repo root) must precede the nearest (most specific) file"
        );
    }

    /// Discovery stops at the git root: a file ABOVE it is never read.
    #[test]
    fn discovery_stops_at_git_root() {
        let outer = tempfile::TempDir::new().unwrap();
        // A file above the repo root — must not be read.
        std::fs::write(
            outer.path().join("AGENTS.md"),
            "ABOVE-ROOT-MUST-NOT-APPEAR\n",
        )
        .unwrap();
        let repo = outer.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_bound(&repo);
        let work = repo.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("AGENTS.md"), "INSIDE-REPO\n").unwrap();

        let d = discover_instruction_files(&work, &test_policy());
        let prompt = inject_project_instructions(&build_system_prompt(true), &d);

        assert!(prompt.contains("INSIDE-REPO"));
        assert!(
            !prompt.contains("ABOVE-ROOT-MUST-NOT-APPEAR"),
            "discovery read a file above the git root"
        );
    }

    /// An oversized file is truncated AND the injected text says so — never a
    /// silent cut.
    #[test]
    fn oversized_file_is_truncated_and_says_so() {
        let dir = tempfile::TempDir::new().unwrap();
        git_bound(dir.path());
        std::fs::write(dir.path().join("AGENTS.md"), "A".repeat(200)).unwrap();

        let policy = InstructionPolicy {
            max_bytes_per_file: 64,
            enable_home_lookup: false,
            ..InstructionPolicy::default()
        };
        let d = discover_instruction_files(dir.path(), &policy);
        let prompt = inject_project_instructions(&build_system_prompt(true), &d);

        assert!(
            prompt.contains("truncated"),
            "truncation must be stated in the injected text"
        );
        assert!(
            prompt.contains("64 bytes"),
            "marker must state the per-file budget"
        );
        // The first 64 bytes survive; the 65th onward do not.
        assert!(prompt.contains(&"A".repeat(64)));
        assert!(!prompt.contains(&"A".repeat(65)));
    }

    /// A non-UTF-8 file is REPORTED (a warning), not silently skipped, and its
    /// bytes are never lossily injected.
    #[test]
    fn non_utf8_file_is_reported_not_silent() {
        let dir = tempfile::TempDir::new().unwrap();
        git_bound(dir.path());
        let mut bytes = b"readable prefix ".to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFE, 0xC0, 0xC0]);
        std::fs::write(dir.path().join("AGENTS.md"), bytes).unwrap();

        let d = discover_instruction_files(dir.path(), &test_policy());
        assert!(
            !d.warnings.is_empty(),
            "a non-UTF-8 file must be reported, not silent"
        );
        assert!(
            d.warnings.iter().any(|w| w.contains("UTF-8")),
            "warning must mention UTF-8: {:?}",
            d.warnings
        );
        assert!(!d.text.contains("readable prefix"));
    }
}
