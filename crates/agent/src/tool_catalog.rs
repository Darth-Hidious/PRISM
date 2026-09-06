use prism_ingest::llm::{FunctionDef, ToolDefinition};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::permissions::{PermissionMode, get_tool_permission};

/// Bytes of serialized OpenAI tool JSON per model token.
///
/// MEASURED on the real catalog, not guessed: the 54 Python tool definitions
/// serialize to 71,577 bytes, which tokenize to 17,758 tokens on Mistral's
/// tokenizer (4.03 B/tok), 16,577 on `o200k_base` (4.32) and 16,249 on
/// `cl100k_base` (4.41). Charging 4 over-estimates against every one of those,
/// so the budget can be under-spent but never blown. Re-measure and update this
/// number if the catalog's shape changes materially.
const BYTES_PER_TOKEN: usize = 4;

/// Ceiling on tool-definition tokens per request, whatever the context window.
/// Sized from the measured live catalog so today's full surface fits with
/// headroom; past that the escape hatch (`find_tools`) starts mattering again.
pub const MAX_TOOL_TOKENS: usize = 32_768;

/// Floor, so a small-context model still gets the meta-tools plus a couple of
/// real ones rather than meta-tools alone (currently 1,536 charged tokens,
/// guarded where the native definitions are declared).
pub const MIN_TOOL_TOKENS: usize = 2_048;

/// How many catalog tools one request may carry, on top of the always-included
/// core, pinned tools, and the meta-tools.
///
/// The token budget alone never restrained anything. Retrieval ranks the WHOLE
/// catalog and `finalize_tools` walked that ranking until the budget ran out —
/// but the whole catalog costs ~12,289 tokens against a budget that clamps at
/// 32,768, so the budget was never the binding constraint and every tool
/// shipped on every request. Progressive disclosure existed and was inert.
///
/// Measured 2026-08-19: a PFAS literature review on glm-5.2 spent its ENTIRE
/// 200,000-token budget on re-sending 171 definitions — 12,289 x 17 requests =
/// 208,913, against 207,689 observed. It stopped at round 4 of a saturation
/// task with zero tokens left for the answer. The tool block, not the research,
/// consumed the run.
///
/// A COUNT cap is what the token cap could not be: the cost of a request grows
/// with turns because the block is re-sent every time, so the ranking has to be
/// truncated, not merely afforded. What does not fit is not hidden — it is
/// listed in the L1 capability menu (`capability::capability_menu`) and
/// retrievable by name through the `find_tools` meta-tool, which is how the
/// model was always meant to discover capability.
pub const MAX_REQUEST_TOOLS: usize = 24;

/// Share of the model's context window spendable on tool definitions (1/N).
/// At 1/4 the whole catalog reaches every 128k-or-larger model — including
/// `ministral-3b` (131k) — while [`MAX_TOOL_TOKENS`] keeps a 262k model at
/// 12.5%. Smaller models spend a quarter of their window on tools and truncate
/// by relevance, which beats being blind to capability they have.
const CONTEXT_TOOL_SHARE: usize = 4;

/// Token budget for tool definitions on a model with `context_window` tokens.
///
/// Replaces the old fixed `MAX_TOOLS_PER_REQUEST = 15` count cap. A count cap
/// sized for the worst case starves every normal case: 15 of 131 tools left the
/// model blind to capability it had, and "call `find_tools` if you need
/// something else" is an instruction models reliably ignore — not looking is
/// free and nothing catches it. A budget lets the whole catalog through when it
/// fits and degrades by relevance only when it genuinely cannot.
#[must_use]
pub fn tool_token_budget(context_window: usize) -> usize {
    (context_window / CONTEXT_TOOL_SHARE).clamp(MIN_TOOL_TOKENS, MAX_TOOL_TOKENS)
}

/// Token cost of one tool definition as the provider will see it: the exact
/// JSON bytes sent on the wire, divided by the measured [`BYTES_PER_TOKEN`].
#[must_use]
pub fn definition_tokens(def: &ToolDefinition) -> usize {
    serde_json::to_string(def)
        .map_or(0, |s| s.len())
        .div_ceil(BYTES_PER_TOKEN)
}

/// Tools that survive keyword filtering in every session.
///
/// Module-level rather than buried in the filter so `stale_tool_names_are_gone`
/// can actually read it. It named the removed `search_materials` for months,
/// which meant the federated OPTIMADE search reached the model only when the
/// user's phrasing happened to keyword-match it — on a materials platform, the
/// one tool that must always be on the table.
///
/// `query_local` earns its place the hard way. Measured: asked to "search our
/// ingested knowledge graph for tunnel magnetoresistance", the agent called
/// `query_platform` — the REMOTE store — got no matches, retried with
/// `semantic=true`, and died on HTTP 402 insufficient credits, while the answer
/// sat in the local graph. Told explicitly "use the query_local tool (not
/// query_platform)", it did the same thing again: a model cannot call a tool it
/// was never offered. `query_platform` was on this list and `query_local` was
/// not, so on any keyword miss the ONLY tool that reads this machine's graph
/// disappeared from the session while its billed remote sibling stayed.
///
/// The umbrella `query` does not cover the gap. Its description ("run `prism
/// query ...`") says nothing about which store it reads, so it loses to a
/// sibling that describes itself confidently.
/// `query` now carries every store behind its `scope` argument, so pinning the
/// one name pins all three. The old four-name list existed because the local
/// and platform tools could be admitted independently — and once were, wrongly.
/// `papers_ingest` is here because a research harness that cannot WRITE is a
/// browser. Every other name on this list reads; without it, the one tool that
/// turns a paper into stored knowledge competes for a slot against ~167
/// candidates and can lose.
///
/// Measured, the PFAS run of 2026-08-27: 156 tool calls — 48 web_browse, 25
/// web, 22 prior_art_search — and ZERO calls to any ingest tool, against a
/// brief that said "ingest the papers that carry the evidence rather than only
/// listing them". Nothing was stored. That is the same failure the research
/// DAG comment in `orchestrator` records ("17 searches, nothing persisted, no
/// report"), and the same lesson the paragraph above learned for
/// `query_local`: a model cannot call a tool it was never offered.
///
/// It is the LOCAL writer (`papers claims --store`) on purpose. The hosted
/// `ingest_and_wait` bills, and a guaranteed slot must not be one that fails
/// closed on an empty balance.
pub const ALWAYS_INCLUDE: &[&str] = &["query", "materials_search", "papers_ingest"];

