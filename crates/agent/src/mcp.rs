// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! MCP (Model Context Protocol) CLIENT — connect external MCP servers and fold
//! their tools into the agent's [`ToolCatalog`].
//!
//! PRISM's own server list lives at `~/.prism/mcp.json` (the repo-root
//! `.mcp.json` is the *inverse* direction: outside agents driving PRISM).
//! Missing config = zero MCP servers, never an error. Schema:
//!
//! ```json
//! {
//!   "servers": [
//!     { "name": "everything", "command": "npx",
//!       "args": ["-y", "@modelcontextprotocol/server-everything"],
//!       "env": { "API_KEY": "..." } }
//!   ]
//! }
//! ```
//!
//! Only the `stdio` transport (spawn a server process) is supported today;
//! `transport` defaults to `"stdio"` and other values are skipped with a
//! warning. Each listed tool becomes a [`LoadedTool`] named
//! `mcp__<server>__<tool>` with `source: "mcp"` — an UNTRUSTED source, so
//! admission goes through [`ToolCatalog::extend_untrusted`] (anti-spoof) and
//! calls go through the standard approval gate (`requires_approval: true`,
//! never auto-approved).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::permissions::get_tool_permission;
use crate::tool_catalog::LoadedTool;

/// Prefix for every catalog name owned by the MCP client. Namespacing by
/// server is the primary collision defence; `extend_untrusted` is the second.
pub const MCP_TOOL_PREFIX: &str = "mcp__";

/// Per-server budget for spawn + initialize handshake and for list_tools —
/// a hung server must not wedge agent startup.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Budget for a single tool call round-trip.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

// ── Config ─────────────────────────────────────────────────────────

/// One entry in `~/.prism/mcp.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    /// Only `"stdio"` is supported today; HTTP/SSE is a documented follow-up.
    #[serde(default = "default_transport")]
    pub transport: String,
    /// Executable to spawn for stdio transport.
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment for the spawned server (inherits ours otherwise).
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Reserved for the future HTTP/SSE transport.
    #[serde(default)]
    pub url: Option<String>,
}

fn default_transport() -> String {
    "stdio".to_string()
}

#[derive(Debug, Default, Deserialize)]
struct McpConfigFile {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

/// PRISM's own MCP server list: `~/.prism/mcp.json`.
#[must_use]
pub fn default_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".prism/mcp.json")
}

/// Load the server list from `path`. Missing file = no servers (not an
/// error); a malformed file is an error so the caller can surface it.
pub fn load_config(path: &Path) -> Result<Vec<McpServerConfig>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let parsed: McpConfigFile = serde_json::from_str(&raw)
        .with_context(|| format!("malformed MCP config {}", path.display()))?;
    Ok(parsed.servers)
}

// ── Manager ────────────────────────────────────────────────────────

/// Live MCP client sessions plus the catalog-shaped view of their tools.
/// Built once at agent startup; sessions live for the process lifetime.
#[derive(Default)]
pub struct McpManager {
    sessions: HashMap<String, RunningService<RoleClient, ()>>,
    /// Namespaced catalog name → (server name, remote tool name). Dispatch is
    /// a lookup here — no name parsing, so server names may contain anything.
    routes: HashMap<String, (String, String)>,
    tools: Vec<LoadedTool>,
}

impl McpManager {
    /// Connect every configured server. Per-server failures are logged and
    /// skipped — one broken server must not take down the rest (or startup).
    pub async fn connect(configs: Vec<McpServerConfig>) -> Self {
        let mut manager = Self::default();
        for cfg in configs {
            if manager.sessions.contains_key(&cfg.name) {
                tracing::warn!(server = %cfg.name, "duplicate MCP server name — skipping");
                continue;
            }
            match connect_one(&cfg).await {
                Ok((service, remote_tools)) => {
                    let count = remote_tools.len();
                    for remote in remote_tools {
                        let loaded = to_loaded_tool(&cfg.name, &remote);
                        if manager.routes.contains_key(&loaded.name) {
                            tracing::warn!(
                                tool = %loaded.name,
                                server = %cfg.name,
                                "duplicate MCP tool name after namespacing — skipping",
                            );
                            continue;
                        }
                        manager.routes.insert(
                            loaded.name.clone(),
                            (cfg.name.clone(), remote.name.to_string()),
                        );
                        manager.tools.push(loaded);
                    }
                    manager.sessions.insert(cfg.name.clone(), service);
                    tracing::info!(server = %cfg.name, tools = count, "MCP server connected");
                }
                Err(err) => {
                    tracing::warn!(
                        server = %cfg.name,
                        error = %err,
                        "MCP server connection failed — skipping",
                    );
                }
            }
        }
        manager
    }

