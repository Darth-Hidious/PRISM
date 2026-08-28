//! Native "meta-tools" — durable-memory and tool-discovery tools that operate
//! on the agent's own state (the Turso provenance store, the tool catalog)
//! rather than on the outside world. They are intercepted in the agent loop
//! before the command-tool / Python tool-server dispatch.
//!
//! Increment 3 ships `recall`: the model's window onto durable memory. Every
//! tool call's full input+output is persisted to Turso (see
//! `agent_loop::record_tool_provenance`); when a result is too large to keep
//! inline, or was dropped by compaction, `recall` pulls it back — by record
//! id (exact) or by query (semantic + keyword search within the session).
//! This replaces the old `peek_result` pointer, which referenced a
//! write-only in-memory map and a tool that never existed.

use anyhow::Result;
use serde_json::{Value, json};

use prism_embed::EmbedBackend;
use prism_provenance::ProvenanceStore;

use crate::permissions::PermissionMode;
use crate::tool_catalog::{LoadedTool, ToolCatalog};

/// What a meta-tool DOES — the classification the access gate keys on. The
/// layer is NOT homogeneous: most members read agent state, `apply_patch`
/// mutates project source, `write_skill` and `run_skill` execute un-sandboxed
/// shell/Python as the node OS user, and `spawn_subagent` drives a nested agent
/// turn over the same tool surface.
/// Classifying by membership alone ("it is a meta-tool") is what once carried
/// the executing members ahead of every gate call site — so classification is
/// by effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaToolEffect {
    /// Pure read of agent state (durable memory, tool catalog, skill
    /// inventory, failure list). Keeps the early interception in the agent
    /// loop; no platform-access gate.
    ReadOnly,
    /// Mutates files inside the trusted project root. This is owner-only even
    /// though it does not spawn a process: source edits are just as capable of
    /// changing what the node executes next.
    WritesWorkspace,
    /// Executes code, or drives a nested agent turn that can. Must pass the
    /// same platform-access gate as every other execution surface (see
    /// `command_tools::gate_meta_tool_execution`).
    ExecutesCode,
}

/// The closed registry of native meta-tools. Adding a tool means adding a
/// variant, and [`MetaTool::name`] and [`MetaTool::effect`] are wildcard-free
/// matches over it — a new meta-tool CANNOT COMPILE until someone declares its
/// wire name AND whether it executes. That exhaustiveness is the property that
/// closed the command-dispatch series (`gate_command_execution`), reused here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaTool {
    ApplyPatch,
    Recall,
    FindTools,
    WriteSkill,
    RunSkill,
    ListSkills,
    SpawnSubagent,
    OrchestrateAgents,
    ListFailures,
    ReloadMcp,
}

impl MetaTool {
    /// Every meta-tool. A variant missing from this array still cannot skip
    /// classification (the wildcard-free matches force it), but it would skip
    /// the registry-parity test — the array makes that a compile-time count.
    pub const ALL: [MetaTool; 10] = [
        MetaTool::ApplyPatch,
        MetaTool::Recall,
        MetaTool::FindTools,
        MetaTool::WriteSkill,
        MetaTool::RunSkill,
        MetaTool::ListSkills,
        MetaTool::SpawnSubagent,
        MetaTool::OrchestrateAgents,
        MetaTool::ListFailures,
        MetaTool::ReloadMcp,
    ];

    /// Parse the wire name. Strings are an open set so the `_` arm is
    /// unavoidable — but this is REGISTRATION, not classification: every
    /// variant that can be constructed is forced through the exhaustive
    /// [`MetaTool::effect`] before any gate decision.
    #[must_use]
    pub fn from_name(tool_name: &str) -> Option<MetaTool> {
        match tool_name {
            "apply_patch" => Some(MetaTool::ApplyPatch),
            "recall" => Some(MetaTool::Recall),
            "find_tools" => Some(MetaTool::FindTools),
            "write_skill" => Some(MetaTool::WriteSkill),
            "run_skill" => Some(MetaTool::RunSkill),
            "list_skills" => Some(MetaTool::ListSkills),
            "spawn_subagent" => Some(MetaTool::SpawnSubagent),
            "orchestrate_agents" => Some(MetaTool::OrchestrateAgents),
            "list_failures" => Some(MetaTool::ListFailures),
            "reload_mcp" => Some(MetaTool::ReloadMcp),
            _ => None,
        }
    }

    /// The wire name — wildcard-free, so a new variant cannot compile until
    /// it is named.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            MetaTool::ApplyPatch => "apply_patch",
            MetaTool::Recall => "recall",
            MetaTool::FindTools => "find_tools",
            MetaTool::WriteSkill => "write_skill",
            MetaTool::RunSkill => "run_skill",
            MetaTool::ListSkills => "list_skills",
            MetaTool::SpawnSubagent => "spawn_subagent",
            MetaTool::OrchestrateAgents => "orchestrate_agents",
            MetaTool::ListFailures => "list_failures",
            MetaTool::ReloadMcp => "reload_mcp",
        }
    }

    /// Effect classification — wildcard-free, so a new variant cannot compile
    /// until someone declares whether it executes.
    #[must_use]
    pub fn effect(self) -> MetaToolEffect {
        match self {
            MetaTool::Recall
            | MetaTool::FindTools
            | MetaTool::ListSkills
            | MetaTool::ListFailures => MetaToolEffect::ReadOnly,
            MetaTool::ApplyPatch => MetaToolEffect::WritesWorkspace,
            // write_skill verifies by RUNNING the code once; run_skill
            // re-executes stored code; spawn_subagent and orchestrate_agents
            // drive nested turns over the same code-running tool surface.
            // All four are node-owner only.
            // reload_mcp launches every server named in ~/.prism/mcp.json as a
            // CHILD PROCESS. That the config is a local file does not make it
            // less than code execution — an entry can name any binary — so it
            // is owner-gated like the rest of this group. It is also why the
            // agent may not reach it on the strength of something it READ: a
            // server name arriving from a paper or a web page is untrusted
            // content, and the gate is what keeps that from becoming a spawn.
            MetaTool::WriteSkill
            | MetaTool::RunSkill
            | MetaTool::SpawnSubagent
            | MetaTool::OrchestrateAgents
            | MetaTool::ReloadMcp => MetaToolEffect::ExecutesCode,
        }
    }
}

/// How many matches `recall(query)` returns by default.
const DEFAULT_RECALL_LIMIT: usize = 5;
/// Cap (chars) on a BY-ID fetch. The by-id path is the model's explicit
/// "pull the full result back" move after the agent loop truncated an
/// oversized tool result inline (an 8k preview of a >30k result). Clipping
/// the by-id fetch at the same 8k as the preview made that pointer useless:
/// the model asked for the rest and got exactly what it already had.
/// 64k (a little over 2x the loop's 30k inline threshold) returns whole
/// every result the loop would have kept inline, bounds a pathological
/// multi-megabyte child output, and still names the remainder honestly
/// when a record exceeds it.
const RECALL_BY_ID_MAX_CHARS: usize = 64_000;
/// Share of the turn's REMAINING input budget a single by-id recall may take.
///
/// The constant above is a sensible size for one fetch and blind to how many
/// fetches a turn makes. Measured 2026-08-20 on a live research run: tool calls
/// 27–35 were nine consecutive recalls, and the turn went from 87% to 100% of a
/// 200k window across them. Nine fetches at 64k chars is ~144k tokens — 86% of
/// everything left after the tool block. Mid-turn compaction fired and could
/// not keep up, because each recall re-injects a full payload faster than
/// compaction sheds one. The run died having saved 570 papers and extracted
/// facts from none of them: the budget went on re-reading what was already
/// durably stored.
///
/// A quarter of what remains lets several recalls through early, when there is
/// room, and shrinks them as the turn fills — which is exactly when a large
/// fetch is most likely to be the thing that kills the run.
const RECALL_BUDGET_SHARE: f64 = 0.25;
/// Below this share of the budget remaining, a by-id recall REFUSES.
///
/// At 90% used the right move is not a smaller fetch, it is to stop fetching:
/// whatever the model is about to read, it will not have room to act on. The
/// refusal says so and names the alternative, because a silent empty result
/// would just be re-tried.
const RECALL_BUDGET_FLOOR: f64 = 0.10;

/// The by-id character cap for this call, given the turn's remaining input
/// budget. `None` remaining (no budget context — a slash command, a test)
/// keeps the flat ceiling, which is the pre-existing behaviour.
///
/// Returns `Err(reason)` when the turn is too far gone to spend on a recall.
fn recall_cap_for_budget(remaining: Option<TurnRemaining>) -> Result<usize, String> {
    let Some(rem) = remaining else {
        return Ok(RECALL_BY_ID_MAX_CHARS);
    };
    if rem.share() < RECALL_BUDGET_FLOOR {
        return Err(format!(
            "refusing to recall: only {:.0}% of this turn's token budget is left \
             ({} of {} tokens), and a full record would consume most of it. \
             Everything recall returns is ALREADY stored durably — nothing is \
             lost by not re-reading it. Write your answer from what you have, \
             or ingest the paper so its facts become graph rows instead of \
             conversation.",
            rem.share() * 100.0,
            rem.remaining,
            rem.total,
        ));
    }
    let allowed_tokens = (rem.remaining as f64 * RECALL_BUDGET_SHARE) as usize;
    Ok(allowed_tokens
        .saturating_mul(prism_llm::CHARS_PER_TOKEN)
        .min(RECALL_BY_ID_MAX_CHARS))
}

/// What is left of the turn's input budget, for sizing a recall.
#[derive(Clone, Copy, Debug)]
pub struct TurnRemaining {
    pub remaining: u64,
    pub total: u64,
}

impl TurnRemaining {
    #[must_use]
    pub fn new(used: u64, total: u64) -> Self {
        Self {
            remaining: total.saturating_sub(used),
            total,
        }
    }