/// Tool names this codebase has renamed away from.
///
/// A stale name in any model-facing list is not a cosmetic bug: the model is
/// told a tool exists, calls it, and gets an error it cannot act on. This
/// shipped once already (PR #91, `search_materials` → `materials_search`) and
/// the guard written for it covered only the prompt block — so the same dead
/// name survived in three other lists. Anything added here is checked against
/// all of them.
pub const RENAMED_AWAY: &[&str] = &[
    "search_materials",
    // Collapsed into `query(scope=local|platform|federated)`. They remain
    // executable as aliases, but they are no longer OFFERED names, so no
    // model-facing list may name them.
    "query_local",
    "query_platform",
    "query_federated",
    "knowledge_search",
    "predict_property",
    "semantic_search",
    // Collapsed into the unified `file` tool (read|write|edit) and the
    // read-only `bash_task` tool (list|read) when `app/tools/system.py` and
    // `app/tools/bash.py` were rewritten. The registry stopped serving these
    // five, but `/read`, `/write`, `/edit`, `/bash tasks` and `/bash read`
    // kept sending them, so all five slash commands failed on an unregistered
    // tool — the PR #91 failure again, five times over.
    "read_file",
    "write_file",
    "edit_file",
    "list_bash_tasks",
    "read_bash_task",
];

/// Full metadata for one loaded tool. Rust keeps this alongside the OpenAI
/// function definition so command views, permission logic, and approval UI all
/// talk about the same concrete tool facts.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub requires_approval: bool,
    /// The tool itself said, in so many words, that it needs no approval.
    ///
    /// Distinct from `!requires_approval`, which is also true when the author
    /// never decided. Only an affirmative declaration lets a call skip the
    /// prompt; silence is gated.
    pub declared_free: bool,
    pub permission_mode: PermissionMode,
    pub source: Option<String>,
    pub source_detail: Option<String>,
}

impl LoadedTool {
    #[must_use]
    pub fn to_definition(&self) -> ToolDefinition {
        ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: self.name.clone(),
                description: self.description.clone(),
                parameters: self.input_schema.clone(),
            },
        }
    }
}

/// Catalog of tools loaded from the Python registry. The LLM still receives
/// plain `ToolDefinition`s, but the runtime keeps the richer metadata here.
/// The catalog WITHOUT any MCP tools — the fixed half, captured once.
static BASE: std::sync::OnceLock<ToolCatalog> = std::sync::OnceLock::new();
/// Base + whichever MCP tools are currently connected. Replaced by
/// [`rebuild_live`]; read by each turn as it starts.
static LIVE: std::sync::RwLock<Option<Arc<ToolCatalog>>> = std::sync::RwLock::new(None);

/// Publish the startup catalog and fold in the MCP tools connected so far.
///
/// The base is kept separately so a later reload can rebuild from it. Without
/// that, repeated reloads would either accumulate stale MCP tools or need the
/// whole catalog rebuilt from the tool server again.
pub fn install_live(
    base: ToolCatalog,
    mcp_tools: Vec<LoadedTool>,
) -> (Arc<ToolCatalog>, Vec<String>) {
    let _ = BASE.set(base);
    let rejected = rebuild_live(mcp_tools)
        .expect("the base was just installed, so a rebuild cannot be a no-op");
    (
        live().expect("the live catalog exists once the base is installed"),
        rejected,
    )
}

/// Rebuild the live catalog as base + `mcp_tools`, returning the names refused
/// for colliding with an existing tool.
///
/// Rebuilt from the BASE every time, never extended in place, so a server
/// removed from the config actually disappears instead of lingering because
/// nothing removed it.
///
/// `None` means no base has been published yet — nothing was rebuilt. It is
/// returned rather than swallowed because a reload that quietly changed
/// nothing while reporting success is the failure this whole change exists to
/// remove.
pub fn rebuild_live(mcp_tools: Vec<LoadedTool>) -> Option<Vec<String>> {
    let base = BASE.get()?;
    let mut catalog = base.clone();
    let rejected = catalog.extend_untrusted(mcp_tools);
    *LIVE.write().expect("live catalog poisoned") = Some(Arc::new(catalog));
    Some(rejected)
}

/// The catalog for a turn about to start: the live one when the agent has
/// published it, else the caller's own.
///
/// Called as each turn begins, which is what makes a reload take effect on the
/// NEXT turn and not the running one. Swapping the catalog mid-turn would let
/// the model call against a list it was never shown.
#[must_use]
pub fn live_or(fallback: &Arc<ToolCatalog>) -> Arc<ToolCatalog> {
    live().unwrap_or_else(|| Arc::clone(fallback))
}

/// The catalog a turn should use. `None` before the agent has started.
#[must_use]
pub fn live() -> Option<Arc<ToolCatalog>> {
    LIVE.read().expect("live catalog poisoned").clone()
}

#[derive(Debug, Clone, Default)]
pub struct ToolCatalog {
    tools: Vec<LoadedTool>,
    definitions: Vec<ToolDefinition>,
}

/// Lower-case words of at least three letters, split on anything that is not
/// a letter or digit — so `cache_ref` yields `cache` and `mace_get_cached_structure`
/// yields four tokens rather than one unmatchable string.
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(str::to_string)
        .collect()
}

/// A prefix match in either direction, but only between words long enough to
/// mean something: `cache`/`cached` yes, `get`/`getting` no.
fn prefix_of(a: &str, b: &str) -> bool {
    a.len() >= 4 && b.len() >= 4 && (a.starts_with(b) || b.starts_with(a))
}

