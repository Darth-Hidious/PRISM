//! Chat-surface adapter — launches the in-process chat UI as PRISM's
//! interactive surface. Replaces the broken Ratatui TUI for chat-mode
//! entry, and is the integration point between PRISM's CLI dispatch
//! and the Apache-2.0 vendored forge_* crates from tailcallhq/forgecode.
//!
//! Companion modules: `chat_config` (target persistence) and
//! `use_command` (the apply() shared by the CLI + the in-chat
//! `/use` slash command).

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use forge_api::ForgeAPI;
use forge_config::{ForgeConfig, ModelConfig, Update, UpdateFrequency};
use forge_domain::{McpConfig, McpServerConfig, ServerName};
use forge_main::{Cli as ForgeCli, UI};
use prism_runtime::StoredCredentials;
use prism_tool_router::{Config as RouterConfig, ToolRouter};

use crate::boot;
use crate::platform_bridge;

const DEFAULT_PROVIDER_ID: &str = "openai_compatible";

/// Default chat model. Switched from `gemini-3.1-flash-lite-preview`
/// to `gpt-5.5` because:
///   1. Gemini's OpenAI-compat shim has a documented year-old bug with
///      streaming tool_calls (the `index` field is missing from delta
///      chunks, breaking every parser that follows the OpenAI spec).
///      Refs:
///        - https://discuss.ai.google.dev/t/gemini-openai-compatibility-issue-with-tool-call-streaming/59886
///        - https://github.com/openai/openai-python/issues/2806
///   2. `gpt-4o*` is being deprecated and the user explicitly asked not
///      to use it as a default.
///   3. `gpt-5.5` is OpenAI's reference implementation — clean
///      streaming + clean tool_calls — and supports `reasoning_effort`
///      (none / low / medium / high / xhigh) so we can pick fast paths
///      for chat and deeper reasoning paths for discourse.
///   4. MARC27 fronts gpt-5.5 at $2/M input, $8/M output — reasonable
///      for materials-research workloads.
///
/// Users override per-session via the upcoming `prism use marc27
/// --model <name>` (and the in-chat `/use` slash command).
const DEFAULT_MODEL_ID: &str = "gpt-5.5";