    fn share(&self) -> f64 {
        if self.total == 0 {
            return 1.0;
        }
        self.remaining as f64 / self.total as f64
    }
}
/// Per-match preview length (chars) in a keyword search.
const RECALL_PREVIEW_CHARS: usize = 240;
/// Minimum cosine similarity for a semantic match. Measured on the native
/// BGE model: related sentences score ~0.8, unrelated ~0.3 — below this
/// floor a weak semantic hit must not displace an exact keyword match.
const SEMANTIC_SCORE_FLOOR: f32 = 0.4;
/// How many tools `find_tools(query)` returns by default.
const DEFAULT_FIND_TOOLS_LIMIT: usize = 8;
/// Hard server-side ceiling on `limit`.
///
/// `limit` is MODEL-controlled, and a JSON-schema `maximum` is advisory — a
/// provider will happily forward `limit: 130`. Every match is auto-pinned by the
/// agent loop, so one such call used to drag the whole catalog's FULL
/// definitions into every later request: 33,410 charged tokens as measured by
/// the adversarial review against the live catalog, against an 8k model's
/// ENTIRE 2,048-token tool budget. The budget cap in
/// `agent_loop::pin_within_budget` is what actually enforces the bound; this
/// ceiling keeps the discovery RESULT itself a readable shortlist rather than a
/// catalog dump the model has to wade through.
pub const MAX_FIND_TOOLS_LIMIT: usize = 25;
/// Per-match tool-description length (chars) in a discovery result.
const FIND_TOOLS_DESC_CHARS: usize = 400;

/// True if `tool_name` is handled by the native meta-tool layer.
#[must_use]
pub fn is_meta_tool(tool_name: &str) -> bool {
    MetaTool::from_name(tool_name).is_some()
}

/// True if `tool_name` is a reserved, trusted built-in — a native meta-tool or
/// a Rust command-tool (the spine). Authored skills and any future
/// user-brought / third-party tools MUST NOT claim these names: allowing it
/// would let an impostor shadow a trusted tool (tool spoofing), so the agent
/// thinks it is calling the real thing but runs attacker-supplied code. Callers
/// that ingest untrusted tools reject-or-namespace on a match here.
#[must_use]
pub fn is_reserved_tool_name(tool_name: &str) -> bool {
    is_meta_tool(tool_name) || crate::command_tools::is_command_tool(tool_name)
}

/// Meta-tools that live in the catalog but are NOT on the always-offered
/// surface — reachable through `find_tools`, and free once found because
/// `is_meta_tool` exempts them from slot competition.
///
/// `reload_mcp` is here rather than in [`definitions`] because the always-on
/// surface is FULL: the nine mandatory meta-tools charge almost exactly the
/// headroom `always_on_meta_tools_leave_room_in_the_minimum_tool_budget`
/// allows, and a tenth pushed it over. That guard is right and the answer is
/// not to raise its limit. Reloading MCP servers is a deliberate, occasional
/// act — the agent looks for it when it wants it, and the smallest supported
/// context keeps its room to work.
#[must_use]
pub fn discoverable_definitions() -> Vec<LoadedTool> {
    vec![LoadedTool {
        name: "reload_mcp".to_string(),
        description: "Reconnect the MCP servers in ~/.prism/mcp.json without restarting \
                PRISM. Edit that file with your file/bash tools, then call this. Reports the \
                servers connected, tools now callable, failures, and refused names. Callable \
                from your next turn."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
        requires_approval: true,
        declared_free: false,
        // Spawning child processes named by a config file sits with the
        // other code-execution surfaces, not with the read-only ones.
        permission_mode: PermissionMode::WorkspaceWrite,
        source: Some("builtin".to_string()),
        source_detail: Some("mcp".to_string()),
    }]
}

/// Catalog entries for the meta-tools, so the model is offered them and
/// `validate_prepared_tool_calls_are_known` accepts them. Merged into the
/// catalog alongside the command tools.
#[must_use]
pub fn definitions() -> Vec<LoadedTool> {
    vec![
        LoadedTool {
            name: "apply_patch".to_string(),
            description: "Patch one UTF-8 file: `*** Begin Patch`, `*** Update File: path`, `@@` hunks of space/`-`/`+` lines, `*** End Patch`; hunks must match uniquely."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "patch": {
                        "type": "string"
                    },
                    "match_policy": {
                        "type": "object",
                        "properties": {
                            "allow_whitespace": {
                                "type": "boolean",
                                "default": true
                            },
                            "allow_fuzzy": {
                                "type": "boolean",
                                "default": true
                            },
                            "fuzzy_similarity_threshold": {
                                "type": "number",
                                "minimum": 0.5,
                                "maximum": 1.0,
                                "default": 0.9
                            },
                            "fuzzy_window_lines": {
                                "type": "integer",
                                "minimum": 1,
                                "maximum": 100000,
                                "default": 4096
                            }
                        },
                        "additionalProperties": false
                    }
                },
                "required": ["patch"],
                "additionalProperties": false
            }),
            requires_approval: true,
            declared_free: false,
            permission_mode: PermissionMode::WorkspaceWrite,
            source: Some("builtin".to_string()),
            source_detail: Some("workspace-edit".to_string()),
        },
        LoadedTool {
            name: "recall".to_string(),
            description: "Retrieve earlier tool results from durable memory. Pass \
                `id` to fetch one specific result in full, or `query` to search \
                past tool calls by meaning and keyword. Defaults to the current \
                session; `session_id` scopes to another, `all_sessions: true` \
                searches all sessions. Use this instead of re-running a tool \
                whose output left your context."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Exact provenance record id to fetch in full."
                    },
                    "query": {
                        "type": "string",
                        "description": "Search past tool calls by meaning and keyword."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Optional session to scope to (defaults to the current session)."
                    },
                    // A typed boolean is less error-prone for a model than a
                    // magic session id such as "*", and cannot collide with a
                    // legitimate session name.
                    "all_sessions": {
                        "type": "boolean",
                        "default": false,
                        "description": "Search every session (with `query` only)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max matches to return (default 5)."
                    }
                }
            }),
            requires_approval: false,
            declared_free: true,
            permission_mode: PermissionMode::ReadOnly,
            source: Some("builtin".to_string()),
            source_detail: Some("durable-memory".to_string()),
        },
        LoadedTool {
            name: "list_failures".to_string(),
            description: "List this session's FAILED tool runs from durable memory, newest \
                first — tool name, exit_code, recorded error, timestamp. Use it to see \
                at a glance what broke without re-running anything. `session_id` scopes \
                to another session; `limit` caps the list (default 10, max 1000)."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "Optional session to scope to (defaults to the current session)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max failures to return (default 10)."
                    }
                }
            }),
            requires_approval: false,
            declared_free: true,
            permission_mode: PermissionMode::ReadOnly,
            source: Some("builtin".to_string()),
            source_detail: Some("durable-memory".to_string()),
        },
        LoadedTool {
            name: "find_tools".to_string(),
            description: "Search the full tool catalog and make matching tools \
                available to call. Use this when you need a capability that isn't \
                among your offered tools: describe what you want to do, then call \
                a returned tool by name."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Describe the capability you need."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_FIND_TOOLS_LIMIT,
                        "description": "Max tools to return (default 8, hard max 25)."
                    }
                },
                "required": ["query"]
            }),
            requires_approval: false,
            declared_free: true,
            permission_mode: PermissionMode::ReadOnly,
            source: Some("builtin".to_string()),
            source_detail: Some("tool-discovery".to_string()),
        },
        LoadedTool {
            name: "write_skill".to_string(),
            description: "Author a REUSABLE skill: a named shell or python snippet you \
                can call again on later turns. VERIFIED by running it once — saved only \
                if it exits cleanly — then `list_skills` shows it and `run_skill` \
                re-executes it. Use it for code you'll likely need again; give a clear \
                one-line `description` (embedded for later retrieval)."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Slug of [A-Za-z0-9_-], 1-64 chars. Becomes the skill id."
                    },
                    "description": {
                        "type": "string",
                        "description": "One line: what the skill does."
                    },
                    "language": {
                        "type": "string",
                        "enum": ["shell", "python"],
                        "description": "Default 'shell'."
                    },
                    "code": {
                        "type": "string",
                        "description": "Skill body; must exit 0."
                    }
                },
                "required": ["name", "description", "code"]
            }),
            requires_approval: true,
            declared_free: false,
            permission_mode: PermissionMode::WorkspaceWrite,
            source: Some("builtin".to_string()),
            source_detail: Some("self-authoring".to_string()),
        },
        LoadedTool {
            name: "run_skill".to_string(),
            description: "Follow a stored skill by name (see list_skills). Agent-authored \
                JSON skills execute their stored code. Human-authored Markdown skills return \
                untrusted procedure instructions only when explicitly selected with `$name` \
                or their policy permits implicit invocation; commands they request still \
                use normal gated tools."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The skill id to run."
                    }
                },
                "required": ["name"]
            }),
            requires_approval: true,
            declared_free: false,
            permission_mode: PermissionMode::WorkspaceWrite,
            source: Some("builtin".to_string()),
            source_detail: Some("self-authoring".to_string()),
        },
        LoadedTool {
            name: "list_skills".to_string(),
            description: "List both Voyager-authored JSON skills and human-authored Markdown \
                procedures, including source kind and implicit-invocation policy. Use this to see \
                what reusable skills exist before writing or explicitly selecting one."
                .to_string(),
            input_schema: json!({ "type": "object", "properties": {} }),
            requires_approval: false,
            declared_free: true,
            permission_mode: PermissionMode::ReadOnly,
            source: Some("builtin".to_string()),
            source_detail: Some("self-authoring".to_string()),
        },
        crate::subagent::definition(),
        crate::orchestrator::definition(),
    ]
}

/// Execute a meta-tool. Returns the tool's result value; the caller wraps it
/// as `{ "result": ... }` to match the command-tool convention.
///
/// Access is resolved HERE, not at any one dispatch site: mutating or executing
/// meta-tools (`apply_patch`, `write_skill`, `run_skill`) must prove node-owner
/// access exactly like every other execution surface — no matter who calls
/// (agent-loop dispatch, the `/skills` slash command, or anything added later).
/// Read-only state access stays open to LocalOnly callers. Mirrors
/// `execute_command_tool`, which gates internally.
pub async fn execute_meta_tool(
    tool_name: &str,
    args: &Value,
    store: Option<&ProvenanceStore>,
    session_id: &str,
    catalog: &ToolCatalog,
) -> Result<Value> {
    execute_meta_tool_with_project_root(tool_name, args, store, session_id, catalog, None, None)
        .await
}