impl ToolCatalog {
    #[must_use]
    pub fn from_tool_server_json(tools_json: &Value) -> Self {
        let empty = Vec::new();
        let raw_tools = tools_json
            .get("tools")
            .and_then(|value| value.as_array())
            .unwrap_or(&empty);

        let mut tools: Vec<LoadedTool> = Vec::with_capacity(raw_tools.len());
        let mut admitted: std::collections::HashSet<String> = std::collections::HashSet::new();

        for tool in raw_tools {
            let Some(name) = tool.get("name").and_then(Value::as_str) else {
                continue;
            };
            let name = name.to_string();
            let Some(description) = tool.get("description").and_then(Value::as_str) else {
                continue;
            };
            let description = description.to_string();
            let source = tool
                .get("source")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);

            // ANTI-SPOOFING: a tool from an UNTRUSTED source (external MCP
            // server, user-brought / custom / plugin) may NOT take a reserved
            // built-in name (meta-tool or command-tool) nor shadow a tool
            // already admitted this build. The agent must never be tricked
            // into running an impostor under a trusted name — outside tools
            // must use a distinct name. Trusted first-party tools (builtin,
            // papers, science-sidecar, unset source) are never gated here.
            let name_lc = name.to_ascii_lowercase();
            if is_untrusted_source(source.as_deref())
                && (crate::meta_tools::is_reserved_tool_name(&name) || admitted.contains(&name_lc))
            {
                tracing::warn!(
                    tool = %name,
                    source = source.as_deref().unwrap_or("unknown"),
                    "rejected untrusted tool: name collides with a reserved built-in \
                     or an already-loaded tool — rename it to a distinct name and re-register",
                );
                continue;
            }

            // TOOL_SURFACE_SPEC D3: a tool MUST carry a real input_schema. When
            // it is absent we keep loading (so v1 doesn't break on an older
            // tool server) but surface the gap loudly — a description-less or
            // schema-less tool silently degrades model selection/arg-filling.
            let schema_present = tool.get("input_schema").is_some();
            let input_schema = tool.get("input_schema").cloned().unwrap_or_else(
                || json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            );
            if !schema_present {
                tracing::warn!(
                    tool = %name,
                    "tool loaded without an input_schema — defaulting to empty \
                     {{type:object}}. Give it a typed schema (SPEC D3) or an \
                     explicit honest-empty with additionalProperties:false",
                );
            }
            // Absent or null means the tool's author never decided. That is
            // NOT the same as "free" — it is gated, so forgetting to think
            // about a tool can never silently make it auto-run. Only an
            // explicit `false` from the tool itself buys it past the prompt.
            let requires_approval = tool
                .get("requires_approval")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let declared_free = tool
                .get("requires_approval")
                .and_then(Value::as_bool)
                .is_some_and(|value| !value);
            let source_detail = tool
                .get("source_detail")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);

            admitted.insert(name_lc);
            tools.push(LoadedTool {
                permission_mode: get_tool_permission(&name),
                name,
                description,
                input_schema,
                requires_approval,
                declared_free,
                source,
                source_detail,
            });
        }

        let definitions = tools.iter().map(LoadedTool::to_definition).collect();
        Self { tools, definitions }
    }

    #[must_use]
    pub fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &LoadedTool> {
        self.tools.iter()
    }

    #[must_use]
    pub fn find(&self, tool_name: &str) -> Option<&LoadedTool> {
        self.tools
            .iter()
            .find(|tool| tool.name.eq_ignore_ascii_case(tool_name))
    }

    /// Keyword search over the catalog, returning up to `limit` matching tools
    /// ranked by relevance. Backs the `find_tools` meta-tool (runtime
    /// discovery) — lightweight name/description matching, no embeddings.
    #[must_use]
    pub fn search(&self, query: &str, limit: usize) -> Vec<&LoadedTool> {
        let q = query.to_lowercase();
        let words = tokens(&q);
        let mut scored: Vec<(usize, &LoadedTool)> = self
            .tools
            .iter()
            .map(|t| {
                let name = t.name.to_lowercase();
                let name_toks = tokens(&name);
                let desc_toks = tokens(&t.description.to_lowercase());
                let mut score = 0usize;
                let mut matched = 0usize;
                // The query names the tool outright. Only a multi-token name
                // earns this: `status` is a substring of "job status", and the
                // +10 that gave it put a bare CLI passthrough above five MACE
                // tools in a query that began with the word MACE.
                if name.contains('_') && q.contains(&name) {
                    score += 10;
                }
                for w in &words {
                    // Best single credit per query word: an exact name token,
                    // then a prefix of one (`cache` → `cached`), then the
                    // description. First hit wins so a word is not counted twice.
                    let credit = if name_toks.iter().any(|t| t == w) {
                        5
                    } else if name_toks.iter().any(|t| prefix_of(t, w)) {
                        3
                    } else if desc_toks.iter().any(|t| t == w) {
                        2
                    } else if desc_toks.iter().any(|t| prefix_of(t, w)) {
                        1
                    } else {
                        0
                    };
                    if credit > 0 {
                        matched += 1;
                        score += credit;
                    }
                }
                // Breadth beats a lucky single word: three of four query words
                // matched must outrank one bare name hit, or every tool with
                // "structure" in its name ties and catalog order decides.
                if matched > 1 {
                    score += 4 * (matched - 1);
                }
                (score, t)
            })
            .filter(|(s, _)| *s > 0)
            .collect();
        scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        scored.into_iter().take(limit).map(|(_, t)| t).collect()
    }

    pub fn extend(&mut self, extra_tools: Vec<LoadedTool>) {
        for tool in extra_tools {
            self.tools
                .retain(|loaded| !loaded.name.eq_ignore_ascii_case(&tool.name));
            self.tools.push(tool);
        }
        self.definitions = self.tools.iter().map(LoadedTool::to_definition).collect();
    }

    /// Merge tools from an UNTRUSTED source (user-brought, MCP, self-authored).
    /// Unlike [`Self::extend`] (trusted, last-writer-wins), this REFUSES to let
    /// an untrusted tool shadow a reserved built-in (meta-tool / command-tool)
    /// or any tool already in the catalog — the anti-spoofing invariant: outside
    /// tools must use a distinct name, so the agent can never be tricked into
    /// running an impostor under a trusted name. Returns the rejected names so
    /// the caller can report them ("rename and re-register").
    #[must_use]
    pub fn extend_untrusted(&mut self, extra_tools: Vec<LoadedTool>) -> Vec<String> {
        let mut rejected = Vec::new();
        for tool in extra_tools {
            let shadows_reserved = crate::meta_tools::is_reserved_tool_name(&tool.name);
            let shadows_existing = self
                .tools
                .iter()
                .any(|loaded| loaded.name.eq_ignore_ascii_case(&tool.name));
            if shadows_reserved || shadows_existing {
                rejected.push(tool.name);
                continue;
            }
            self.tools.push(tool);
        }
        self.definitions = self.tools.iter().map(LoadedTool::to_definition).collect();
        rejected
    }

    /// Every tool name currently in the catalog, in catalog order.
    #[must_use]
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|tool| tool.name.clone()).collect()
    }

    /// Rank the WHOLE catalog by keyword relevance to `query`, most relevant
    /// first. Lightweight name/description matching — no embedding server.
    ///
    /// Ranking only; nothing is dropped here. Truncation is the token budget's
    /// job (`agent_loop::finalize_tools`), so a tool is excluded because the
    /// request genuinely cannot afford it — never because of an arbitrary count.
    #[must_use]
    pub fn names_by_relevance(&self, query: &str) -> Vec<String> {
        if query.trim().is_empty() {
            return self.tools.iter().map(|t| t.name.clone()).collect();
        }

        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower
            .split_whitespace()
            .filter(|w| w.len() > 2)
            .collect();

        let mut scored: Vec<(usize, &LoadedTool)> = self
            .tools
            .iter()
            .map(|tool| {
                let name_lower = tool.name.to_lowercase();
                let desc_lower = tool.description.to_lowercase();

                let mut score = 0usize;

                // Exact name match — highest signal
                if query_lower.contains(&name_lower) {
                    score += 10;
                }

                // Query words appearing in tool name
                for word in &query_words {
                    if name_lower.contains(word) {
                        score += 5;
                    }
                }

                // Query words appearing in description
                for word in &query_words {
                    if desc_lower.contains(word) {
                        score += 2;
                    }
                }

                // Always-include tools get a floor score
                if ALWAYS_INCLUDE.contains(&tool.name.as_str()) {
                    score += 1;
                }

                (score, tool)
            })
            .collect();

        // Stable sort by score descending: equal-scoring (including zero-scoring)
        // tools keep catalog order, so the tail is deterministic.
        scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        scored
            .into_iter()
            .map(|(_, tool)| tool.name.clone())
            .collect()
    }
}