const PRISM_BANNER: &str = "\x1b[38;2;0;255;255m\
██████╗ ██████╗ ██╗███████╗███╗   ███╗
██╔══██╗██╔══██╗██║██╔════╝████╗ ████║
██████╔╝██████╔╝██║███████╗██╔████╔██║
██╔═══╝ ██╔══██╗██║╚════██║██║╚██╔╝██║
██║     ██║  ██║██║███████║██║ ╚═╝ ██║
╚═╝     ╚═╝  ╚═╝╚═╝╚══════╝╚═╝     ╚═╝\x1b[0m\n\
\x1b[38;2;120;120;120m              · built on Forge ·\x1b[0m";

pub async fn run(
    project_root: &Path,
    credentials: Option<&StoredCredentials>,
    python_path: &Path,
) -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Register PRISM's MCP servers in forge user-scope config so all PRISM
    // tools are visible to the LLM. Two servers:
    //   prism-rust   → native Rust MCP server (this binary, mcp-server-native
    //                  subcommand) for query / ingest / mesh / workflow /
    //                  knowledge-graph and other Rust-side tools.
    //   prism-python → FastMCP server for the materials-science Python tools
    //                  (calphad, pyiron, ML predictions, OPTIMADE, …) that
    //                  cannot be expressed natively in Rust.
    // Idempotent: preserves any other servers the user has configured.
    if let Err(e) = register_prism_mcp_servers(project_root, python_path) {
        eprintln!("\x1b[33m[prism]\x1b[0m MCP registration skipped: {e:#}");
    }

    // Brand the chat surface as PRISM. FORGE_BANNER overrides the default
    // ASCII; FORGE_HIDE_ZSH_TIP suppresses the "forge zsh setup" tip that
    // doesn't apply inside prism.
    unsafe {
        std::env::set_var("FORGE_BANNER", PRISM_BANNER);
        std::env::set_var("FORGE_HIDE_ZSH_TIP", "1");
    }

    // Surface chat-UI panics to stderr instead of dying silently after
    // PRISM's splash teardown clears the alternate screen. The internal
    // crate is the vendored forge harness, but the user-visible label
    // says PRISM — they don't need to know the harness's name.
    std::panic::set_hook(Box::new(|info| {
        eprintln!("\x1b[31m[prism] chat UI panic:\x1b[0m {info}");
    }));

    // Bring up the semantic tool retriever (EmbeddingGemma only).
    // FunctionGemma's local-routing path was removed (it caused silent
    // failures when it picked a tool the chat LLM never got to summarise);
    // the chat LLM now does selection + arg extraction on the top-K tools.
    // On any failure the proxy falls back to FIFO body-budget trimming so
    // the user can still chat — semantic retrieval just gets disabled.
    boot::section("Local intelligence");
    let router: Option<Arc<ToolRouter>> = match start_tool_router().await {
        Ok(r) => Some(r),
        Err(e) => {
            boot::warn(&format!(
                "semantic tool router unavailable ({}), falling back to chat-LLM routing",
                short_error(&e)
            ));
            None
        }
    };

    // Spawn the in-process MARC27 ↔ OpenAI proxy if we have credentials.
    // The proxy lives for the lifetime of this fn (its handle is held until
    // forge UI exits), exposing an OpenAI surface on a free localhost port
    // that translates to MARC27's custom JSON+SSE protocol upstream.
    // Result of starting the MARC27 ↔ OpenAI proxy. Drives the boot
    // status line below + decides whether to clear stale forge creds.
    enum ProxyState {
        Ok(platform_bridge::ProxyHandle),
        NoCreds,
        NoProject,
        TokenRejected,
        StartFailed(String),
    }

    let proxy_state = if let Some(creds) = credentials {
        let project_id = creds.project_id.as_deref().unwrap_or_default();
        if project_id.is_empty() {
            ProxyState::NoProject
        } else if !token_works(&creds.platform_url, &creds.access_token).await {
            // Token validation BEFORE starting the proxy. Without this,
            // a rejected/expired token still passes platform_bridge::start
            // (which only binds a port) and the boot screen lies "Chat
            // OK" while every chat request 401s and forge infinite-retries.
            // This is the actual fix for Bug #23.
            ProxyState::TokenRejected
        } else {
            let initial_chat_target = crate::chat_config::load().unwrap_or_default().chat;
            match platform_bridge::start(
                &creds.platform_url,
                project_id,
                &creds.access_token,
                router.clone(),
                initial_chat_target,
            )
            .await
            {
                Ok(handle) => {
                    let proxy_url = handle.url.clone();
                    unsafe {
                        std::env::set_var("OPENAI_URL", &proxy_url);
                        std::env::set_var("OPENAI_API_KEY", &creds.access_token);
                    }
                    if let Err(e) =
                        upsert_openai_compatible_credential(&proxy_url, &creds.access_token)
                    {
                        eprintln!("\x1b[33m[prism]\x1b[0m credential upsert failed: {e:#}");
                    }
                    ProxyState::Ok(handle)
                }
                Err(e) => ProxyState::StartFailed(short_error(&e)),
            }
        }
    } else {
        ProxyState::NoCreds
    };

    // When the proxy doesn't start, scrub any stale openai_compatible
    // entry from `~/.forge/.credentials.json` AND unset OPENAI_URL/
    // OPENAI_API_KEY env vars. Without this, forge picks up a dead
    // localhost port from a previous successful PRISM run and
    // infinite-retries with "Connection refused" — Bug #23.
    let proxy_ok = matches!(proxy_state, ProxyState::Ok(_));
    if !proxy_ok {
        if let Err(e) = clear_openai_compatible_credential() {
            eprintln!("\x1b[33m[prism]\x1b[0m couldn't clear stale forge credentials: {e:#}");
        }
        unsafe {
            std::env::remove_var("OPENAI_URL");
            std::env::remove_var("OPENAI_API_KEY");
        }
    }

    // Truthful boot status lines. Two lines, parallel structure —
    // see Bug #23 in docs/SHIPPED.md for why these moved out of
    // start_tool_router(). The Chat line tells the user where their
    // first message will (or won't) actually go BEFORE they type it.
    let chat_target = crate::chat_config::load().unwrap_or_default().chat;
    match &proxy_state {
        ProxyState::Ok(_) => {
            boot::status_line("Chat", true, &chat_target.human_full());
            boot::status_line("Tools", true, "MARC27 cloud");
        }
        ProxyState::NoCreds => {
            boot::status_line("Chat", false, "not logged in — run `prism login`");
            boot::status_line("Tools", false, "platform unavailable until login");
        }
        ProxyState::NoProject => {
            boot::status_line(
                "Chat",
                false,
                "no project selected — run `prism login` and pick a project",
            );
            boot::status_line("Tools", false, "platform unavailable until project picked");
        }
        ProxyState::TokenRejected => {
            boot::status_line("Chat", false, "platform token rejected — run `prism login`");
            boot::status_line("Tools", false, "platform unavailable until login");
        }
        ProxyState::StartFailed(reason) => {
            boot::status_line("Chat", false, &format!("proxy start failed — {reason}"));
            boot::status_line("Tools", false, "platform unavailable");
        }
    }
    let _proxy = match proxy_state {
        ProxyState::Ok(h) => Some(h),
        _ => None,
    };

    let mut cli = ForgeCli::parse_from(["prism-chat"]);

    // If stdin is piped (non-TTY), forward the contents as a one-shot prompt
    // so callers can drive the chat non-interactively, e.g. for scripted
    // smoke tests:  echo "show prism status" | prism tui
    if !std::io::stdin().is_terminal() {
        let mut buf = String::new();
        if std::io::stdin().read_to_string(&mut buf).is_ok() {
            let trimmed = buf.trim();
            if !trimmed.is_empty() {
                cli.piped_input = Some(trimmed.to_string());
            }
        }
    }

    let mut config =
        ForgeConfig::read().context("Failed to read Forge configuration from .forge.toml")?;
    if config.session.is_none() {
        config.session = Some(ModelConfig {
            provider_id: DEFAULT_PROVIDER_ID.to_string(),
            model_id: DEFAULT_MODEL_ID.to_string(),
        });
    }

    // Disable forge's auto-update self-installer.
    //
    // The vendored forge_main checks tailcallhq/forgecode's GitHub releases on
    // every interactive launch (UpdateFrequency::Always default) and, if a
    // newer version exists, runs `curl -fsSL https://forgecode.dev/cli | sh`
    // followed by std::process::exit(0). That makes sense for the standalone
    // forge CLI but is wrong here: forge IS prism's chat surface. Letting it
    // "update" would install a separate ~/.local/bin/forge binary and kill
    // prism before chat starts. We override unconditionally — this isn't a
    // setting the user should be able to turn back on.
    config.updates = Some(
        Update::default()
            .frequency(UpdateFrequency::Never)
            .auto_update(false),
    );

    let cwd = project_root.to_path_buf();
    let mut ui = UI::init(cli, config, move |config| {
        ForgeAPI::init(cwd.clone(), config)
    })?;
    ui.run().await;
    Ok(())
}

/// Spin up the semantic tool router. Looks up `~/.prism/models/` for the
/// EmbeddingGemma GGUF and spawns a llama-server subprocess against it.
/// Returns Err if the model file is missing or the subprocess won't come
/// up; callers should fall back to non-semantic behaviour rather than
/// abort the whole chat session.
async fn start_tool_router() -> Result<Arc<ToolRouter>> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME not set")?;
    let config = RouterConfig::default_for_home(&home);

    // First-launch UX: stream the embedder GGUF down from HF Hub if missing.
    // EmbeddingGemma is the only local model the bridge still uses — it
    // narrows 125 tools → top-K relevant ones before forwarding to the chat
    // LLM. FunctionGemma is no longer loaded; selection + arg extraction
    // happen at the chat-LLM layer where the model is best-in-class at it.
    prism_tool_router::ensure_model(&config.embedder_remote, &config.embedder_gguf)
        .await
        .context("download EmbeddingGemma")?;
    boot::status_line("Embedder model", true, &short_path(&config.embedder_gguf));

    let router = Arc::new(ToolRouter::new(config.clone()).await?);
    router.start().await?;
    boot::status_line("Semantic retrieval", true, "online");

    // Note: the Chat + Tools status lines are NOT printed here. They
    // depend on whether the MARC27 proxy actually started successfully,
    // which happens later in `run()` after credentials are checked.
    // Printing "Chat OK" here regardless of proxy state was Bug #23 —
    // the boot screen lied to the user when auth was rejected, and the
    // TUI then infinite-retried a dead localhost port from a stale
    // forge credentials entry. Caller (run) prints those lines.

    Ok(router)
}

/// Produce a short, human-readable rendering of an error chain — drops the
/// stack/debug noise so users see the root cause in one line.
fn short_error(e: &anyhow::Error) -> String {
    let mut s = e.to_string();
    if let Some(src) = e.source() {
        s.push_str(": ");
        s.push_str(&src.to_string());
    }
    // Strip any trailing JSON dumps so the line stays scannable.
    if let Some(idx) = s.find('{') {
        s.truncate(idx);
    }
    s.trim().to_string()
}

fn short_path(p: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::Path::new(&home);
        if let Ok(rel) = p.strip_prefix(home) {
            return format!("~/{}", rel.display());
        }
    }
    p.display().to_string()
}

