use prism_ingest::llm::{FunctionDef, ToolDefinition};
use serde_json::{Value, json};

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
/// The whole live catalog (54 Python + 77 command + 6 meta = 119,554 bytes)
/// charges 29,889 here, so today everything fits with ~10% headroom; past that
/// the escape hatch (`find_tools`) starts mattering again.
pub const MAX_TOOL_TOKENS: usize = 32_768;

/// Floor, so a small-context model still gets the meta-tools plus a couple of
/// real ones rather than meta-tools alone (the meta-tools alone charge 984).
pub const MIN_TOOL_TOKENS: usize = 2_048;

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

/// Full metadata for one loaded tool. Rust keeps this alongside the OpenAI
/// function definition so command views, permission logic, and approval UI all
/// talk about the same concrete tool facts.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub requires_approval: bool,
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
#[derive(Debug, Clone, Default)]
pub struct ToolCatalog {
    tools: Vec<LoadedTool>,
    definitions: Vec<ToolDefinition>,
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
            let requires_approval = tool
                .get("requires_approval")
                .and_then(Value::as_bool)
                .unwrap_or(false);
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
        let words: Vec<&str> = q.split_whitespace().filter(|w| w.len() > 2).collect();
        let mut scored: Vec<(usize, &LoadedTool)> = self
            .tools
            .iter()
            .map(|t| {
                let name = t.name.to_lowercase();
                let desc = t.description.to_lowercase();
                let mut score = 0usize;
                if q.contains(&name) {
                    score += 10;
                }
                for w in &words {
                    if name.contains(w) {
                        score += 5;
                    }
                    if desc.contains(w) {
                        score += 1;
                    }
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

        // Always-include tools that are core to every session
        const ALWAYS_INCLUDE: &[&str] = &["query", "search_materials", "query_platform"];

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

    #[test]
    fn extend_untrusted_rejects_reserved_and_duplicate_names() {
        let tool = |name: &str| LoadedTool {
            name: name.to_string(),
            description: "x".to_string(),
            input_schema: json!({ "type": "object" }),
            requires_approval: false,
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