/// First-person incapacity markers. Paired with [`CAPABILITY_MARKERS`] below.
/// The past-tense forms are here because that is how the failure actually reads
/// in the wild — the live capped run produced "I couldn't find any graphics card
/// rental tools", not a tidy "I don't have a tool".
const INCAPACITY_MARKERS: &[&str] = &[
    "can't",
    "cannot",
    "can not",
    "couldn't",
    "could not",
    "didn't find",
    "did not find",
    "unable",
    "don't have",
    "do not have",
    "no access",
    "lack",
    "no way to",
];

/// Words that make an incapacity statement about TOOLING rather than about the
/// subject matter. "I can't tell from the abstract" is a hedge; "I don't have a
/// tool for that" is a capability gap.
const CAPABILITY_MARKERS: &[&str] = &[
    "tool",
    "access",
    "capability",
    "capabilities",
    "function",
    "api",
];

/// Extract the sentence in which the model admitted a CAPABILITY gap, to be
/// used verbatim as a re-retrieval query. `None` for ordinary turns.
///
/// Structural, not prompted: "call `find_tools` if you need something else" is
/// an instruction models ignore, because not-looking is free and nothing
/// catches it. This is the thing that catches it — the harness re-retrieves on
/// the model's own words instead of asking it to.
///
/// A sentence qualifies only when all three hold, which is what keeps it off
/// ordinary conversational turns:
///  1. it is FIRST PERSON (`i `/`i'`/`my `) — "you can't heat-treat above 900 °C"
///     is subject-matter talk, not a gap;
///  2. it contains an incapacity marker; and
///  3. it contains a capability/tooling marker — "I can't tell from the abstract
///     whether it was homogenised" is a hedge about evidence, not about tools.
#[must_use]
pub fn capability_gap_query(text: &str) -> Option<String> {
    text.split_terminator(['.', '!', '?', '\n'])
        .map(str::trim)
        .find(|sentence| {
            let s = sentence.to_lowercase();
            let first_person = s.starts_with("i ")
                || s.starts_with("i'")
                || s.contains(" i ")
                || s.contains(" i'");
            first_person
                && INCAPACITY_MARKERS.iter().any(|m| s.contains(m))
                && CAPABILITY_MARKERS.iter().any(|m| s.contains(m))
        })
        .map(ToOwned::to_owned)
}