    /// Connect from `~/.prism/mcp.json`. A malformed config is loud but
    /// non-fatal: the agent still starts, with zero MCP servers.
    pub async fn connect_from_default_config() -> Self {
        let path = default_config_path();
        match load_config(&path) {
            Ok(configs) => Self::connect(configs).await,
            Err(err) => {
                tracing::error!(error = %err, "ignoring MCP config — fix {} and relaunch", path.display());
                Self::default()
            }
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    #[must_use]
    pub fn server_count(&self) -> usize {
        self.sessions.len()
    }

    /// Connected server names, sorted for stable display.
    #[must_use]
    pub fn server_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.sessions.keys().cloned().collect();
        names.sort();
        names
    }

    /// Catalog-shaped view of every connected server's tools, ready for
    /// [`crate::tool_catalog::ToolCatalog::extend_untrusted`].
    #[must_use]
    pub fn loaded_tools(&self) -> Vec<LoadedTool> {
        self.tools.clone()
    }

    /// Call a namespaced MCP tool on its owning server. Returns the same
    /// `{"result": …}` / `{"error": …}` shape the agent loop already parses.
    pub async fn call_tool(&self, namespaced: &str, args: &Value) -> Result<Value> {
        let (server, remote) = self
            .routes
            .get(namespaced)
            .ok_or_else(|| anyhow!("unknown MCP tool '{namespaced}'"))?;
        let session = self
            .sessions
            .get(server)
            .ok_or_else(|| anyhow!("MCP server '{server}' is not connected"))?;

        let mut request = rmcp::model::CallToolRequestParams::new(remote.clone());
        match args {
            Value::Object(map) if !map.is_empty() => {
                request = request.with_arguments(map.clone());
            }
            Value::Object(_) | Value::Null => {}
            other => bail!("MCP tool arguments must be a JSON object, got: {other}"),
        }

        let result = tokio::time::timeout(CALL_TIMEOUT, session.call_tool(request))
            .await
            .map_err(|_| anyhow!("MCP tool '{namespaced}' timed out"))?
            .with_context(|| format!("MCP tool '{namespaced}' failed"))?;

        // Prefer the structured result; otherwise join the text blocks.
        let payload = if let Some(structured) = result.structured_content {
            structured
        } else {
            let text = result
                .content
                .iter()
                .filter_map(|block| block.as_text().map(|t| t.text.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            Value::String(text)
        };
        if result.is_error == Some(true) {
            let message = match &payload {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            return Ok(json!({ "error": message }));
        }
        Ok(json!({ "result": payload }))
    }
}

/// Spawn + initialize one stdio server and list its tools.
async fn connect_one(
    cfg: &McpServerConfig,
) -> Result<(RunningService<RoleClient, ()>, Vec<rmcp::model::Tool>)> {
    if cfg.transport != "stdio" {
        bail!(
            "transport '{}' is not supported yet — only 'stdio' (spawned server process)",
            cfg.transport
        );
    }
    if cfg.command.is_empty() {
        bail!("stdio MCP server '{}' needs a 'command'", cfg.name);
    }
    let mut command = tokio::process::Command::new(&cfg.command);
    command.args(&cfg.args).envs(&cfg.env);
    let transport = TokioChildProcess::new(command)
        .with_context(|| format!("failed to spawn '{}'", cfg.command))?;

    let service = tokio::time::timeout(CONNECT_TIMEOUT, ().serve(transport))
        .await
        .map_err(|_| anyhow!("initialize handshake timed out"))?
        .context("MCP initialize handshake failed")?;
    let tools = tokio::time::timeout(CONNECT_TIMEOUT, service.list_all_tools())
        .await
        .map_err(|_| anyhow!("tools/list timed out"))?
        .context("tools/list failed")?;
    Ok((service, tools))
}

/// Convert one remote MCP tool into catalog metadata. The namespaced name
/// keeps servers from colliding with each other or with built-ins; `source:
/// "mcp"` marks it untrusted and `requires_approval: true` keeps it behind
/// the user approval gate (unknown names also resolve to WorkspaceWrite, so
/// the dynamic read-only auto-approve path can never pick these up).
fn to_loaded_tool(server: &str, tool: &rmcp::model::Tool) -> LoadedTool {
    let name = format!(
        "{MCP_TOOL_PREFIX}{}__{}",
        sanitize_segment(server),
        sanitize_segment(&tool.name)
    );
    let description = format!(
        "[MCP:{server}] {}",
        tool.description.as_deref().unwrap_or("(no description)")
    );
    LoadedTool {
        description,
        input_schema: Value::Object(tool.input_schema.as_ref().clone()),
        requires_approval: true,
        permission_mode: get_tool_permission(&name),
        source: Some("mcp".to_string()),
        source_detail: Some(server.to_string()),
        name,
    }
}

/// LLM function names must match `[a-zA-Z0-9_-]`; anything else becomes `_`.
fn sanitize_segment(segment: &str) -> String {
    segment
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ── Process-global registry ────────────────────────────────────────
//
// Mirrors the embeddings-backend / capability-index pattern: built once in
// `build_agent_seed`, read by the dispatch arm in `agent_loop` (including
// nested subagent turns) without threading another parameter through every
// `run_turn` call site.

static GLOBAL: OnceLock<Arc<McpManager>> = OnceLock::new();

/// Install the process-global manager. First caller wins (both transports
/// build from the same config file, so a second seed would be identical).
pub fn init_global(manager: McpManager) {
    let _ = GLOBAL.set(Arc::new(manager));
}

#[must_use]
pub fn global() -> Option<Arc<McpManager>> {
    GLOBAL.get().cloned()
}

/// Dispatch entry used by the agent loop for catalog tools with
/// `source == "mcp"`.
pub async fn call_global_tool(namespaced: &str, args: &Value) -> Result<Value> {
    match global() {
        Some(manager) => manager.call_tool(namespaced, args).await,
        None => bail!("no MCP servers are connected (missing ~/.prism/mcp.json?)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_means_zero_servers() {
        let path = Path::new("/nonexistent/prism-mcp-test/mcp.json");
        let servers = load_config(path).expect("missing file is not an error");
        assert!(servers.is_empty());
    }

    #[test]
    fn parses_config_schema() {
        let dir = std::env::temp_dir().join(format!("prism-mcp-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mcp.json");
        std::fs::write(
            &path,
            r#"{ "servers": [
                { "name": "everything", "command": "npx",
                  "args": ["-y", "@modelcontextprotocol/server-everything"],
                  "env": { "API_KEY": "k" } },
                { "name": "remote", "transport": "http", "url": "https://example.com/mcp" }
            ] }"#,
        )
        .unwrap();

        let servers = load_config(&path).expect("valid config parses");
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].name, "everything");
        assert_eq!(servers[0].transport, "stdio", "transport defaults to stdio");
        assert_eq!(servers[0].args.len(), 2);
        assert_eq!(servers[0].env.get("API_KEY").map(String::as_str), Some("k"));
        assert_eq!(servers[1].transport, "http");
        assert_eq!(servers[1].url.as_deref(), Some("https://example.com/mcp"));

        std::fs::remove_dir_all(&dir).ok();
        let malformed = dir.join("nope.json");
        assert!(load_config(&malformed).unwrap().is_empty());
    }

    #[test]
    fn namespaced_tool_carries_untrusted_metadata() {
        let remote = rmcp::model::Tool::new(
            "echo",
            "Echo back the input",
            serde_json::Map::from_iter([("type".to_string(), Value::String("object".to_string()))]),
        );
        let loaded = to_loaded_tool("stub server!", &remote);
        assert_eq!(loaded.name, "mcp__stub_server___echo");
        assert_eq!(loaded.source.as_deref(), Some("mcp"));
        assert_eq!(loaded.source_detail.as_deref(), Some("stub server!"));
        assert!(
            loaded.requires_approval,
            "MCP tools must stay behind the approval gate"
        );
        assert_eq!(
            loaded.permission_mode,
            crate::permissions::PermissionMode::WorkspaceWrite,
            "unknown namespaced name must not resolve to auto-approvable ReadOnly"
        );
        assert!(loaded.description.starts_with("[MCP:stub server!]"));
    }
}