/// Execute a meta-tool with the trusted project root supplied by the agent
/// runtime. Callers that do not own a project context use [`execute_meta_tool`];
/// `apply_patch` refuses in that context instead of deriving a root from CWD or
/// accepting one from model-controlled arguments.
#[allow(clippy::too_many_arguments)]
pub async fn execute_meta_tool_with_project_root(
    tool_name: &str,
    args: &Value,
    store: Option<&ProvenanceStore>,
    session_id: &str,
    catalog: &ToolCatalog,
    project_root: Option<&std::path::Path>,
    remaining: Option<TurnRemaining>,
) -> Result<Value> {
    let meta_tool = MetaTool::from_name(tool_name)
        .ok_or_else(|| anyhow::anyhow!("unknown meta-tool '{tool_name}'"))?;
    crate::command_tools::gate_meta_tool_execution(
        meta_tool,
        crate::command_tools::current_platform_access(),
    )?;

    // Wildcard-free over the closed registry: a new meta-tool cannot compile
    // until it is wired here.
    match meta_tool {
        MetaTool::ApplyPatch => {
            let project_root = project_root.ok_or_else(|| {
                anyhow::anyhow!("apply_patch requires a trusted project-root context")
            })?;
            crate::apply_patch::execute(project_root, args)
        }
        MetaTool::Recall => recall(args, store, session_id, remaining).await,
        MetaTool::FindTools => Ok(find_tools(args, catalog)),
        MetaTool::WriteSkill => write_skill(args).await,
        MetaTool::RunSkill => run_skill(args).await,
        MetaTool::ListSkills => Ok(list_skills()),
        MetaTool::ListFailures => list_failures(args, store, session_id).await,
        MetaTool::ReloadMcp => {
            let report = crate::mcp::reload_global().await;
            Ok(serde_json::to_value(report)?)
        }
        // Needs the live turn machinery (LLM client, tool server, approval
        // channel), which this signature cannot carry — the agent loop
        // intercepts it BEFORE this dispatcher (see agent_loop.rs). Reaching
        // this arm means a caller (e.g. the single-tool executor) tried to
        // run it out of context. The access gate above already ran, so this
        // refusal is about context, not privilege.
        MetaTool::SpawnSubagent => anyhow::bail!(
            "spawn_subagent runs a nested agent turn and is dispatched inside the agent loop only"
        ),
        // Same context requirement as spawn_subagent: the fan-out drives
        // nested turns over the live turn machinery.
        MetaTool::OrchestrateAgents => anyhow::bail!(
            "orchestrate_agents runs nested agent turns and is dispatched inside the agent loop only"
        ),
    }
}

/// Clip a string to `max` chars (whole chars, not bytes) for echoing back.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

/// Clip keeping BOTH a head and a tail, with an explicit elided-middle marker.
///
/// stderr from a Python traceback puts the load-bearing `Error: msg` /
/// `Exception: ...` line at the END — a head-only clip ([`clip`]) drops it,
/// so the agent sees stack frames but never the actual error. This keeps the
/// first `head` chars (traceback header — file/line context) AND the last
/// `tail` chars (the final exception line), with a marker naming how many
/// chars were elided. When the whole string fits in `head + tail`, it is
/// returned unchanged (no marker).
///
/// Whole-char-safe: head/tail boundaries snap to UTF-8 char boundaries.
/// VS1/F2: head+tail is enough to see the error line. A full recall-pointer
/// (durable record of the unclipped stderr) is deferred to the verified
/// provenance-store work — it belongs there, not in this foundation patch.
fn clip_head_tail(s: &str, head: usize, tail: usize) -> String {
    let total = s.chars().count();
    if total <= head + tail {
        return s.to_string();
    }
    // Walk char boundaries from the front for the head, and from the back
    // (in bytes) for the tail — snapping inward to the nearest char start.
    let head_str: String = s.chars().take(head).collect();
    // Tail: find the byte index of the (total - tail)-th char.
    let skip = total - tail;
    let tail_byte_start = s
        .char_indices()
        .nth(skip)
        .map(|(byte_idx, _)| byte_idx)
        .unwrap_or(s.len());
    let tail_str = &s[tail_byte_start..];
    let elided = total - head - tail;
    format!(
        "{head_str}\n[…{elided} chars elided — showing head + tail; the final error line is below…]\n…{tail_str}"
    )
}

/// `write_skill`: verify by executing once (Voyager: execute-before-store), and
/// only persist if it exits cleanly. An unverified skill is NOT saved — the
/// error is handed back so the model can fix the code and call again.
async fn write_skill(args: &Value) -> Result<Value> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let code = args.get("code").and_then(Value::as_str).unwrap_or("");
    let language = args
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or("shell")
        .trim();

    if !crate::skills::valid_name(name) {
        anyhow::bail!("invalid skill name '{name}': use 1-64 chars of [A-Za-z0-9_-]");
    }
    if is_reserved_tool_name(name) {
        // Anti-spoofing: an authored skill may never take the name of a trusted
        // built-in (meta-tool or command-tool). Reject so the impostor can't
        // shadow the real tool. Reported to the model so it renames + retries.
        return Ok(json!({
            "stored": false,
            "verified": false,
            "name": name,
            "error": format!("'{name}' is a reserved built-in tool name — authored skills must use a distinct name. Rename the skill (e.g. add a domain prefix) and call write_skill again."),
        }));
    }
    if description.is_empty() {
        anyhow::bail!("`description` is required (one line, embedded for retrieval)");
    }
    if code.trim().is_empty() {
        anyhow::bail!("`code` is required");
    }

    let lang = language.to_string();
    let code_for_run = code.to_string();
    let out =
        tokio::task::spawn_blocking(move || crate::skills::execute(&lang, &code_for_run)).await??;

    if !out.ok {
        return Ok(json!({
            "stored": false,
            "verified": false,
            "name": name,
            "error": "skill failed verification (non-zero exit); fix the code and call write_skill again",
            "exit_code": out.code,
            "stderr": clip_head_tail(&out.stderr, 500, 1500),
        }));
    }

    let skill = crate::skills::AuthoredSkill::new(name, description, language, code, true);
    let path = crate::skills::store(&skill)?;
    Ok(json!({
        "stored": true,
        "verified": true,
        "name": name,
        "path": path.to_string_lossy(),
        "stdout": clip(&out.stdout, 1000),
        "note": "Saved (untrusted). It now appears in list_skills; re-run it later with run_skill(name).",
    }))
}

/// `run_skill`: execute authored JSON or load an authorized human procedure.
async fn run_skill(args: &Value) -> Result<Value> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if !crate::skills::valid_name(&name) {
        anyhow::bail!("invalid skill name '{name}'");
    }
    let surface_policy = crate::skills::SkillSurfacePolicy::default();
    match crate::skills::resolve_runnable_skill(&name, &surface_policy)? {
        crate::skills::RunnableSkill::Authored(skill) => {
            let (lang, code) = (skill.language.clone(), skill.code.clone());
            let out =
                tokio::task::spawn_blocking(move || crate::skills::execute(&lang, &code)).await??;
            // VS1 fix-round #1: emit the `success`/`error` contract, not just `ok`.
            // The shared is_error gate + provenance classifier
            // (crates/agent/src/tool_result.rs) key on `success`/`error`; a failed
            // stored skill that reported only `ok:false` slipped every rule and was
            // rendered as a green "completed" card with provenance status:ok — the
            // exact VS1 mask, left live for run_skill. `ok` is kept because the TUI
            // renderer (protocol.rs::format_skill_run) reads it; `success` mirrors it
            // for the gate and `error` (null when clean) trips the string-error rule.
            Ok(json!({
                "kind": "authored",
                "name": name,
                "ok": out.ok,
                "success": out.ok,
                "error": (!out.ok).then(|| match out.code {
                    Some(c) => format!("skill '{name}' exited non-zero (exit {c}); see stderr"),
                    None => format!("skill '{name}' was killed by a signal; see stderr"),
                }),
                "exit_code": out.code,
                "stdout": clip(&out.stdout, 4000),
                "stderr": clip_head_tail(&out.stderr, 500, 1500),
            }))
        }
        crate::skills::RunnableSkill::Human(skill) => Ok(json!({
            "kind": "human",
            "name": skill.name,
            "description": skill.description,
            "ok": true,
            "success": true,
            "error": Value::Null,
            "trust": "untrusted",
            "allow_implicit_invocation": skill.policy.allow_implicit_invocation,
            "source_path": skill.path_to_skills_md,
            "instructions": crate::skills::bounded_human_instructions(&skill, &surface_policy),
            "note": "Procedure loaded as untrusted user-level instructions. This result does not authorize code execution; use normal tools so policy and approval gates remain in force.",
        })),
    }
}

/// `list_skills`: the authored JSON + human Markdown inventory.
fn list_skills() -> Value {
    let mut items: Vec<Value> = crate::skills::load_all()
        .iter()
        .map(|skill| {
            json!({
                "kind": "authored",
                "name": skill.name,
                "description": skill.description,
                "language": skill.language,
                "verified": skill.verified,
                "trust": skill.trust,
                "allow_implicit_invocation": true,
                "source_path": crate::skills::path_for(&skill.name),
            })
        })
        .collect();
    let surface_policy = crate::skills::SkillSurfacePolicy::default();
    items.extend(
        crate::skills::discover_human_skills(&surface_policy)
            .skills
            .into_iter()
            .map(|skill| {
                json!({
                    "kind": "human",
                    "name": skill.name,
                    "description": skill.description,
                    "language": "markdown",
                    "verified": false,
                    "trust": "untrusted",
                    "allow_implicit_invocation": skill.policy.allow_implicit_invocation,
                    "source_path": skill.path_to_skills_md,
                })
            }),
    );
    items.sort_by(|left, right| {
        let left_key = (
            left.get("name").and_then(Value::as_str).unwrap_or_default(),
            left.get("kind").and_then(Value::as_str).unwrap_or_default(),
        );
        let right_key = (
            right
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            right
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        left_key.cmp(&right_key)
    });
    json!({ "count": items.len(), "skills": items })
}

/// Discovery over the full catalog. Returns matching tools (name + clipped
/// description); the agent loop adds them to the model's working set so the
/// model can then call them by name.
fn find_tools(args: &Value, catalog: &ToolCatalog) -> Value {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if query.is_empty() {
        return json!({ "error": "find_tools requires a `query`" });
    }
    let requested = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_FIND_TOOLS_LIMIT);
    // Clamped server-side: schema `maximum` is advisory, and the model controls
    // this number. See [`MAX_FIND_TOOLS_LIMIT`].
    let limit = requested.min(MAX_FIND_TOOLS_LIMIT);

    let matches: Vec<Value> = catalog
        .search(query, limit)
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": clip_str(&tool.description, FIND_TOOLS_DESC_CHARS),
            })
        })
        .collect();

    json!({
        "query": query,
        "count": matches.len(),
        "matches": matches,
        // Told, not silently applied: a model that asked for 130 and got 25 must
        // know the shortlist is a shortlist, or it concludes the catalog is small.
        "limit_clamped_to": (requested > limit).then_some(limit),
        "hint": "these tools are now available — call one by name to use it",
    })
}