/// Ensure the `openai_compatible` provider credential in
/// `~/.forge/.credentials.json` points at our local MARC27 proxy with
/// the current process's port. Idempotent: preserves any user-added
/// entries, only updates our own.
///
/// Also sanitizes empty-keyed stub entries that forge ships with by
/// default. Those stubs (e.g. `{ id: "anthropic", auth_details: { api_key: "" } }`)
/// crash `/model` because the picker iterates all entries and hits
/// the empty Anthropic with a 401. Yesterday I conflated this stub
/// crash with "our entry pollutes the file" — they're separate bugs.
/// The fix is: write OUR entry (with the live port), and remove
/// stub entries with empty keys. Real user-added Anthropic entries
/// (with non-empty keys) stay untouched.
/// Quick token-validity check against the platform.
///
/// Hits `GET {platform_url}/api/v1/users/me` with a short timeout.
/// Returns true iff status is 2xx — anything else (401, 5xx, network
/// error, timeout) means we shouldn't trust the token. Used at boot
/// to decide whether to start the chat proxy or surface
/// "platform token rejected — run `prism login`" instead of letting
/// forge infinite-retry against a localhost port whose upstream will
/// 401 every request (Bug #23).
async fn token_works(platform_url: &str, access_token: &str) -> bool {
    if access_token.trim().is_empty() {
        return false;
    }
    let url = format!("{}/api/v1/users/me", platform_url.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.get(&url).bearer_auth(access_token).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

/// Remove the `openai_compatible` entry from `~/.forge/.credentials.json`.
///
/// Used when the MARC27 proxy fails to start (no credentials, expired
/// token, etc.). Without this, a previous successful run leaves an
/// entry pointing at a now-dead localhost port; forge keeps trying it
/// and infinite-retries with `Connection refused` errors flooding the
/// chat panel — Bug #23.
///
/// Idempotent: missing file or missing entry are not errors.
fn clear_openai_compatible_credential() -> Result<()> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    let path = std::path::PathBuf::from(home).join(".forge/.credentials.json");
    if !path.exists() {
        return Ok(());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut entries: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
    let before = entries.len();
    entries.retain(|e| e.get("id").and_then(|v| v.as_str()) != Some("openai_compatible"));
    if entries.len() == before {
        return Ok(()); // nothing to do
    }
    let out = serde_json::to_string_pretty(&entries).context("serialising credentials")?;
    std::fs::write(&path, out).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn upsert_openai_compatible_credential(proxy_url: &str, access_token: &str) -> Result<()> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    let forge_dir = std::path::PathBuf::from(home).join(".forge");
    std::fs::create_dir_all(&forge_dir)
        .with_context(|| format!("creating {}", forge_dir.display()))?;
    let path = forge_dir.join(".credentials.json");

    let mut entries: Vec<serde_json::Value> = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).unwrap_or_default()
    } else {
        Vec::new()
    };

    let entry = serde_json::json!({
        "id": "openai_compatible",
        "auth_details": { "api_key": access_token },
        "url_params": { "OPENAI_URL": proxy_url.trim_end_matches('/') },
    });

    let mut replaced = false;
    for e in entries.iter_mut() {
        if e.get("id").and_then(|v| v.as_str()) == Some("openai_compatible") {
            *e = entry.clone();
            replaced = true;
            break;
        }
    }
    if !replaced {
        entries.push(entry);
    }

    // Strip empty-keyed stub entries. forge_config ships a default
    // .credentials.json with empty Anthropic / OpenAI / etc. stubs
    // so the user can `forge auth login anthropic` later. Those
    // stubs are harmless to forge directly (it skips empty keys
    // when trying to use the provider) but `/model` iterates every
    // entry and queries each provider's /v1/models endpoint — which
    // 401s on the empty-keyed Anthropic and dumps a stack trace at
    // the user. Filter them out: any entry whose api_key is empty,
    // EXCEPT our just-written openai_compatible entry (which has a
    // real PRISM-issued JWT in api_key — so it'd never match the
    // empty filter, this is belt-and-suspenders).
    entries.retain(|e| {
        if e.get("id").and_then(|v| v.as_str()) == Some("openai_compatible") {
            return true;
        }
        e.get("auth_details")
            .and_then(|a| a.get("api_key"))
            .and_then(|k| k.as_str())
            .map(|k| !k.is_empty())
            .unwrap_or(false)
    });

    let text = serde_json::to_string_pretty(&entries).context("serialising credentials")?;
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Write `~/.forge/.mcp.json` (user scope) registering both the Rust-native
/// MCP server (`prism-rust`) and the Python MCP server (`prism-python`).
/// Preserves any existing servers — only inserts/updates our two keys.
fn register_prism_mcp_servers(project_root: &Path, python_path: &Path) -> Result<()> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    let forge_dir = std::path::PathBuf::from(home).join(".forge");
    std::fs::create_dir_all(&forge_dir)
        .with_context(|| format!("creating {}", forge_dir.display()))?;
    let mcp_path = forge_dir.join(".mcp.json");

    let mut config: McpConfig = if mcp_path.exists() {
        let text = std::fs::read_to_string(&mcp_path)
            .with_context(|| format!("reading {}", mcp_path.display()))?;
        serde_json::from_str(&text).unwrap_or_default()
    } else {
        McpConfig::default()
    };

    // Drop any legacy single "prism" entry from earlier builds.
    let legacy = ServerName::from("prism".to_string());
    config.mcp_servers.remove(&legacy);

    // prism-rust: this binary, mcp-server-native subcommand. No Python in
    // the loop for Rust-side tools.
    let prism_exe = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "prism".to_string());
    let rust_server =
        McpServerConfig::new_stdio(prism_exe, vec!["mcp-server-native".to_string()], None);

    // prism-python: FastMCP server for the materials-science Python tools.
    // Re-enabled in Stage 2 because the EmbeddingGemma-backed semantic
    // router prunes forge's per-turn tool list to top-K=8, keeping us
    // well under MARC27's 64 KiB body limit even with all 108+ Python
    // tools registered.
    let mut py_env = BTreeMap::new();
    py_env.insert(
        "PYTHONPATH".to_string(),
        project_root.to_string_lossy().into_owned(),
    );
    let python_server = McpServerConfig::new_stdio(
        python_path.to_string_lossy().into_owned(),
        vec!["-m".to_string(), "app.mcp_server".to_string()],
        Some(py_env),
    );

    let mut needs_write = false;
    for (name, server) in [("prism-rust", rust_server), ("prism-python", python_server)] {
        let key = ServerName::from(name.to_string());
        let dirty = config
            .mcp_servers
            .get(&key)
            .map(|existing| existing != &server)
            .unwrap_or(true);
        if dirty {
            config.mcp_servers.insert(key, server);
            needs_write = true;
        }
    }

    if needs_write {
        let text = serde_json::to_string_pretty(&config).context("serialising MCP config")?;
        std::fs::write(&mcp_path, text)
            .with_context(|| format!("writing {}", mcp_path.display()))?;
    }
    Ok(())
}