/// Sources considered UNTRUSTED for anti-spoofing: external MCP servers and
/// user-brought / custom / plugin tools. First-party sources (builtin, papers,
/// science-sidecar) and an unset source are trusted and never gated.
fn is_untrusted_source(source: Option<&str>) -> bool {
    matches!(
        source.map(str::to_ascii_lowercase).as_deref(),
        Some("mcp" | "custom" | "custom_loader" | "user" | "plugin" | "marketplace" | "external")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every model-facing tool list, checked against the names we renamed away.
    ///
    /// The existing guard in `prism_llm` covers only the quick-reference prompt
    /// block. When `search_materials` became `materials_search`, that guard went
    /// green while the dead name survived here, in `CORE_TOOL_SET`, and in the
    /// permission map — so a weak model never got the materials tool at all and
    /// the live tool fell through to the `WorkspaceWrite` default. One list
    /// being guarded is not the same as the drift being caught.
    /// The complement of the RENAMED_AWAY guard below, and the bug that guard
    /// cannot see: a name that is NOT renamed away, IS a real command tool, and
    /// is still never offered because a filter removes it.
    ///
    /// `CORE_TOOL_SET` named `"query"` while `REDUNDANT_UMBRELLA_TOOLS`
    /// guaranteed it was never in the catalog — so the weak models that get
    /// ONLY that curated list were pointed at a name filtered out as absent.
    /// Its own comment records the identical bug already shipping for the three
    /// split `file` names. A curated list of tool names is a second registry,
    /// and nothing was keeping it in sync with the first.
    #[test]
    fn curated_lists_only_name_tools_the_catalog_actually_offers() {
        for (list_name, names) in [
            ("ALWAYS_INCLUDE", ALWAYS_INCLUDE),
            ("CORE_TOOL_SET", crate::prompt_profile::CORE_TOOL_SET),
        ] {
            for name in names {
                // Python tool-server names cannot be resolved from here; this
                // guard covers the half that CAN be checked, which is the half
                // that broke.
                if !crate::command_tools::is_command_tool(name) {
                    continue;
                }
                for node_online in [true, false] {
                    assert!(
                        crate::command_tools::command_tools_filtered(node_online)
                            .iter()
                            .any(|tool| tool.name == *name),
                        "{list_name} names `{name}`, which IS a command tool but \
                         is filtered out of the offered catalog (node_online={node_online}) \
                         — the model is told to use a tool it is never given"
                    );
                }
            }
        }
    }

    #[test]
    fn stale_tool_names_are_gone_from_every_model_facing_list() {
        for stale in RENAMED_AWAY {
            assert!(
                !ALWAYS_INCLUDE.contains(stale),
                "ALWAYS_INCLUDE still names the removed tool `{stale}`"
            );
            assert!(
                !crate::prompt_profile::CORE_TOOL_SET.contains(stale),
                "CORE_TOOL_SET still names the removed tool `{stale}`"
            );
        }
    }

    /// THE USER'S OWN GRAPH MUST ALWAYS BE ON THE TABLE.
    ///
    /// Regression for a measured failure: `query_platform` was always
    /// included and `query_local` was not, so on any keyword miss the only
    /// tool that reads THIS machine's graph vanished while its billed remote
    /// sibling stayed. Asked to "search our ingested knowledge graph for
    /// tunnel magnetoresistance", the agent queried the remote store, found
    /// nothing, retried semantically and hit HTTP 402 insufficient credits —
    /// with the answer in the local graph the whole time. Naming the tool
    /// explicitly in the prompt did not help, because a model cannot call a
    /// tool it was never offered.
    ///
    /// Asymmetry is the specific defect: whichever way the pair is filtered,
    /// they go together, or the model is handed the paid option and denied
    /// the free one.
    #[test]
    fn the_local_graph_is_always_offered_alongside_the_platform() {
        // The pair can no longer be filtered apart: there is ONE `query` tool
        // and the store is chosen by `scope`. So the invariant moves down a
        // level — it is now about the scope enum, not about two tool names.
        assert!(
            ALWAYS_INCLUDE.contains(&"query"),
            "the user's own ingested graph must survive keyword filtering"
        );
        for stale in ["query_local", "query_platform", "query_federated"] {
            assert!(
                !ALWAYS_INCLUDE.contains(&stale),
                "`{stale}` is no longer an offered name — it is a `query` scope"
            );
        }

        let spec = crate::command_tools::command_tools_filtered(false)
            .into_iter()
            .find(|tool| tool.name == "query")
            .expect(
                "query must be offered with the node OFFLINE — the local \
                     store is a file on disk and needs no node",
            );
        let scopes = spec.input_schema["properties"]["scope"]["enum"]
            .as_array()
            .expect("scope is an enum")
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        assert!(
            scopes.contains(&"local"),
            "the free local store must always be reachable"
        );
        assert_eq!(
            scopes.contains(&"local"),
            scopes.contains(&"platform"),
            "local and platform must be offered together — offering only the \
             billed remote one is how the agent skipped the user's own data"
        );
        assert_eq!(
            spec.input_schema["properties"]["scope"]["default"], "local",
            "an unspecified scope must read the user's FREE local graph, never \
             the billed remote one"
        );

        assert!(
            crate::prompt_profile::CORE_TOOL_SET.contains(&"query"),
            "a weak model must be able to read the local graph without credits"
        );
        assert_eq!(
            get_tool_permission("query"),
            PermissionMode::ReadOnly,
            "reading the user's own local graph must not require approval"
        );
    }

    /// The +1 relevance floor is NOT a guarantee, and pretending otherwise is
    /// what let this fail in the wild.
    ///
    /// A keyword hit in a tool NAME scores +5; the always-include floor is +1.
    /// So a query containing "query" ranks `query_materials_project` ABOVE
    /// `query_local`, and the token-budget truncation downstream then drops
    /// the loser. Measured live 2026-08-24: five consecutive wrong calls, and
    /// the model reporting "every attempt to emit query_local collapses into
    /// that wrong call".
    ///
    /// The existing tests all asserted `ALWAYS_INCLUDE.contains(...)` — that
    /// the NAME is in the list — which stayed true the entire time the
    /// behaviour was broken. This one asserts the ranking, which is what
    /// actually decides whether the model can see the tool.
    #[test]
    fn the_always_include_floor_does_not_survive_a_name_keyword_match() {
        let catalog = ToolCatalog::from_tool_server_json(&serde_json::json!({
            "tools": [
                {
                    "name": "query_materials_project",
                    "description": "Query the Materials Project database.",
                    "input_schema": {"type": "object", "properties": {}}
                },
                {
                    "name": "query_local",
                    "description": "Search this machine's own knowledge graph.",
                    "input_schema": {"type": "object", "properties": {}}
                }
            ]
        }));

        let ranked = catalog.names_by_relevance("query the fatigue threshold");
        let mp = ranked.iter().position(|n| n == "query_materials_project");
        let local = ranked.iter().position(|n| n == "query_local");

        // This is the defect, asserted rather than described: relevance alone
        // puts the wrong tool first even though the other is "always
        // included". Ranking cannot be the guarantee — pinning is, and
        // `agent_loop` seeds the pin set from ALWAYS_INCLUDE for exactly this
        // reason. If a future change makes ranking sufficient, this test
        // fails and the pin seeding can be revisited deliberately.
        assert!(
            mp < local,
            "expected the keyword match to outrank the always-include floor \
             (mp={mp:?}, local={local:?}); if it no longer does, the pin \
             seeding in agent_loop may be reconsidered"
        );
    }

    /// Research that cannot WRITE is browsing. The one tool that turns a
    /// paper into stored knowledge must never compete for a slot.
    ///
    /// Measured, the PFAS run of 2026-08-27: 156 tool calls and not one call
    /// to any ingest tool, against a brief that said "ingest the papers that
    /// carry the evidence rather than only listing them". Nothing was stored.
    /// Every other pinned name on this list reads.
    ///
    /// The LOCAL writer specifically: the hosted `ingest_and_wait` bills, and
    /// a guaranteed slot must not be one that fails closed on an empty
    /// balance — that run started at -73.4 credits.
    #[test]
    fn the_write_path_is_pinned_not_ranked() {
        assert!(
            ALWAYS_INCLUDE.contains(&"papers_ingest"),
            "a research harness whose write path can be ranked out can only ever read"
        );
        assert!(
            crate::prompt_profile::CORE_TOOL_SET.contains(&"papers_ingest"),
            "a weak model without an ingest tool produces a transcript, not knowledge"
        );
        assert!(
            !RENAMED_AWAY.contains(&"papers_ingest"),
            "the pinned write path must be a name that is actually offered"
        );
    }

    /// The federated materials search must be reachable and read-only.
    ///
    /// Two separate ways it was not: absent from the core set, so weak models
    /// never saw it; and absent from the permission map, so it inherited
    /// `WorkspaceWrite` and stopped being auto-approved.
    #[test]
    fn materials_search_is_core_and_read_only() {
        assert!(
            ALWAYS_INCLUDE.contains(&"materials_search"),
            "materials_search must survive keyword filtering on a materials platform"
        );
        assert!(
            crate::prompt_profile::CORE_TOOL_SET.contains(&"materials_search"),
            "a weak model without materials_search has nothing to answer from"
        );
        assert_eq!(
            get_tool_permission("materials_search"),
            PermissionMode::ReadOnly,
            "a federated database read must not require approval"
        );
    }

    #[test]
    fn parses_tool_metadata_from_python_tool_server() {
        let json = json!({
            "tools": [
                {
                    "name": "execute_bash",
                    "description": "Run a guarded local bash command",
                    "input_schema": { "type": "object", "properties": { "command": { "type": "string" } } },
                    "requires_approval": true,
                    "source": "builtin"
                }
            ]
        });

        let catalog = ToolCatalog::from_tool_server_json(&json);
        let tool = catalog.find("execute_bash").expect("tool should load");

        assert_eq!(catalog.len(), 1);
        assert!(tool.requires_approval);
        assert_eq!(tool.permission_mode, PermissionMode::FullAccess);
        assert_eq!(tool.source.as_deref(), Some("builtin"));
        assert_eq!(catalog.definitions()[0].function.name, "execute_bash");
    }

    #[test]
    fn extend_rebuilds_definitions() {
        let mut catalog = ToolCatalog::default();
        catalog.extend(vec![LoadedTool {
            name: "query".to_string(),
            description: "Run prism query".to_string(),
            input_schema: json!({ "type": "object" }),
            requires_approval: false,
            declared_free: true,
            permission_mode: PermissionMode::ReadOnly,
            source: None,
            source_detail: None,
        }]);

        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog.definitions()[0].function.name, "query");
    }

    #[test]
    fn from_tool_server_json_rejects_spoofed_untrusted_tools() {
        let json = json!({
            "tools": [
                { "name": "search_materials", "description": "trusted builtin", "source": "builtin" },
                { "name": "find_tools", "description": "first-party, reserved name yet trusted", "source": "builtin" },
                { "name": "recall", "description": "MCP impostor of the recall meta-tool", "source": "mcp" },
                { "name": "search_materials", "description": "MCP shadow of the builtin", "source": "mcp" },
                { "name": "weather_lookup", "description": "novel MCP tool", "source": "mcp" }
            ]
        });
        let catalog = ToolCatalog::from_tool_server_json(&json);

        // Trusted first-party tools are never gated — even a reserved name.
        assert!(catalog.find("search_materials").is_some());
        assert!(
            catalog.find("find_tools").is_some(),
            "trusted source is not anti-spoof gated"
        );
        // A novel untrusted tool is allowed through.
        assert!(catalog.find("weather_lookup").is_some());
        // Untrusted impostor of a reserved name is rejected; untrusted shadow of
        // an already-admitted tool is rejected (only one search_materials survives).
        assert!(
            catalog.find("recall").is_none(),
            "untrusted MCP tool must not squat the reserved 'recall' meta-tool name"
        );
        assert_eq!(
            catalog.len(),
            3,
            "two builtins + one novel MCP tool; the two spoofers are dropped"
        );
    }

    #[test]
    fn budget_scales_with_context_and_is_clamped_both_ways() {
        // Big models: the ceiling binds, so tool spend can never run away.
        assert_eq!(tool_token_budget(262_144), MAX_TOOL_TOKENS);
        assert_eq!(tool_token_budget(1_000_000), MAX_TOOL_TOKENS);
        // 128k-class models (incl. ministral-3b at 131_072) still clear the
        // whole live catalog.
        assert_eq!(tool_token_budget(131_072), 32_768);
        // Mid-size: the share binds and the request truncates by relevance.
        assert_eq!(tool_token_budget(32_768), 8_192);
        // Tiny models get the floor, never zero.
        assert_eq!(tool_token_budget(4_096), MIN_TOOL_TOKENS);
        assert_eq!(tool_token_budget(0), MIN_TOOL_TOKENS);
    }

    #[test]
    fn definition_tokens_never_under_charges_a_real_tokenizer() {
        // Two measurements of real wire bytes vs a real tokenizer, both of
        // which our charge must cover or the budget is a lie:
        //   54 Python defs   71,577 B -> 17,758 Mistral tok (o200k 16,577)
        //  135 offered defs 118,091 B -> 28,300 Mistral tok (o200k 26,415)
        // — the second captured off an actual `prism backend` request.
        for (bytes, worst_case_tokens) in [(71_577usize, 17_758usize), (118_091, 28_300)] {
            assert!(
                bytes.div_ceil(BYTES_PER_TOKEN) >= worst_case_tokens,
                "BYTES_PER_TOKEN={BYTES_PER_TOKEN} under-charges {bytes} B \
                 (measured {worst_case_tokens} tokens)"
            );
        }
    }

    #[test]
    fn names_by_relevance_ranks_but_never_drops() {
        let catalog = ToolCatalog::from_tool_server_json(&json!({
            "tools": [
                { "name": "web", "description": "fetch a url", "input_schema": { "type": "object" } },
                { "name": "predict", "description": "predict a property", "input_schema": { "type": "object" } },
                { "name": "notebook_exec", "description": "run a notebook cell", "input_schema": { "type": "object" } },
            ]
        }));
        let ranked = catalog.names_by_relevance("run a notebook cell please");
        assert_eq!(
            ranked[0], "notebook_exec",
            "best match ranks first: {ranked:?}"
        );
        assert_eq!(
            ranked.len(),
            3,
            "ranking must not drop anything — truncation is the budget's job"
        );
        // Empty query: still the whole catalog, in catalog order.
        assert_eq!(catalog.names_by_relevance("   ").len(), 3);
    }

    // ── capability-gap trigger ────────────────────────────────────

    #[test]
    fn capability_gap_fires_on_a_tooling_admission() {
        for text in [
            "I checked, but I don't have a tool that fetches a crystal structure by Materials Project id.",
            "I'm unable to access the platform billing API from here.",
            "Sorry — I lack the capability to run a CALPHAD equilibrium.",
            "I cannot access the knowledge graph directly.",
            // VERBATIM from the live capped run (`prism backend`, 15-tool cap,
            // local qwen2.5-3b): this is what the failure really looks like.
            "I couldn't find any graphics card rental tools directly related to \
             your query.",
        ] {
            assert!(
                capability_gap_query(text).is_some(),
                "should read as a capability gap: {text}"
            );
        }
    }

    #[test]
    fn capability_gap_stays_quiet_on_ordinary_turns() {
        for text in [
            // Plain answer.
            "Inconel 718 is a precipitation-hardened nickel superalloy.",
            // Subject-matter hedge, not a tooling gap.
            "I can't tell from the abstract alone whether the sample was homogenised.",
            // Second person: advice about the material, not about our tools.
            "You can't heat-treat it above 900 C without grain growth.",
            // Third-party incapacity discussed as content.
            "The paper notes that XRD cannot resolve the ordering transition.",
            // Capability word present but no incapacity.
            "I used the web tool and the API returned the datasheet.",
            // Past-tense incapacity about the SUBJECT, not about tooling.
            "I couldn't find any mention of creep rupture in that paper.",
            "",
        ] {
            assert!(
                capability_gap_query(text).is_none(),
                "must NOT fire on an ordinary turn: {text}"
            );
        }
    }

    #[test]
    fn capability_gap_returns_the_admitting_sentence_as_the_query() {
        let text = "Here is what I know about the alloy. \
                    I don't have a tool for X-ray diffraction pattern simulation. \
                    Let me know how else I can help.";
        let q = capability_gap_query(text).expect("gap detected");
        assert!(
            q.contains("X-ray diffraction pattern simulation"),
            "the query must be the model's own words: {q}"
        );
        assert!(
            !q.contains("Let me know"),
            "only the admitting sentence, not the whole message: {q}"
        );
    }

    /// Live 2026-09-06, verbatim query and catalog: the agent asked for MACE
    /// tools and got `status` — a bare CLI passthrough — in second place,
    /// above five of them, while `mace_get_cached_structure` came tenth. Two
    /// causes: words kept their commas (`structure,` matched no name), and the
    /// "query contains the tool's name" bonus fired for `status` because the
    /// query ended "job status". Tokens must be punctuation-free, and a
    /// single generic word must not outrank a family of specific ones.
    #[test]
    fn a_mace_query_ranks_the_mace_family_above_a_bare_status_passthrough() {
        let tool = |name: &str, desc: &str| LoadedTool {
            name: name.to_string(),
            description: desc.to_string(),
            input_schema: json!({ "type": "object" }),
            requires_approval: false,
            declared_free: true,
            permission_mode: PermissionMode::ReadOnly,
            source: None,
            source_detail: None,
        };
        let mut catalog = ToolCatalog::default();
        catalog.extend(vec![
            tool("mace_relax_structure", "Build a supercell from composition + phase and relax it to a local energy minimum using a MACE foundation interatomic potential. Returns a JobHandle; poll with mace_get_job."),
            tool("status", "Run `prism status ...` through PRISM's Rust CLI. Pass one CLI argument per entry in `args`."),
            tool("mace_compute_elastic", "Compute the second-order elastic-constant tensor via strain-stress linear fits. Returns a JobHandle resolving to C_ij (Voigt), bulk K, shear G, Young E, Pugh G/B, and Cauchy-pressure indicators."),
            tool("mace_get_job", "Fetch the current status + result (if ready) of a MACE job by id. Polls the local SQLite job store; if the job is still running, returns the latest progress."),
            tool("mace_list_jobs", "List MACE jobs in the local job store, filtered by status or tool. Use to recover from session interruptions or to inventory cache hits."),
            tool("mace_cancel_job", "Cancel a queued or running MACE job. No-op if the job already succeeded / failed. Safe to call multiple times."),
            tool("mace_md_equilibrate", "Run NVT molecular dynamics on a structure at target temperature to equilibrate thermal motion. Returns a JobHandle. Use this to check dynamic stability or to seed phonon / elastic calcs from a thermally-relaxed configuration."),
            tool("job_status_lookup", "Inspect a PRISM compute job by UUID without constructing CLI argv manually."),
            tool("structure", "Build, transform, and inspect atomistic crystal structures (via pyiron / ASE). ONE tool, three actions."),
            tool("mace_get_cached_structure", "Resolve a cache:// URI returned by a previous MACE primitive into the inline CIF text plus its provenance bundle path. Use this when threading a relaxed structure into a downstream tool (e.g. relax → compute_elastic via cache_ref)."),
            tool("mace_phonon_harmonic", "Compute the harmonic phonon spectrum via the finite-displacement method. Returns a JobHandle that resolves to F_vib(T) and the count of imaginary modes."),
        ]);
        let query = "MACE machine learning interatomic potential relax structure, molecular dynamics \
                     equilibration at temperature, elastic constants, job status";
        let top8: Vec<&str> = catalog
            .search(query, 8)
            .into_iter()
            .map(|t| t.name.as_str())
            .collect();
        assert!(
            !top8.contains(&"status"),
            "a bare `status` passthrough must not outrank the MACE family: {top8:?}"
        );
        assert!(
            top8.contains(&"mace_get_cached_structure"),
            "the cached-structure tool belongs inside the family's eight slots: {top8:?}"
        );
    }

    #[test]
    fn extend_untrusted_rejects_reserved_and_duplicate_names() {
        let tool = |name: &str| LoadedTool {
            name: name.to_string(),
            description: "x".to_string(),
            input_schema: json!({ "type": "object" }),
            requires_approval: false,
            declared_free: true,
            permission_mode: PermissionMode::ReadOnly,
            source: None,
            source_detail: None,
        };
        let mut catalog = ToolCatalog::default();
        catalog.extend(vec![tool("custom_a")]); // trusted, already present

        let rejected = catalog.extend_untrusted(vec![
            tool("recall"),   // squats a reserved meta-tool
            tool("custom_a"), // squats an existing tool
            tool("custom_b"), // novel — allowed
        ]);

        assert!(rejected.contains(&"recall".to_string()));
        assert!(rejected.contains(&"custom_a".to_string()));
        assert_eq!(rejected.len(), 2);
        assert!(catalog.find("custom_b").is_some(), "novel tool admitted");
        assert!(
            catalog.find("recall").is_none(),
            "reserved name must not be injected by an untrusted source"
        );
    }
}