async fn recall(
    args: &Value,
    store: Option<&ProvenanceStore>,
    session_id: &str,
    remaining: Option<TurnRemaining>,
) -> Result<Value> {
    // Only an already-initialized backend: recall must never stall a turn on
    // model init. The background provenance tasks warm it up on first write,
    // so in practice it's ready long before the model asks to recall.
    let backend = crate::embeddings::backend_if_ready();
    recall_with_backend(args, store, session_id, backend.as_deref(), remaining).await
}

async fn recall_with_backend(
    args: &Value,
    store: Option<&ProvenanceStore>,
    session_id: &str,
    backend: Option<&dyn EmbedBackend>,
    remaining: Option<TurnRemaining>,
) -> Result<Value> {
    let Some(store) = store else {
        return Ok(json!({ "error": "durable memory is unavailable in this session" }));
    };

    let requested_session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let all_sessions = args
        .get("all_sessions")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if all_sessions && requested_session_id.is_some() {
        return Ok(json!({
            "error": "`all_sessions` cannot be combined with `session_id`"
        }));
    }

    // Exact id lookup wins when present.
    if let Some(id) = args
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if all_sessions {
            return Ok(json!({
                "error": "`all_sessions` is only valid with `query`; pass `session_id` to fetch an id from another session"
            }));
        }
        // A full record is the single largest thing the model can pull into a
        // turn, so what it may cost depends on what the turn has left.
        let cap = match recall_cap_for_budget(remaining) {
            Ok(cap) => cap,
            Err(reason) => return Ok(json!({ "error": reason })),
        };
        // `query_chain` starts at `id` and walks parents within the selected
        // session; the record itself is included, so find it in the chain.
        let chain = store
            .query_chain(id, requested_session_id.unwrap_or(session_id))
            .await?;
        return Ok(match chain.into_iter().find(|r| r.id == id) {
            Some(rec) => {
                let mut out = json!({
                    "id": rec.id,
                    "tool_name": rec.tool_name,
                    "input": rec.input_json,
                    "output": clip_value(rec.output_json, cap),
                    "status": rec.status,
                    "exit_code": rec.exit_code,
                });
                // Say WHY it is short, or the model reads a budget trim as the
                // record being small and stops looking for the rest.
                if cap < RECALL_BY_ID_MAX_CHARS
                    && let Some(rem) = remaining
                {
                    out["budget_note"] = json!(format!(
                        "trimmed to {cap} chars: {:.0}% of this turn's token \
                         budget remains. Recalls are the most expensive thing \
                         you can do to a turn, and this record is still stored \
                         in full — narrow the query, or ingest instead of \
                         re-reading.",
                        rem.share() * 100.0
                    ));
                }
                out
            }
            None => json!({ "error": format!("no record with id '{id}'") }),
        });
    }

    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if query.is_empty() {
        return Ok(json!({ "error": "recall requires either `id` or `query`" }));
    }
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_RECALL_LIMIT);

    let mut matches = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let search_session_id = if all_sessions {
        None
    } else {
        Some(requested_session_id.unwrap_or(session_id))
    };

    // Semantic pass first — matches by meaning, no shared substring needed.
    // Best-effort: any failure just leaves the keyword pass to fill in.
    if let Some(backend) = backend {
        match backend.embed(std::slice::from_ref(&query)).await {
            Ok(vectors) => {
                if let Some(query_vec) = vectors.first() {
                    match store
                        .semantic_search(query_vec, search_session_id, limit)
                        .await
                    {
                        Ok(hits) => {
                            for (rec, score) in hits {
                                if score < SEMANTIC_SCORE_FLOOR {
                                    break; // hits are sorted — the rest are weaker
                                }
                                let output_str = rec
                                    .output_json
                                    .as_ref()
                                    .map(ToString::to_string)
                                    .unwrap_or_default();
                                seen.insert(rec.id.clone());
                                let mut hit = json!({
                                    "id": rec.id,
                                    "tool_name": rec.tool_name,
                                    "preview": clip_str(&output_str, RECALL_PREVIEW_CHARS),
                                    "status": rec.status,
                                    "exit_code": rec.exit_code,
                                    "score": format!("{score:.3}"),
                                });
                                if all_sessions {
                                    hit["session_id"] = json!(rec.session_id);
                                }
                                matches.push(hit);
                            }
                        }
                        Err(e) => tracing::debug!("semantic recall failed: {e:#}"),
                    }
                }
            }
            Err(e) => tracing::debug!("recall query embedding failed: {e:#}"),
        }
    }

    // Newest-first keyword scan over the same scope as semantic search — fills
    // remaining slots; the only pass when no embed backend is ready.
    let needle = query.to_lowercase();
    let records = match search_session_id {
        Some(sid) => store.query_by_session(sid).await?,
        None => store.query_all().await?,
    };
    for rec in records.into_iter().rev() {
        if matches.len() >= limit {
            break;
        }
        if seen.contains(&rec.id) {
            continue; // already surfaced by the semantic pass
        }
        let output_str = rec
            .output_json
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let haystack = format!(
            "{} {} {}",
            rec.tool_name.as_deref().unwrap_or(""),
            rec.input_json,
            output_str
        )
        .to_lowercase();
        if haystack.contains(&needle) {
            let mut hit = json!({
                "id": rec.id,
                "tool_name": rec.tool_name,
                "preview": clip_str(&output_str, RECALL_PREVIEW_CHARS),
                "status": rec.status,
                "exit_code": rec.exit_code,
            });
            if all_sessions {
                hit["session_id"] = json!(rec.session_id);
            }
            matches.push(hit);
        }
    }
    matches.truncate(limit);

    Ok(json!({
        "query": query,
        "count": matches.len(),
        "matches": matches,
        "hint": if all_sessions || requested_session_id.is_some() {
            "call recall with a returned id and session_id to get that result's full output"
        } else {
            "call recall with a returned id to get that result's full output"
        },
    }))
}

/// Default cap on how many failures `list_failures` returns at once. The store
/// itself clamps to 1000 regardless (see `ProvenanceStore::query_failures`); a
/// smaller default keeps the model-facing payload readable.
const DEFAULT_FAILURES_LIMIT: usize = 10;

/// VS3: the agent-callable "which runs failed?" query. Returns the session's
/// (or another session's) failed tool runs, newest first, with the tool name,
/// exit code, a one-line error, and a timestamp — enough to see what broke
/// without re-running anything. Mirrors `recall`'s store-missing handling.
async fn list_failures(
    args: &Value,
    store: Option<&ProvenanceStore>,
    session_id: &str,
) -> Result<Value> {
    let Some(store) = store else {
        return Ok(json!({ "error": "durable memory is unavailable in this session" }));
    };

    // Optional session scope (defaults to the current session), bounded limit.
    let scope = args
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_FAILURES_LIMIT);

    let failures = store
        .query_failures(scope.as_deref().or(Some(session_id)), limit)
        .await?;

    let count = failures.len();
    let entries: Vec<Value> = failures
        .into_iter()
        .map(|rec| {
            // Surface the recorded error line: prefer output_json.error, then
            // output_json.stderr's last line, then nothing (the status itself
            // is already 'error').
            let out = rec.output_json.as_ref();
            let error = out
                .and_then(|o| o.get("error"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    out.and_then(|o| o.get("stderr"))
                        .and_then(Value::as_str)
                        .and_then(|s| s.lines().find(|l| !l.trim().is_empty()))
                        .map(str::to_string)
                });
            json!({
                "id": rec.id,
                "tool_name": rec.tool_name,
                "exit_code": rec.exit_code,
                "error": error,
                "timestamp": rec.timestamp,
            })
        })
        .collect();

    Ok(json!({
        "count": count,
        "failures": entries,
        "hint": "call recall with a returned id to see that run's full output",
    }))
}

/// Echo a stored output back to the model, preserving JSON structure when it
/// fits and clipping to a string when it exceeds `max_chars`. The by-id
/// fetch path passes [`RECALL_BY_ID_MAX_CHARS`] — see that constant for why
/// an explicit single-record fetch must be able to return more than the
/// preview the model already saw.
fn clip_value(v: Option<Value>, max_chars: usize) -> Value {
    match v {
        None => Value::Null,
        Some(value) => {
            let serialized = value.to_string();
            if serialized.chars().count() <= max_chars {
                value
            } else {
                let total = serialized.chars().count();
                Value::String(clip_str_with_remainder(&serialized, max_chars, total))
            }
        }
    }
}

fn clip_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    format!("{head}…[clipped]")
}

