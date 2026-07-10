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

use crate::prompt_profile::{LengthBudget, PromptProfile, ReasoningMode, StructureStyle};
use crate::tool_catalog::ToolCatalog;

/// Build the full base system prompt for either interactive or autonomous mode.
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
pub fn append_runtime_tool_guidance(base_prompt: &str, tools: &ToolCatalog) -> String {
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

    if has_all_tools(
        &tool_names,
        &["read_file", "edit_file", "write_file", "execute_bash"],
    ) {
        bullets.push(
            "For code work, inspect first with `read_file`, prefer `edit_file` for targeted changes, reserve `write_file` for whole-file replacement or creation, and use `execute_bash` for search, build, test, and git flows."
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
            "Treat workflows as the primary orchestration surface for YAML-defined pipelines. Prefer typed workflow tools before falling back to the root `workflow` command wrapper."
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

    if has_any_tools(
        &tool_names,
        &[
            "discourse_create",
            "discourse_list",
            "discourse_run",
            "discourse_status",
        ],
    ) {
        bullets.push(
            "Use discourse for structured multi-agent debate or comparison runs. Prefer the typed discourse tools over manually assembling command arguments."
                .to_string(),
        );
    }

    if has_any_tools(
        &tool_names,
        &["models_list", "models_search", "models_info"],
    ) {
        bullets.push(
            "Use the models tools to discover available hosted LLMs and their metadata instead of assuming provider names, model IDs, pricing, or context windows."
                .to_string(),
        );
    }

    if has_any_tools(
        &tool_names,
        &[
            "marketplace_search",
            "marketplace_info",
            "marketplace_install",
            "marketplace",
        ],
    ) {
        bullets.push(
            "Use marketplace tools when the task depends on installing or inspecting published workflows and tools. Do not assume a workflow is locally available until you have checked the marketplace or local workflow catalog."
                .to_string(),
        );
    }

    if has_any_tools(
        &tool_names,
        &[
            "deploy_create",
            "deploy_list",
            "deploy_status",
            "deploy_health",
            "deploy_stop",
        ],
    ) {
        bullets.push(
            "Use deploy tools for persistent serving or target-based deployment. Do not treat deployment as an ad hoc shell process when the PRISM deploy surface already covers it."
                .to_string(),
        );
    }

    if has_any_tools(
        &tool_names,
        &[
            "node_probe",
            "node_status",
            "node_logs",
            "mesh_discover",
            "mesh_peers",
            "mesh_publish",
            "mesh_subscribe",
            "mesh_unsubscribe",
            "mesh_subscriptions",
        ],
    ) {
        bullets.push(
            "Use node tools to inspect local capability, visibility, and operator state before assuming compute or storage exists. Use mesh tools for discovery, publication, and subscription flows between nodes instead of treating deployment or ingest as the same thing."
                .to_string(),
        );
    }

    if has_any_tools(&tool_names, &["run_submit", "run"]) {
        bullets.push(
            "Use `run_submit` for one-off compute jobs across local, MARC27, or BYOC backends instead of hand-building `run` argv or shell wrappers."
                .to_string(),
        );
    }

    if has_any_tools(&tool_names, &["publish_artifact", "publish"]) {
        bullets.push(
            "Use `publish_artifact` for structured model, dataset, or workflow publishing instead of manually assembling `publish` arguments."
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

    if bullets.is_empty() {
        return base_prompt.to_string();
    }

    format!(
        "{base_prompt}\n\n# Loaded Tool Strategy\n{}",
        bullets
            .into_iter()
            .map(|bullet| format!("- {bullet}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
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
- Verify important work with tests, commands, or direct inspection when possible.
- Report outcomes exactly. If you did not run a check, say so plainly.

# Planning And Clarification
- When the request is ambiguous, ask one concrete question at a time.
- For multi-step work, give a short plan before acting and wait for approval when the user is steering interactively.
- In plan mode, focus on sequencing, constraints, and implementation shape rather than execution.

# Coding Workflow
- Inspect before editing.
- Prefer direct file tools for file work and shell tools for search, build, test, and git flows.
- Use targeted edits when possible. Use whole-file writes only when replacing or creating a file body is the right move.
- Prefer PRISM-native command tools over shelling out when a PRISM command already covers the operation.
- Keep changes small, coherent, and easy to verify.

# PRISM Workflow
- Treat workflows as the primary orchestration surface for YAML-defined pipelines.
- Use query for targeted retrieval, research for iterative retrieval-and-synthesis loops, and discourse for structured multi-agent debate.
- For DEEP research that would take minutes, use start_background_research (a separate platform agent works while you keep helping the user) and collect the result later with check_background_research — do not block the conversation on the synchronous research tool for big questions, and do not busy-poll.
- Use models to discover available hosted LLMs instead of assuming model names.
- Use marketplace when a workflow, tool, or artifact may need to be discovered or installed before execution.
- Use deploy for persistent serving or target-based deployment rather than ad hoc shell processes.
- Use node to inspect or prepare local capability, and use mesh for discovery/publication/subscription between nodes.
- Use ingest as one end-to-end command. Do not split extraction, embedding, and graph loading into separate user-facing steps unless the user explicitly asks for low-level control.
- Use find_tools to discover tools, agent_capabilities to inspect providers/models/connectivity, and status/tools for the local environment before planning.
- ACQUIRING NEW TOOLS: when no loaded tool fits, follow discover -> install -> connect -> verify: find_tools first (already have it?), then marketplace_search/marketplace_info, then marketplace_install (lands in ~/.prism/tools or ~/.prism/workflows; never overwrites local edits). Installed workflows are runnable immediately; installed Python tools load at the NEXT tool-server start — say so honestly and verify with list_tools before claiming a tool is callable. Full playbook: read docs/TOOL_ACQUISITION.md in the PRISM repo (read_file) when you need the complete procedure, publishing steps, or the anti-spoof/approval rules.
- Keep local, MARC27-hosted, and BYOC boundaries explicit in your reasoning when you choose a compute or storage path.

# Tool Use
- For greetings, chit-chat, and questions about things already visible in this conversation, respond with plain text. Do NOT call tools for simple chat.
- Call tools WITHOUT being asked when the answer depends on facts the user will act on and you cannot verify from memory: material properties, knowledge-graph contents, platform/job/deployment state, prices, availability.
- For explicit operations (deploy, ingest, run workflow, compute), use the matching tool — never claim you did something you didn't.

# Knowing Your Limits
You may be running as a small local model. The harness compensates for that only if you follow these rules:
- Treat your parametric memory as a sketch, not a reference. The things you "remember" most confidently — numeric values, formulas, citations, API names — are exactly the things most likely to be wrong.
- For any scientific or platform fact a user might act on, prefer a tool lookup over recall. If you must answer from memory, label it: "from model memory, unverified".
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
- Verify important work with tests, commands, or direct inspection when possible.
- Report outcomes exactly. If you could not run a check, say so plainly.

# Planning And Execution
- For multi-step work, state a short plan before acting.
- If the request is underspecified, make reasonable assumptions and state them explicitly before proceeding.
- In plan mode, focus on sequencing, constraints, and implementation shape rather than execution.

# Coding Workflow
- Inspect before editing.
- Prefer direct file tools for file work and shell tools for search, build, test, and git flows.
- Use targeted edits when possible. Use whole-file writes only when replacing or creating a file body is the right move.
- Prefer PRISM-native command tools over shelling out when a PRISM command already covers the operation.
- Keep changes small, coherent, and easy to verify.

# PRISM Workflow
- Treat workflows as the primary orchestration surface for YAML-defined pipelines.
- Use query for targeted retrieval, research for iterative retrieval-and-synthesis loops, and discourse for structured multi-agent debate.
- For DEEP research that would take minutes, use start_background_research (a separate platform agent works while you keep helping the user) and collect the result later with check_background_research — do not block the conversation on the synchronous research tool for big questions, and do not busy-poll.
- Use models to discover available hosted LLMs instead of assuming model names.
- Use marketplace when a workflow, tool, or artifact may need to be discovered or installed before execution.
- Use deploy for persistent serving or target-based deployment rather than ad hoc shell processes.
- Use node to inspect or prepare local capability, and use mesh for discovery/publication/subscription between nodes.
- Use ingest as one end-to-end command. Do not split extraction, embedding, and graph loading into separate user-facing steps unless low-level control is explicitly required by the task.
- Use find_tools to discover tools, agent_capabilities to inspect providers/models/connectivity, and status/tools for the local environment before planning.
- ACQUIRING NEW TOOLS: when no loaded tool fits, follow discover -> install -> connect -> verify: find_tools first (already have it?), then marketplace_search/marketplace_info, then marketplace_install (lands in ~/.prism/tools or ~/.prism/workflows; never overwrites local edits). Installed workflows are runnable immediately; installed Python tools load at the NEXT tool-server start — say so honestly and verify with list_tools before claiming a tool is callable. Full playbook: read docs/TOOL_ACQUISITION.md in the PRISM repo (read_file) when you need the complete procedure, publishing steps, or the anti-spoof/approval rules.
- Keep local, MARC27-hosted, and BYOC boundaries explicit in your reasoning when you choose a compute or storage path.

# Tool Use
- For greetings, chit-chat, and questions about things already visible in this conversation, respond with plain text. Do NOT call tools for simple chat.
- Call tools WITHOUT being asked when the answer depends on facts the user will act on and you cannot verify from memory: material properties, knowledge-graph contents, platform/job/deployment state, prices, availability.
- For explicit operations (deploy, ingest, run workflow, compute), use the matching tool — never claim you did something you didn't.

# Knowing Your Limits
You may be running as a small local model. The harness compensates for that only if you follow these rules:
- Treat your parametric memory as a sketch, not a reference. The things you "remember" most confidently — numeric values, formulas, citations, API names — are exactly the things most likely to be wrong.
- For any scientific or platform fact a user might act on, prefer a tool lookup over recall. If you must answer from memory, label it: "from model memory, unverified".
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
const COT_TITLE: &str = "Reasoning";
const COT_BODY: &str = "Think step by step before acting. In one or two lines, state what the user needs, which tool fits, and what could go wrong — then take a single concrete action. Reason briefly, then act; do not pad the answer.";

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
                parts.push(render_one_section(title, &block.body, profile.structure_style));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::PermissionMode;
    use crate::prompt_profile::{profile_for_model, LengthBudget, MaxTokensPolicy, ToolSurface};
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
        assert_eq!(render_system_prompt(INTERACTIVE_PROMPT, &markdown_full()), INTERACTIVE_PROMPT);
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

    /// PromptedCoT appends a reasoning nudge; other modes do not.
    #[test]
    fn prompted_cot_appends_reasoning_section() {
        let unknown = profile_for_model("some-local-model-7b");
        assert_eq!(unknown.reasoning_invocation, ReasoningMode::PromptedCoT);
        let with_cot = render_system_prompt(INTERACTIVE_PROMPT, &unknown);
        assert!(with_cot.contains("Think step by step before acting"));

        let no_cot = render_system_prompt(INTERACTIVE_PROMPT, &markdown_full());
        assert!(!no_cot.contains("Think step by step before acting"));
    }

    #[test]
    fn interactive_prompt_contains_interactive_guidance() {
        let prompt = build_system_prompt(true);
        assert!(prompt.contains("interactive agent"));
        assert!(prompt.contains("ask one concrete question at a time"));
        assert!(prompt.contains("wait for approval"));
    }

    #[test]
    fn autonomous_prompt_contains_assumption_guidance() {
        let prompt = build_system_prompt(false);
        assert!(prompt.contains("autonomous agent"));
        assert!(prompt.contains("make reasonable assumptions"));
        assert!(!prompt.contains("wait for approval"));
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
                permission_mode: PermissionMode::WorkspaceWrite,
                source: None,
                source_detail: None,
            },
            LoadedTool {
                name: "agent_capabilities".to_string(),
                description: "Inspect capabilities".to_string(),
                input_schema: json!({ "type": "object" }),
                requires_approval: false,
                permission_mode: PermissionMode::ReadOnly,
                source: None,
                source_detail: None,
            },
        ]);

        let prompt = append_runtime_tool_guidance(SYSTEM_PROMPT, &catalog);
        assert!(prompt.contains("# Loaded Tool Strategy"));
        assert!(prompt.contains("Treat workflows as the primary orchestration surface"));
        assert!(prompt.contains("agent_capabilities"));
    }

    #[test]
    fn runtime_guidance_mentions_node_mesh_and_marketplace() {
        let mut catalog = ToolCatalog::default();
        catalog.extend(vec![
            LoadedTool {
                name: "node_probe".to_string(),
                description: "Inspect node".to_string(),
                input_schema: json!({ "type": "object" }),
                requires_approval: false,
                permission_mode: PermissionMode::ReadOnly,
                source: None,
                source_detail: None,
            },
            LoadedTool {
                name: "mesh_publish".to_string(),
                description: "Publish to mesh".to_string(),
                input_schema: json!({ "type": "object" }),
                requires_approval: true,
                permission_mode: PermissionMode::FullAccess,
                source: None,
                source_detail: None,
            },
            LoadedTool {
                name: "marketplace_search".to_string(),
                description: "Search marketplace".to_string(),
                input_schema: json!({ "type": "object" }),
                requires_approval: false,
                permission_mode: PermissionMode::ReadOnly,
                source: None,
                source_detail: None,
            },
        ]);

        let prompt = append_runtime_tool_guidance(SYSTEM_PROMPT, &catalog);
        assert!(prompt.contains("Use node tools to inspect local capability"));
        assert!(prompt.contains("Use marketplace tools"));
    }

    #[test]
    fn runtime_guidance_stays_empty_for_empty_catalog() {
        let catalog = ToolCatalog::default();
        let prompt = append_runtime_tool_guidance(SYSTEM_PROMPT, &catalog);
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
            permission_mode: PermissionMode::WorkspaceWrite,
            source: Some("mcp".to_string()),
            source_detail: Some("atlas".to_string()),
        }]);

        let prompt = append_runtime_tool_guidance(SYSTEM_PROMPT, &catalog);
        assert!(prompt.contains("external MCP servers"));
    }
}
