use prism_ingest::llm::{FunctionDef, ToolDefinition};
use serde_json::{Value, json};

use crate::permissions::{PermissionMode, get_tool_permission};

/// How many tool definitions are offered to the model per LLM request
/// (top-K by relevance to the user message, plus the meta-tools and any
/// tools already called this session). The full catalog stays reachable
/// through the `find_tools` meta-tool. Advertised in `ui.welcome` as
/// `model_tool_selection.max_per_request` so clients and smokes can pin
/// the contract.
pub const MAX_TOOLS_PER_REQUEST: usize = 15;

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

    /// Return tool definitions filtered to the top-K most relevant for
    /// the user's query. Uses lightweight keyword matching on tool name
    /// and description — no embedding server required.
    ///
    /// This prevents "tool stuffing" (sending all 99 tools = 21K tokens
    /// to the LLM every turn). Falls back to all definitions if the
    /// query is empty or matches nothing.
    #[must_use]
    pub fn definitions_for_query(&self, query: &str, top_k: usize) -> Vec<ToolDefinition> {
        if query.trim().is_empty() || self.tools.len() <= top_k {
            return self.definitions.clone();
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

        // Sort by score descending, take top_k
        scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));

        let selected: Vec<&LoadedTool> = scored
            .into_iter()
            .filter(|(score, tool)| *score > 0 || ALWAYS_INCLUDE.contains(&tool.name.as_str()))
            .take(top_k)
            .map(|(_, tool)| tool)
            .collect();

        if selected.is_empty() {
            // No keyword matches — fall back to a sensible default set
            self.tools
                .iter()
                .filter(|t| ALWAYS_INCLUDE.contains(&t.name.as_str()))
                .take(top_k)
                .map(|t| t.to_definition())
                .collect()
        } else {
            selected.iter().map(|t| t.to_definition()).collect()
        }
    }
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