/// Like [`clip_str`], but names how much of the record remains unread —
/// a still-clipped by-id fetch must say so instead of presenting itself
/// as the whole record.
fn clip_str_with_remainder(s: &str, max_chars: usize, total_chars: usize) -> String {
    let head: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!(
        "{head}…[record is {total_chars} chars; showing first {max_chars} — refine the \
         original query or lower max_results for a smaller result]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_provenance::{ActionType, Actor, new_record};

    async fn seeded_store() -> (ProvenanceStore, String) {
        let store = ProvenanceStore::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        let session = "sess-recall";
        let mut r1 = new_record(
            session,
            ActionType::ToolCall,
            Actor::Agent,
            Some("file"),
            None,
            json!({ "path": "alloy.csv" }),
        );
        r1.output_json = Some(json!("titanium aluminide rows: 42"));
        store.record(&r1).await.unwrap();
        let mut r2 = new_record(
            session,
            ActionType::ToolCall,
            Actor::Agent,
            Some("shell"),
            None,
            json!({ "cmd": "ls" }),
        );
        r2.output_json = Some(json!("a\nb"));
        store.record(&r2).await.unwrap();
        (store, r1.id)
    }

    async fn cross_session_seeded_store() -> (ProvenanceStore, Vec<String>) {
        let store = ProvenanceStore::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        let mut ids = Vec::new();
        for session in ["sess-current", "sess-other", "sess-third"] {
            let mut rec = new_record(
                session,
                ActionType::ToolCall,
                Actor::Agent,
                Some("file"),
                None,
                json!({ "alloy": "Ti-6Al-4V" }),
            );
            rec.output_json = Some(json!(format!("shared brief marker for {session}")));
            store.record(&rec).await.unwrap();
            ids.push(rec.id);
        }
        (store, ids)
    }

    fn recall_match_ids(output: &Value) -> Vec<String> {
        output["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn is_meta_tool_recognizes_native_tools() {
        assert!(is_meta_tool("apply_patch"));
        assert!(is_meta_tool("recall"));
        assert!(is_meta_tool("find_tools"));
        assert!(is_meta_tool("spawn_subagent"));
        assert!(is_meta_tool("list_failures"));
        assert!(!is_meta_tool("file"));
        assert!(!is_meta_tool("peek_result"));
    }

    /// The exhaustive-classification invariant (round 7): a meta-tool added
    /// later cannot skip declaring what it does. The compile-time half is the
    /// wildcard-free matches in [`MetaTool::name`] and [`MetaTool::effect`] —
    /// a new variant fails to build until both are extended. This test is the
    /// runtime half: the closed registry round-trips name->enum->name, matches
    /// the definitions offered to the model exactly, and the effect split the
    /// access gate keys on is the deliberate one (moving a tool across the
    /// line must update this test).
    #[test]
    fn new_meta_tool_cannot_skip_name_or_effect_classification() {
        // Every registered variant round-trips and is recognized.
        for tool in MetaTool::ALL {
            assert_eq!(MetaTool::from_name(tool.name()), Some(tool));
            assert!(is_meta_tool(tool.name()), "{}", tool.name());
        }
        // Read-only state access: may keep the early interception.
        for tool in [
            MetaTool::Recall,
            MetaTool::FindTools,
            MetaTool::ListSkills,
            MetaTool::ListFailures,
        ] {
            assert_eq!(tool.effect(), MetaToolEffect::ReadOnly, "{}", tool.name());
        }
        assert_eq!(
            MetaTool::ApplyPatch.effect(),
            MetaToolEffect::WritesWorkspace
        );
        // Code execution (or a nested turn that drives it): owner-gated.
        for tool in [
            MetaTool::WriteSkill,
            MetaTool::RunSkill,
            MetaTool::SpawnSubagent,
            MetaTool::OrchestrateAgents,
        ] {
            assert_eq!(
                tool.effect(),
                MetaToolEffect::ExecutesCode,
                "{}",
                tool.name()
            );
        }
        // Registry parity with the definitions the model can reach: neither
        // side may grow without the other. Now spread over TWO lists — the
        // always-offered surface and the find_tools-discoverable one — because
        // the always-on surface is full (see `discoverable_definitions`). A
        // variant must appear in exactly one of them: absent from both it is
        // undispatchable, present in both it would be offered twice.
        let mut def_names: Vec<String> = definitions()
            .iter()
            .chain(discoverable_definitions().iter())
            .map(|t| t.name.clone())
            .collect();
        let mut all_names: Vec<String> =
            MetaTool::ALL.iter().map(|t| t.name().to_string()).collect();
        def_names.sort();
        all_names.sort();
        assert_eq!(def_names, all_names);

        // ...and the two lists are disjoint.
        let always: std::collections::HashSet<String> =
            definitions().iter().map(|t| t.name.clone()).collect();
        for tool in discoverable_definitions() {
            assert!(
                !always.contains(&tool.name),
                "{} is both always-on and discoverable",
                tool.name
            );
        }
    }

    /// The gate is INSIDE the executor (mirrors `execute_command_tool`), so
    /// every dispatch path — agent loop, `/skills` slash command, anything
    /// later — resolves access here. Default platform access is LocalOnly
    /// (non-owner): executing meta-tools must refuse, read-only state access
    /// must not.
    #[tokio::test]
    async fn execute_meta_tool_refuses_non_owner_execution_and_allows_read_only() {
        let catalog = ToolCatalog::from_tool_server_json(&json!({ "tools": [] }));
        for tool in ["apply_patch", "write_skill", "run_skill"] {
            let err = execute_meta_tool(tool, &json!({ "name": "x" }), None, "", &catalog)
                .await
                .expect_err("{tool} must be refused for a LocalOnly caller");
            assert!(err.to_string().contains("owner-only"), "{tool}: {err:#}");
        }
        // Read-only members pass the same default access.
        let out = execute_meta_tool("list_skills", &json!({}), None, "", &catalog)
            .await
            .expect("read-only meta-tools must not be gated");
        assert!(out["skills"].is_array(), "{out}");
    }

    #[tokio::test]
    async fn apply_patch_dispatch_is_owner_only_and_uses_the_trusted_root() {
        use crate::command_tools::{CommandToolPlatformAccess, with_platform_access};

        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("demo.txt"), b"before\n").unwrap();
        let catalog = ToolCatalog::from_tool_server_json(&json!({ "tools": [] }));
        let args = json!({
            "patch": "*** Begin Patch\n*** Update File: demo.txt\n@@\n-before\n+after\n*** End Patch"
        });

        let missing_root = with_platform_access(
            CommandToolPlatformAccess::VerifiedNodeOwner,
            execute_meta_tool("apply_patch", &args, None, "", &catalog),
        )
        .await
        .expect_err("a caller may not derive the project root implicitly");
        assert!(
            missing_root.to_string().contains("trusted project-root"),
            "{missing_root:#}"
        );

        with_platform_access(
            CommandToolPlatformAccess::VerifiedNodeOwner,
            execute_meta_tool_with_project_root(
                "apply_patch",
                &args,
                None,
                "",
                &catalog,
                Some(root.path()),
                None,
            ),
        )
        .await
        .expect("verified owner should reach the patch engine");
        assert_eq!(
            std::fs::read(root.path().join("demo.txt")).unwrap(),
            b"after\n"
        );
    }

    #[test]
    fn reserved_names_cover_meta_and_command_tools_but_not_arbitrary() {
        // Anti-spoofing: both trusted layers are reserved against authored/user tools.
        assert!(is_reserved_tool_name("apply_patch")); // mutating meta-tool
        assert!(is_reserved_tool_name("recall")); // meta-tool
        assert!(is_reserved_tool_name("mesh_publish")); // command-tool (spine)
        assert!(is_reserved_tool_name("mesh_health")); // freshly-ported command-tool
        assert!(is_reserved_tool_name("research")); // command-tool
        // A normal, non-built-in skill name is allowed.
        assert!(!is_reserved_tool_name("my_alloy_screener"));
        assert!(!is_reserved_tool_name("summarize_dft_run"));
    }

    #[tokio::test]
    async fn write_skill_rejects_reserved_names() {
        // A skill trying to squat a trusted built-in name is refused BEFORE any
        // execution or storage — the model is told to rename and retry.
        for squat in ["apply_patch", "recall", "mesh_publish", "research"] {
            let out = write_skill(&json!({
                "name": squat,
                "description": "impostor",
                "code": "echo hi",
                "language": "shell",
            }))
            .await
            .unwrap();
            assert_eq!(out["stored"], json!(false), "{squat} must not be stored");
            assert_eq!(out["verified"], json!(false), "{squat} must not run");
            assert!(
                out["error"].as_str().unwrap_or("").contains("reserved"),
                "{squat} error should explain the reservation"
            );
        }
    }

    #[test]
    fn meta_tool_permissions_match_their_effect() {
        let defs = definitions();
        let by = |name: &str| defs.iter().find(|t| t.name == name).expect(name).clone();

        // Read-only, no-approval: memory + discovery, listing skills, and
        // listing failures (a pure read over durable memory).
        for name in ["recall", "find_tools", "list_skills", "list_failures"] {
            let t = by(name);
            assert_eq!(t.permission_mode, PermissionMode::ReadOnly, "{name}");
            assert!(!t.requires_approval, "{name} must not need approval");
        }
        // Code-executing self-authoring tools — and delegation, which spends
        // tokens and drives tools — are workspace-write + gated.
        for name in ["apply_patch", "write_skill", "run_skill", "spawn_subagent"] {
            let t = by(name);
            assert_eq!(t.permission_mode, PermissionMode::WorkspaceWrite, "{name}");
            assert!(
                t.requires_approval,
                "{name} mutates state or executes code → must need approval"
            );
        }
    }

    #[test]
    fn apply_patch_definition_declares_every_match_relaxation() {
        let tool = definitions()
            .into_iter()
            .find(|tool| tool.name == "apply_patch")
            .expect("apply_patch definition");
        let policy = &tool.input_schema["properties"]["match_policy"]["properties"];

        assert_eq!(policy["allow_whitespace"]["type"], json!("boolean"));
        assert_eq!(policy["allow_fuzzy"]["type"], json!("boolean"));
        assert_eq!(
            policy["fuzzy_similarity_threshold"]["type"],
            json!("number")
        );
        assert_eq!(policy["fuzzy_window_lines"]["type"], json!("integer"));
        assert_eq!(policy["fuzzy_window_lines"]["default"], json!(4096));
        assert_eq!(policy["fuzzy_window_lines"]["maximum"], json!(100000));
        assert_eq!(
            tool.input_schema["properties"]["match_policy"]["additionalProperties"],
            json!(false)
        );
    }

    #[test]
    fn always_on_meta_tools_leave_room_in_the_minimum_tool_budget() {
        // The smallest supported context must still afford one ordinary task
        // tool after the mandatory meta surface. A token-sized remainder can
        // pass the arithmetic bound while leaving the agent unable to work.
        const MIN_CATALOG_HEADROOM: usize = 512;
        let charged: usize = definitions()
            .iter()
            .map(|tool| crate::tool_catalog::definition_tokens(&tool.to_definition()))
            .sum();
        assert!(
            charged + MIN_CATALOG_HEADROOM <= crate::tool_catalog::MIN_TOOL_TOKENS,
            "always-on meta-tools charge {charged} tokens and leave less than {MIN_CATALOG_HEADROOM} for task tools"
        );
    }

    #[test]
    fn recall_definition_exposes_session_scopes() {
        let recall = definitions()
            .into_iter()
            .find(|tool| tool.name == "recall")
            .unwrap();
        let properties = &recall.input_schema["properties"];

        assert_eq!(properties["session_id"]["type"], json!("string"));
        assert_eq!(
            properties["session_id"]["description"],
            json!("Optional session to scope to (defaults to the current session).")
        );
        assert_eq!(properties["all_sessions"]["type"], json!("boolean"));
        assert_eq!(properties["all_sessions"]["default"], json!(false));
        assert!(
            recall.description.contains("all sessions"),
            "the model-facing description must advertise cross-session recall"
        );
        assert!(
            !recall.description.contains("this session's"),
            "the description must not claim recall is current-session-only"
        );
    }

    fn catalog_with(names_and_descs: &[(&str, &str)]) -> ToolCatalog {
        let tools: Vec<Value> = names_and_descs
            .iter()
            .map(|(n, d)| {
                json!({
                    "name": n,
                    "description": d,
                    "input_schema": { "type": "object", "properties": {} }
                })
            })
            .collect();
        ToolCatalog::from_tool_server_json(&json!({ "tools": tools }))
    }

    #[test]
    fn find_tools_returns_relevant_matches() {
        let catalog = catalog_with(&[
            (
                "deploy_model",
                "Deploy a trained model to a serving endpoint",
            ),
            ("query_graph", "Query the materials knowledge graph"),
            ("send_email", "Send an email to a recipient"),
        ]);
        let out = find_tools(&json!({ "query": "deploy a model" }), &catalog);
        let names: Vec<&str> = out["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"deploy_model"));
        assert!(!names.contains(&"send_email"));
    }

    /// `limit` is MODEL-controlled. A schema `maximum` is advisory — providers
    /// forward whatever the model emitted — so the ceiling has to be applied
    /// HERE. Unclamped, one `find_tools(limit=130)` returned the whole catalog,
    /// every entry of which the agent loop auto-pins into every later request.
    #[test]
    fn find_tools_limit_is_clamped_server_side() {
        let entries: Vec<(String, String)> = (0..130)
            .map(|i| (format!("tool_{i:03}"), format!("capability number {i}")))
            .collect();
        let pairs: Vec<(&str, &str)> = entries
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_str()))
            .collect();
        let catalog = catalog_with(&pairs);

        let out = find_tools(&json!({ "query": "capability", "limit": 130 }), &catalog);
        let count = out["matches"].as_array().unwrap().len();
        assert_eq!(
            count, MAX_FIND_TOOLS_LIMIT,
            "an unclamped limit hands the model the whole catalog to pin"
        );
        assert_eq!(out["count"], json!(MAX_FIND_TOOLS_LIMIT));
        // Told, not silently truncated.
        assert_eq!(out["limit_clamped_to"], json!(MAX_FIND_TOOLS_LIMIT));

        // A sane limit is untouched, and reports no clamp.
        let out = find_tools(&json!({ "query": "capability", "limit": 3 }), &catalog);
        assert_eq!(out["matches"].as_array().unwrap().len(), 3);
        assert_eq!(out["limit_clamped_to"], json!(null));
    }

    #[test]
    fn find_tools_requires_query() {
        let catalog = catalog_with(&[("x", "y")]);
        let out = find_tools(&json!({}), &catalog);
        assert!(out["error"].as_str().unwrap().contains("query"));
    }

    /// A recall's size must fall out of what the TURN has left, not a constant.
    ///
    /// The flat 64k ceiling is a reasonable size for one fetch and blind to how
    /// many fetches a turn makes. Measured on a live run: nine consecutive
    /// recalls took a 200k-token turn from 87% to 100%, because nine × 64k
    /// chars is ~144k tokens — 86% of everything left after the tool block.
    #[test]
    fn a_recall_is_sized_by_what_the_turn_has_left() {
        // No budget context (slash command, test): unchanged behaviour.
        assert_eq!(recall_cap_for_budget(None), Ok(RECALL_BY_ID_MAX_CHARS));

        // Fresh turn — plenty of room, so the flat ceiling still binds.
        let fresh = TurnRemaining::new(0, 200_000);
        assert_eq!(
            recall_cap_for_budget(Some(fresh)),
            Ok(RECALL_BY_ID_MAX_CHARS)
        );

        // Half spent: 100k left, a quarter of that is 25k tokens = 100k chars,
        // still above the ceiling.
        let half = TurnRemaining::new(100_000, 200_000);
        assert_eq!(
            recall_cap_for_budget(Some(half)),
            Ok(RECALL_BY_ID_MAX_CHARS)
        );

        // Tight but usable: 30k left -> 7.5k tokens -> 30k chars, under the
        // ceiling, so the budget is what binds.
        let tight = TurnRemaining::new(170_000, 200_000);
        let cap = recall_cap_for_budget(Some(tight)).expect("15% left is still spendable");
        assert!(
            cap < RECALL_BY_ID_MAX_CHARS && cap > 0,
            "the budget must bind before the constant does: {cap}"
        );

        // The case that actually killed the run, simulated: nine recalls in a
        // row on a 200k turn. Each one is sized against what is left AT THAT
        // MOMENT, so the sequence decays instead of nine equal 64k bites.
        let total = 200_000_u64;
        let mut used = 60_000_u64; // tool block + the searches that came first
        let mut refused_at = None;
        for call in 1..=9 {
            match recall_cap_for_budget(Some(TurnRemaining::new(used, total))) {
                Ok(cap) => used += (cap / prism_llm::CHARS_PER_TOKEN) as u64,
                Err(_) => {
                    refused_at = Some(call);
                    break;
                }
            }
            assert!(
                used < total,
                "recall #{call} pushed the turn to {used}/{total} — the sequence \
                 that died at 100% must not be reachable"
            );
        }
        // With the flat ceiling every call took 64k chars (16k tokens) and the
        // ninth landed past 200k. Now the turn either survives all nine or is
        // told to stop before it can spend itself to death.
        assert!(
            used < total,
            "nine budget-sized recalls must not exhaust the turn: {used}/{total}"
        );
        assert!(
            refused_at.is_none() || refused_at.is_some_and(|c| c > 1),
            "the floor must not fire on the first call of a turn with 70% left"
        );
    }

    /// Past the floor the answer is not a smaller fetch, it is "stop fetching".
    #[test]
    fn a_nearly_spent_turn_refuses_to_recall_and_says_why() {
        let spent = TurnRemaining::new(195_000, 200_000); // 2.5% left
        let reason = recall_cap_for_budget(Some(spent))
            .expect_err("at 2.5% left a full record would consume what is left");

        // The refusal has to be actionable, or it is just a failure the model
        // retries. Name the two facts it needs: nothing is lost, and what to do.
        assert!(reason.contains("ALREADY stored"), "{reason}");
        assert!(reason.contains("ingest"), "{reason}");
    }

    #[tokio::test]
    async fn recall_by_id_returns_full_record() {
        let (store, id) = seeded_store().await;
        let out = recall(
            &json!({ "id": id.clone() }),
            Some(&store),
            "sess-recall",
            None,
        )
        .await
        .unwrap();
        assert_eq!(out["id"], json!(id));
        assert_eq!(out["tool_name"], json!("file"));
        assert_eq!(out["output"], json!("titanium aluminide rows: 42"));
    }

    /// B2: the truncation pointer in `agent_loop::process_large_result`
    /// promises the model it can pull an oversized result back with
    /// `recall(id=...)`. The by-id cap (64k) must sit far ABOVE the 8k
    /// inline preview, or the pointer returns exactly what the model
    /// already saw; and a record that still exceeds the cap must say how
    /// much remains instead of presenting itself as whole.
    #[tokio::test]
    async fn recall_by_id_pulls_back_more_than_the_inline_preview() {
        let store = ProvenanceStore::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        let mut rec = new_record(
            "sess-big",
            ActionType::ToolCall,
            Actor::Agent,
            Some("papers"),
            None,
            json!({ "q": "full-text" }),
        );
        let payload = "x".repeat(40_000);
        rec.output_json = Some(json!(payload.clone()));
        store.record(&rec).await.unwrap();

        let out = recall(
            &json!({ "id": rec.id.clone() }),
            Some(&store),
            "sess-big",
            None,
        )
        .await
        .unwrap();
        // 40k fits whole under the 64k by-id cap — "pull it back" is
        // literally true for this record.
        assert_eq!(out["output"], json!(payload));

        // A record past the cap is honestly marked, never silently cut.
        let mut huge = rec.clone();
        huge.id = "rec-huge".to_string();
        let payload2 = "y".repeat(100_000);
        huge.output_json = Some(json!(payload2));
        store.record(&huge).await.unwrap();
        let out2 = recall(&json!({ "id": "rec-huge" }), Some(&store), "sess-big", None)
            .await
            .unwrap();
        let clipped = out2["output"].as_str().unwrap();
        assert!(
            clipped.contains("record is 100002 chars; showing first 64000"),
            "{clipped}"
        );
    }

    #[tokio::test]
    async fn recall_by_query_finds_matches() {
        let (store, _) = seeded_store().await;
        let out = recall(
            &json!({ "query": "titanium" }),
            Some(&store),
            "sess-recall",
            None,
        )
        .await
        .unwrap();
        assert_eq!(out["count"], json!(1));
        assert_eq!(out["matches"][0]["tool_name"], json!("file"));
    }

    #[tokio::test]
    async fn recall_scopes_dispatch_through_execute_meta_tool() {
        let (store, ids) = cross_session_seeded_store().await;
        let catalog = ToolCatalog::from_tool_server_json(&json!({ "tools": [] }));

        let current = execute_meta_tool(
            "recall",
            &json!({ "query": "shared brief marker" }),
            Some(&store),
            "sess-current",
            &catalog,
        )
        .await
        .unwrap();
        assert_eq!(recall_match_ids(&current), vec![ids[0].clone()]);
        assert!(
            current["matches"][0].get("session_id").is_none(),
            "the default result shape remains unchanged"
        );
        assert_eq!(
            current["hint"],
            json!("call recall with a returned id to get that result's full output")
        );

        let other = execute_meta_tool(
            "recall",
            &json!({ "query": "shared brief marker", "session_id": "sess-other" }),
            Some(&store),
            "sess-current",
            &catalog,
        )
        .await
        .unwrap();
        assert_eq!(recall_match_ids(&other), vec![ids[1].clone()]);

        let all = execute_meta_tool(
            "recall",
            &json!({ "query": "shared brief marker", "all_sessions": true }),
            Some(&store),
            "sess-current",
            &catalog,
        )
        .await
        .unwrap();
        let mut actual = recall_match_ids(&all);
        actual.sort();
        let mut expected = ids.clone();
        expected.sort();
        assert_eq!(actual, expected);

        // A cross-session hit remains fetchable in full through the same
        // dispatcher when its returned session_id is passed back.
        let returned_other_session = all["matches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"].as_str() == Some(ids[1].as_str()))
            .and_then(|entry| entry["session_id"].as_str())
            .expect("all-session hits must identify their source session")
            .to_string();
        assert_eq!(returned_other_session, "sess-other");
        let fetched = execute_meta_tool(
            "recall",
            &json!({ "id": ids[1], "session_id": returned_other_session }),
            Some(&store),
            "sess-current",
            &catalog,
        )
        .await
        .unwrap();
        assert_eq!(fetched["id"], json!(ids[1]));

        let ambiguous = execute_meta_tool(
            "recall",
            &json!({
                "query": "shared brief marker",
                "session_id": "sess-other",
                "all_sessions": true
            }),
            Some(&store),
            "sess-current",
            &catalog,
        )
        .await
        .unwrap();
        assert!(
            ambiguous["error"]
                .as_str()
                .unwrap()
                .contains("cannot be combined")
        );
    }

    #[tokio::test]
    async fn recall_by_id_surfaces_failed_run_status_and_exit_code() {
        // VS3: recall must hand the model the OUTCOME of a prior run, not just
        // its output text — otherwise "last time this failed with exit -11" is
        // invisible. Seed a failed execute_python run and look it up by id.
        let store = ProvenanceStore::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        let mut failed = new_record(
            "sess-fail",
            ActionType::ToolCall,
            Actor::Agent,
            Some("execute_python"),
            None,
            json!({ "code": "import jax_md" }),
        );
        failed.output_json = Some(json!({
            "success": false, "exit_code": -11, "stderr": "SIGSEGV"
        }));
        failed.status = Some("error".to_string());
        failed.exit_code = Some(-11);
        store.record(&failed).await.unwrap();

        let out = recall(
            &json!({ "id": failed.id.clone() }),
            Some(&store),
            "sess-fail",
            None,
        )
        .await
        .unwrap();
        assert_eq!(out["status"], json!("error"));
        assert_eq!(out["exit_code"], json!(-11));
    }

    #[tokio::test]
    async fn recall_by_query_surfaces_status_in_match_list() {
        // The keyword-pass match objects (and semantic, same shape) must also
        // carry status/exit_code so the model sees outcome in a results list.
        let store = ProvenanceStore::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        let mut failed = new_record(
            "sess-fail",
            ActionType::ToolCall,
            Actor::Agent,
            Some("execute_python"),
            None,
            json!({ "code": "raise ValueError('boom')" }),
        );
        failed.output_json = Some(json!({ "success": false, "exit_code": 1, "stderr": "boom" }));
        failed.status = Some("error".to_string());
        failed.exit_code = Some(1);
        store.record(&failed).await.unwrap();

        let out = recall(&json!({ "query": "boom" }), Some(&store), "sess-fail", None)
            .await
            .unwrap();
        assert_eq!(out["count"], json!(1));
        assert_eq!(out["matches"][0]["status"], json!("error"));
        assert_eq!(out["matches"][0]["exit_code"], json!(1));
    }

    #[tokio::test]
    async fn recall_requires_id_or_query() {
        let (store, _) = seeded_store().await;
        let out = recall(&json!({}), Some(&store), "sess-recall", None)
            .await
            .unwrap();
        assert!(out["error"].as_str().unwrap().contains("either"));
    }

    #[tokio::test]
    async fn recall_without_store_is_graceful() {
        let out = recall(&json!({ "query": "x" }), None, "sess-recall", None)
            .await
            .unwrap();
        assert!(out["error"].as_str().unwrap().contains("unavailable"));
    }

    /// Deterministic embed stub: axis 0 fires on titanium-ish words, axis 1
    /// on everything else — "Ti-6Al-4V" lands next to "titanium" without
    /// sharing a substring, which is exactly what semantic recall adds.
    struct TitaniumAxis;

    #[async_trait::async_trait]
    impl EmbedBackend for TitaniumAxis {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    let t = t.to_lowercase();
                    if t.contains("titanium") || t.contains("ti-6al") {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    }
                })
                .collect())
        }
        fn dimensions(&self) -> usize {
            2
        }
        fn id(&self) -> &str {
            "test:titanium-axis"
        }
    }

    /// This specifically guards the store call site: changing
    /// `semantic_search(..., search_session_id, ...)` back to
    /// `semantic_search(..., Some(session_id), ...)` makes the explicit and
    /// all-session assertions fail. The query has no keyword overlap, so the
    /// fallback scan cannot mask a disconnected semantic scope.
    #[tokio::test]
    async fn recall_semantic_search_honors_resolved_session_scope() {
        let (store, ids) = cross_session_seeded_store().await;
        let backend = TitaniumAxis;
        for rec in store.query_all().await.unwrap() {
            store
                .embed_and_store(&rec.id, &prism_provenance::embedding_text(&rec), &backend)
                .await
                .unwrap();
        }

        let current = recall_with_backend(
            &json!({ "query": "titanium" }),
            Some(&store),
            "sess-current",
            Some(&backend),
            None,
        )
        .await
        .unwrap();
        assert_eq!(recall_match_ids(&current), vec![ids[0].clone()]);

        let other = recall_with_backend(
            &json!({ "query": "titanium", "session_id": "sess-other" }),
            Some(&store),
            "sess-current",
            Some(&backend),
            None,
        )
        .await
        .unwrap();
        assert_eq!(recall_match_ids(&other), vec![ids[1].clone()]);

        let all = recall_with_backend(
            &json!({ "query": "titanium", "all_sessions": true }),
            Some(&store),
            "sess-current",
            Some(&backend),
            None,
        )
        .await
        .unwrap();
        let mut actual = recall_match_ids(&all);
        actual.sort();
        let mut expected = ids;
        expected.sort();
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn recall_merges_semantic_and_keyword_and_dedupes() {
        let (store, r1_id) = seeded_store().await;
        let backend = TitaniumAxis;

        // r3: semantically titanium-ish but shares no substring with the
        // query — only the semantic pass can surface it.
        let mut r3 = new_record(
            "sess-recall",
            ActionType::ToolCall,
            Actor::Agent,
            Some("generate"),
            None,
            json!({ "alloy": "Ti-6Al-4V" }),
        );
        r3.output_json = Some(json!("candidate accepted"));
        store.record(&r3).await.unwrap();

        // Embed r1 (keyword AND semantic match → must dedupe) and r3.
        for id in [&r1_id, &r3.id] {
            let recs = store.query_chain(id, "sess-recall").await.unwrap();
            let rec = recs.into_iter().find(|r| &r.id == id).unwrap();
            store
                .embed_and_store(id, &prism_provenance::embedding_text(&rec), &backend)
                .await
                .unwrap();
        }

        let out = recall_with_backend(
            &json!({ "query": "titanium" }),
            Some(&store),
            "sess-recall",
            Some(&backend),
            None,
        )
        .await
        .unwrap();

        let matches = out["matches"].as_array().unwrap();
        let ids: Vec<&str> = matches.iter().map(|m| m["id"].as_str().unwrap()).collect();
        // Both surfaced, each exactly once (r1 hit both passes).
        assert!(ids.contains(&r1_id.as_str()));
        assert!(ids.contains(&r3.id.as_str()));
        assert_eq!(out["count"], json!(2));
        // Semantic hits come first and carry a score; both here are semantic.
        assert!(matches.iter().all(|m| m["score"].is_string()));
    }

    #[tokio::test]
    async fn recall_keyword_only_when_backend_missing() {
        let (store, _) = seeded_store().await;
        let out = recall_with_backend(
            &json!({ "query": "titanium" }),
            Some(&store),
            "sess-recall",
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(out["count"], json!(1));
        assert_eq!(out["matches"][0]["tool_name"], json!("file"));
        // Keyword matches carry no semantic score.
        assert!(out["matches"][0]["score"].is_null());
    }

    /// Seed a store with a mix of outcomes so list_failures has something to
    /// filter. Returns the store; failures are written into `sess-fail`.
    async fn failures_seeded_store() -> ProvenanceStore {
        let store = ProvenanceStore::open(std::path::Path::new(":memory:"))
            .await
            .unwrap();
        // Two failures, distinct exit codes (1 + the JAX-MD SIGSEGV shape -11).
        let mut f1 = new_record(
            "sess-fail",
            ActionType::ToolCall,
            Actor::Agent,
            Some("execute_python"),
            None,
            json!({ "code": "raise ValueError('boom')" }),
        );
        f1.output_json =
            Some(json!({ "success": false, "exit_code": 1, "stderr": "ValueError: boom" }));
        f1.status = Some("error".to_string());
        f1.exit_code = Some(1);
        f1.timestamp = "2026-01-01T00:00:00+00:00".to_string();
        let mut f2 = new_record(
            "sess-fail",
            ActionType::ToolCall,
            Actor::Agent,
            Some("execute_python"),
            None,
            json!({ "code": "import jax_md" }),
        );
        f2.output_json = Some(json!({
            "success": false, "exit_code": -11, "stderr": "SIGSEGV in native lib"
        }));
        f2.status = Some("error".to_string());
        f2.exit_code = Some(-11);
        f2.timestamp = "2026-01-02T00:00:00+00:00".to_string();
        // A success and a status-less row that must NOT appear.
        let mut ok = new_record(
            "sess-fail",
            ActionType::ToolCall,
            Actor::Agent,
            Some("execute_bash"),
            None,
            json!({ "cmd": "echo hi" }),
        );
        ok.output_json = Some(json!({ "success": true, "exit_code": 0 }));
        ok.status = Some("ok".to_string());
        ok.exit_code = Some(0);
        for rec in [f1, f2, ok] {
            store.record(&rec).await.unwrap();
        }
        store
    }

    #[tokio::test]
    async fn list_failures_returns_only_errors_newest_first() {
        let store = failures_seeded_store().await;
        let out = list_failures(&json!({}), Some(&store), "sess-fail")
            .await
            .unwrap();
        assert_eq!(out["count"], json!(2));
        let fails = out["failures"].as_array().unwrap();
        // Newest-first: f2 (Jan 2, exit -11) before f1 (Jan 1, exit 1).
        assert_eq!(fails[0]["exit_code"], json!(-11));
        assert_eq!(fails[0]["error"].as_str(), Some("SIGSEGV in native lib"));
        assert_eq!(fails[1]["exit_code"], json!(1));
        assert_eq!(fails[1]["error"].as_str(), Some("ValueError: boom"));
        // Every entry is a failure with a tool name + timestamp.
        for e in fails {
            assert_eq!(
                e["status"],
                json!(null),
                "status not echoed per-entry (it's implicit)"
            );
            assert!(e["tool_name"].as_str().is_some());
            assert!(e["timestamp"].as_str().is_some());
            assert!(e["id"].as_str().is_some());
        }
    }

    #[tokio::test]
    async fn list_failures_respects_limit() {
        let store = failures_seeded_store().await;
        let out = list_failures(&json!({ "limit": 1 }), Some(&store), "sess-fail")
            .await
            .unwrap();
        assert_eq!(out["count"], json!(1), "limit caps the returned failures");
    }

    #[tokio::test]
    async fn list_failures_without_store_is_graceful() {
        // Mirrors recall's "durable memory unavailable" contract — never panic.
        let out = list_failures(&json!({}), None, "sess-fail").await.unwrap();
        assert!(out["error"].as_str().unwrap().contains("unavailable"));
    }

    #[tokio::test]
    async fn list_failures_dispatches_via_execute_meta_tool() {
        // The registration/dispatch layer (what the agent loop calls), not just
        // the handler. Proves the META_TOOLS + definitions + match-arm wiring.
        let store = failures_seeded_store().await;
        let catalog = ToolCatalog::from_tool_server_json(&json!({ "tools": [] }));
        let out = execute_meta_tool(
            "list_failures",
            &json!({}),
            Some(&store),
            "sess-fail",
            &catalog,
        )
        .await
        .unwrap();
        assert_eq!(out["count"], json!(2));
    }

    /// VS1 fix-round #1: a stored skill can pass write-time verification yet
    /// FAIL on a later run (environment drift). run_skill must then report the
    /// failure via the `success`/`error` contract the shared is_error gate +
    /// provenance classifier read — not a bare `ok:false` that renders as a
    /// green "completed" card. We store a failing body directly (the same
    /// `skills::store` path write_skill uses after verification) to reach the
    /// "stored-then-broke" state deterministically.
    // The env guard is held across `.await` only to serialize `PRISM_SKILLS_DIR`
    // between tests; `#[tokio::test]` is single-threaded so this can't deadlock.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn run_skill_failure_emits_success_error_contract() {
        let (_g, _dir) = crate::skills::test_env_guard("meta-runskill-fail");
        // run_skill is execution-class: the gate requires node-owner access,
        // which this test simulates (TUI dispatch scopes turns the same way).
        crate::command_tools::with_platform_access(
            crate::command_tools::CommandToolPlatformAccess::VerifiedNodeOwner,
            run_skill_failure_emits_success_error_contract_owner(),
        )
        .await;
    }

    async fn run_skill_failure_emits_success_error_contract_owner() {
        let catalog = ToolCatalog::from_tool_server_json(&json!({ "tools": [] }));

        let drifted = crate::skills::AuthoredSkill::new(
            "drifted",
            "fails at run time",
            "shell",
            "exit 7",
            true,
        );
        crate::skills::store(&drifted).unwrap();

        let r = execute_meta_tool(
            "run_skill",
            &json!({ "name": "drifted" }),
            None,
            "",
            &catalog,
        )
        .await
        .unwrap();
        assert_eq!(r["ok"], json!(false), "failing skill reports ok:false: {r}");
        assert_eq!(
            r["success"],
            json!(false),
            "and success:false for the shared gate: {r}"
        );
        assert!(
            r["error"].is_string(),
            "and a string error so the gate/provenance catch it: {r}"
        );
        // The gate over the wrapped shape the agent loop actually builds.
        let wrapped = json!({ "result": r });
        assert!(
            crate::tool_result::tool_result_is_error(&wrapped),
            "a wrapped failed run_skill MUST classify as error: {wrapped}"
        );
    }

    /// The self-authoring (Voyager) loop end to end through the meta-tool layer:
    /// write_skill verifies-then-stores, a failing skill is rejected, list_skills
    /// shows only the verified one, run_skill re-executes it, and it surfaces to
    /// the capability/retrieval layer. Deterministic (shell only, temp dir).
    // The env guard is held across `.await` only to serialize `PRISM_SKILLS_DIR`
    // between tests; `#[tokio::test]` is single-threaded so this can't deadlock.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn write_skill_verifies_stores_lists_and_runs() {
        let (_g, _dir) = crate::skills::test_env_guard("meta-writeskill");
        // write_skill/run_skill are execution-class: the gate requires
        // node-owner access, which this test simulates (TUI dispatch scopes
        // turns the same way; see protocol::spawn_agent_turn).
        crate::command_tools::with_platform_access(
            crate::command_tools::CommandToolPlatformAccess::VerifiedNodeOwner,
            write_skill_verifies_stores_lists_and_runs_owner(),
        )
        .await;
    }

    async fn write_skill_verifies_stores_lists_and_runs_owner() {
        let catalog = ToolCatalog::from_tool_server_json(&json!({ "tools": [] }));

        // 1. A valid shell skill: verified by running once, then stored.
        let w = execute_meta_tool(
            "write_skill",
            &json!({
                "name": "say_hi",
                "description": "print a greeting to stdout",
                "language": "shell",
                "code": "echo hello-from-skill",
            }),
            None,
            "",
            &catalog,
        )
        .await
        .unwrap();
        assert_eq!(w["stored"], json!(true), "valid skill must store: {w}");
        assert_eq!(w["verified"], json!(true));

        // 2. A skill that exits non-zero is REJECTED, not stored.
        let bad = execute_meta_tool(
            "write_skill",
            &json!({
                "name": "broken",
                "description": "exits non-zero",
                "language": "shell",
                "code": "exit 7",
            }),
            None,
            "",
            &catalog,
        )
        .await
        .unwrap();
        assert_eq!(
            bad["stored"],
            json!(false),
            "failing skill must NOT store: {bad}"
        );
        assert_eq!(bad["verified"], json!(false));

        // 3. Only the verified skill is listed.
        let list = execute_meta_tool("list_skills", &json!({}), None, "", &catalog)
            .await
            .unwrap();
        assert_eq!(
            list["count"],
            json!(1),
            "only verified skill listed: {list}"
        );
        assert_eq!(list["skills"][0]["name"], json!("say_hi"));

        // 4. run_skill re-executes the stored skill and returns its output.
        let r = execute_meta_tool(
            "run_skill",
            &json!({ "name": "say_hi" }),
            None,
            "",
            &catalog,
        )
        .await
        .unwrap();
        assert_eq!(r["ok"], json!(true), "run must succeed: {r}");
        assert!(
            r["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("hello-from-skill"),
            "stdout must carry the skill output: {r}"
        );

        // 5. The stored skill surfaces to the capability/retrieval layer.
        assert_eq!(
            crate::skills::retrieval_entries(),
            vec![(
                "say_hi".to_string(),
                "say_hi: print a greeting to stdout".to_string()
            )]
        );
    }

    /// Both storage formats coexist in one inventory. An explicit-only human
    /// procedure stays un-runnable until the turn resolver records `$name`,
    /// after which the existing approval-class `run_skill` boundary returns
    /// instructions without executing their contents.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn list_skills_includes_authored_and_human_and_human_run_needs_selection() {
        let (_guard, root) = crate::skills::test_env_guard("meta-human-skill");
        crate::skills::store(&crate::skills::AuthoredSkill::new(
            "generated",
            "agent generated code",
            "shell",
            "echo generated",
            true,
        ))
        .unwrap();
        let human_dir = root.join("owner-review");
        std::fs::create_dir_all(&human_dir).unwrap();
        std::fs::write(
            human_dir.join("SKILL.md"),
            "---\nname: owner-review\ndescription: Follow the owner's review procedure\npolicy:\n  allow_implicit_invocation: false\n---\n# Review\nRun each command through normal approval gates.\n",
        )
        .unwrap();
        let catalog = ToolCatalog::from_tool_server_json(&json!({ "tools": [] }));

        let listed = execute_meta_tool("list_skills", &json!({}), None, "", &catalog)
            .await
            .unwrap();
        assert_eq!(listed["count"], json!(2), "both formats list: {listed}");
        let kinds = listed["skills"]
            .as_array()
            .unwrap()
            .iter()
            .map(|skill| {
                (
                    skill["name"].as_str().unwrap(),
                    skill["kind"].as_str().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert!(kinds.contains(&("generated", "authored")));
        assert!(kinds.contains(&("owner-review", "human")));

        let implicit_error = crate::command_tools::with_platform_access(
            crate::command_tools::CommandToolPlatformAccess::VerifiedNodeOwner,
            execute_meta_tool(
                "run_skill",
                &json!({ "name": "owner-review" }),
                None,
                "",
                &catalog,
            ),
        )
        .await
        .expect_err("explicit-only Markdown must not run implicitly");
        assert!(
            implicit_error
                .to_string()
                .contains("forbids implicit invocation")
        );

        let context = crate::skills::prepare_turn_skill_context(
            "Follow $owner-review",
            &root,
            &crate::skills::SkillSurfacePolicy::default(),
        )
        .unwrap();
        let loaded = crate::command_tools::with_platform_access(
            crate::command_tools::CommandToolPlatformAccess::VerifiedNodeOwner,
            crate::skills::with_turn_skill_context(
                context,
                execute_meta_tool(
                    "run_skill",
                    &json!({ "name": "owner-review" }),
                    None,
                    "",
                    &catalog,
                ),
            ),
        )
        .await
        .unwrap();
        assert_eq!(loaded["kind"], json!("human"));
        assert_eq!(loaded["trust"], json!("untrusted"));
        assert!(
            loaded["instructions"]
                .as_str()
                .unwrap()
                .contains("normal approval gates")
        );
        assert!(
            loaded["stdout"].is_null(),
            "Markdown content is loaded, never directly executed: {loaded}"
        );
    }

    // ── VS1 / F2: skill stderr head+tail keeps the final Error line ────

    #[test]
    fn f2_clip_head_tail_preserves_final_error_line() {
        // A Python-style traceback: header context at the front, the real
        // `ValueError: ...` line at the END. Head-only clipping would drop it.
        let frames = "Traceback (most recent call last):\n".to_string()
            + &"  File \"skill.py\", line N, in run\n    pass\n".repeat(60);
        let stderr = frames + "ValueError: x must be positive\n";
        let clipped = clip_head_tail(&stderr, 500, 1500);

        // The tail must survive — that's the whole point.
        assert!(
            clipped.contains("ValueError: x must be positive"),
            "final error line must survive head+tail clip: {clipped}"
        );
        // The traceback header should still be there too.
        assert!(
            clipped.contains("Traceback (most recent call last)"),
            "traceback header must survive head+tail clip: {clipped}"
        );
        // The elision must be announced, not silent.
        assert!(
            clipped.contains("chars elided"),
            "elided middle must be marked explicitly: {clipped}"
        );
    }

    #[test]
    fn f2_clip_head_tail_short_string_unchanged() {
        // Below the head+tail budget: returned verbatim, no marker.
        let s = "Traceback (most recent call last):\nValueError: boom\n";
        assert_eq!(clip_head_tail(s, 500, 1500), s);
    }

    #[test]
    fn f2_clip_head_tail_multibyte_safe() {
        // Whole-char boundaries: a multibyte emoji must not be split at the
        // head/tail seams. The head and tail windows land on char starts.
        let head_pad = "a".repeat(400);
        let tail_pad = "b".repeat(1400);
        // Surround a multibyte char with ASCII so the seam could land mid-codepoint.
        let s = format!("{head_pad}😀{tail_pad}");
        let clipped = clip_head_tail(&s, 500, 1500);
        // The string should be valid UTF-8 (no panic) — clip_head_tail returns
        // a String, so this is enforced by construction. Assert no replacement
        // char and that the emoji survives somewhere in the tail.
        assert!(
            !clipped.contains('\u{FFFD}'),
            "no replacement char from a split codepoint: {clipped}"
        );
    }
}