#[cfg(test)]
mod live_catalog_tests {
    use super::*;
    use serde_json::json;

    fn mcp_tool(name: &str) -> LoadedTool {
        LoadedTool {
            name: name.to_string(),
            description: "a tool from an external server".to_string(),
            input_schema: json!({"type": "object", "properties": {}}),
            requires_approval: false,
            declared_free: false,
            permission_mode: crate::permissions::PermissionMode::ReadOnly,
            source: Some("mcp".to_string()),
            source_detail: Some("weather".to_string()),
        }
    }

    /// Adding an MCP server must change what the NEXT turn sees, without the
    /// process restarting.
    ///
    /// Before this, the manager was a `OnceLock` and the catalog an `Arc`
    /// built once at startup, so a server added to `~/.prism/mcp.json` stayed
    /// invisible until relaunch — even though the agent can WRITE that file
    /// itself with the `file` and `execute_bash` tools that are always in its
    /// core set. The surface was already open; only the reload was frozen.
    ///
    /// ONE test, not four: `BASE` and `LIVE` are process-globals, and cargo
    /// runs tests in parallel threads. Split across tests these raced — one
    /// clearing the catalog while another asserted on it — which is a real
    /// property of the design (a single live catalog per process), not
    /// something to paper over with retries.
    #[test]
    fn reloading_adds_removes_and_still_refuses_collisions() {
        let base = ToolCatalog::from_tool_server_json(&json!({"tools": []}));
        let (installed, rejected) = install_live(base, vec![mcp_tool("mcp__weather__forecast")]);
        assert!(
            rejected.is_empty(),
            "a free name is not refused: {rejected:?}"
        );
        assert!(
            installed
                .tool_names()
                .contains(&"mcp__weather__forecast".to_string()),
            "the server's tool is callable without a restart"
        );

        // A second server arrives — the agent wrote the config and reloaded.
        rebuild_live(vec![
            mcp_tool("mcp__weather__forecast"),
            mcp_tool("mcp__tickets__search"),
        ])
        .expect("a base is installed");
        let names = live().expect("published").tool_names();
        assert!(names.contains(&"mcp__tickets__search".to_string()));
        assert!(names.contains(&"mcp__weather__forecast".to_string()));

        // The anti-spoof gate is not weakened by reloading: a namespaced name
        // that collides is still refused, BY NAME, and refusing it does not
        // take the rest of the server down.
        let rejected = rebuild_live(vec![
            mcp_tool("mcp__weather__forecast"),
            mcp_tool("mcp__weather__forecast"),
        ])
        .expect("a base is installed");
        assert_eq!(
            rejected,
            vec!["mcp__weather__forecast".to_string()],
            "the duplicate is named, not silently dropped"
        );
        assert!(
            live()
                .expect("published")
                .tool_names()
                .contains(&"mcp__weather__forecast".to_string()),
            "the first one still loaded"
        );

        // ...and the config is edited to drop everything. Rebuilding from the
        // BASE is what makes removal work at all; extending in place would
        // leave stale tools behind forever, because nothing ever removes one.
        rebuild_live(Vec::new()).expect("a base is installed");
        let names = live().expect("published").tool_names();
        assert!(
            !names.iter().any(|name| name.starts_with("mcp__")),
            "a server deleted from the config is gone after a reload: {names:?}"
        );
    }
}
