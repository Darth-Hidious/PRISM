//! JSON-RPC 2.0 stdio server for the Ink TUI frontend.
//!
//! Reads JSON-RPC requests from stdin, dispatches them, and emits
//! `ui.*` notifications on stdout. Stdout is the protocol channel
//! so all logging MUST go through `tracing`, never `println!`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use prism_client::api::{OrgInfo, PlatformClient, ProjectInfo};
use prism_client::{auth::DeviceCodeResponse, auth::TokenResponse, DeviceFlowAuth};
use prism_ingest::llm::{ChatMessage, LlmClient};
use prism_ingest::LlmConfig;
use prism_python_bridge::tool_server::{ToolServer, ToolServerHandle};
use prism_runtime::{PlatformEndpoints, PrismPaths, StoredCredentials};
use prism_workflows::{
    discover_workflows, execute_workflow_with_policy, find_workflow, parse_workflow_command_args,
    WorkflowRunResult, WorkflowSpec,
};
use serde_json::{json, Value};
use tokio::process::Command as TokioCommand;
use tokio::sync::oneshot;
use tokio::time::timeout;

use crate::agent_loop;
use crate::command_tools::{self, CommandToolRuntime};
use crate::commands::{builtin_help_text, is_cli_backed_slash_root};
use crate::hooks::{build_default_hooks, HookRegistry};
use crate::permissions::{
    PermissionMode, PermissionOverrides, SharedPermissionOverrides, ToolPermissionContext,
};
use crate::prompts::{append_runtime_tool_guidance, build_system_prompt};
use crate::scratchpad::Scratchpad;
use crate::session::{RuntimeSessionState, SessionStore};
use crate::tool_catalog::ToolCatalog;
use crate::transcript::{
    extract_key_files, extract_pending_work, TranscriptEntry, TranscriptStore,
};
use crate::types::{AgentConfig, AgentEvent};

// ── Emit helpers ──────────────────────────────────────────────────

fn emit_raw(value: &Value) {
    let line = serde_json::to_string(value).expect("JSON serialization failed");
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn emit_notification(method: &str, params: Value) {
    emit_raw(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    }));
}

fn emit_response(id: Value, result: Value) {
    emit_raw(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }));
}

fn emit_error(code: i64, message: &str, id: Value) {
    emit_raw(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    }));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionMode {
    Chat,
    Plan,
}

impl SessionMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Plan => "plan",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanStatus {
    None,
    Draft,
    Approved,
    Rejected,
}

impl PlanStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Draft => "draft",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "draft" => Self::Draft,
            "approved" => Self::Approved,
            "rejected" => Self::Rejected,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct PlanRuntimeState {
    status: Option<PlanStatus>,
    approved_plan_body: Option<String>,
}

enum DeferredRuntimeUpdate {
    AllowTool(String),
    DenyTool(String),
}

#[derive(Debug, Clone)]
struct SlashCommandContext {
    current_exe: PathBuf,
    project_root: PathBuf,
    python_bin: PathBuf,
}

struct ServerRuntime {
    tool_server: ToolServerHandle,
    command_tool_runtime: CommandToolRuntime,
    llm_config: LlmConfig,
    history: Vec<ChatMessage>,
    transcript: TranscriptStore,
    session_mode: SessionMode,
    plan_state: PlanRuntimeState,
    permission_overrides: PermissionOverrides,
    permissions: ToolPermissionContext,
    scratchpad: Scratchpad,
    session_store: SessionStore,
    policy_engine: Option<prism_policy::PolicyEngine>,
}

#[derive(Debug, Clone, Default)]
struct SelectedContext {
    org_id: Option<String>,
    org_name: Option<String>,
    project_id: Option<String>,
    project_name: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct SelectionOutcome {
    context: SelectedContext,
    notes: Vec<String>,
}

fn env_project_override() -> Option<String> {
    std::env::var("MARC27_PROJECT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn default_project_name(display_name: Option<&str>) -> String {
    match display_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(name) => format!("{name} PRISM Workspace"),
        None => "PRISM Workspace".to_string(),
    }
}

fn default_project_slug() -> String {
    let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    format!("prism-{timestamp}")
}

fn pick_organization(
    orgs: &[OrgInfo],
    prior: Option<&StoredCredentials>,
) -> Option<(OrgInfo, String)> {
    if orgs.is_empty() {
        return None;
    }

    if let Some(prior_org_id) = prior.and_then(|creds| creds.org_id.as_deref()) {
        if let Some(org) = orgs.iter().find(|org| org.id == prior_org_id) {
            return Some((org.clone(), format!("Reused organization {}", org.name)));
        }
    }

    if orgs.len() == 1 {
        let org = orgs[0].clone();
        return Some((
            org.clone(),
            format!("Using only available organization {}", org.name),
        ));
    }

    let mut sorted = orgs.to_vec();
    sorted.sort_by_key(|org| org.name.to_ascii_lowercase());
    let org = sorted[0].clone();
    Some((
        org.clone(),
        format!(
            "Selected default organization {} from {} available options",
            org.name,
            orgs.len()
        ),
    ))
}

fn pick_project(
    projects: &[ProjectInfo],
    prior: Option<&StoredCredentials>,
) -> Option<(ProjectInfo, String)> {
    if projects.is_empty() {
        return None;
    }

    if let Some(prior_project_id) = prior.and_then(|creds| creds.project_id.as_deref()) {
        if let Some(project) = projects
            .iter()
            .find(|project| project.id == prior_project_id)
        {
            return Some((project.clone(), format!("Reused project {}", project.name)));
        }
    }

    if let Some(project) = projects
        .iter()
        .find(|project| project.name.eq_ignore_ascii_case("Sandbox"))
    {
        return Some((
            project.clone(),
            format!("Selected default project {}", project.name),
        ));
    }

    if projects.len() == 1 {
        let project = projects[0].clone();
        return Some((
            project.clone(),
            format!("Using only available project {}", project.name),
        ));
    }

    let mut sorted = projects.to_vec();
    sorted.sort_by_key(|project| project.name.to_ascii_lowercase());
    let project = sorted[0].clone();
    Some((
        project.clone(),
        format!(
            "Selected default project {} from {} available options",
            project.name,
            projects.len()
        ),
    ))
}

async fn select_project_context_automatically(
    platform: &PlatformClient,
    display_name: Option<&str>,
    prior: Option<&StoredCredentials>,
) -> Result<SelectionOutcome> {
    if let Some(project_id) = env_project_override() {
        match platform.get_project(&project_id).await {
            Ok(project) => {
                let org_name = platform.list_orgs().await.ok().and_then(|orgs| {
                    orgs.into_iter()
                        .find(|org| org.id == project.org_id)
                        .map(|org| org.name)
                });
                return Ok(SelectionOutcome {
                    context: SelectedContext {
                        org_id: Some(project.org_id.clone()),
                        org_name,
                        project_id: Some(project.id),
                        project_name: Some(project.name),
                    },
                    notes: vec![format!("Using MARC27_PROJECT_ID override ({project_id})")],
                });
            }
            Err(error) => {
                tracing::warn!(error = %error, project_id, "failed to resolve MARC27_PROJECT_ID override");
            }
        }
    }

    let orgs = platform.list_orgs().await?;
    if orgs.is_empty() {
        return Ok(SelectionOutcome {
            context: SelectedContext::default(),
            notes: vec!["No organizations available for this account yet.".to_string()],
        });
    }

    let (org, org_note) = pick_organization(&orgs, prior)
        .ok_or_else(|| anyhow::anyhow!("failed to select an organization"))?;
    let mut notes = vec![org_note];
    let projects = platform.list_projects_for_org(&org.id).await?;

    if let Some((project, project_note)) = pick_project(&projects, prior) {
        notes.push(project_note);
        return Ok(SelectionOutcome {
            context: SelectedContext {
                org_id: Some(org.id),
                org_name: Some(org.name),
                project_id: Some(project.id),
                project_name: Some(project.name),
            },
            notes,
        });
    }

    let created = platform
        .create_project(
            &org.id,
            &default_project_name(display_name),
            &default_project_slug(),
        )
        .await
        .with_context(|| {
            format!(
                "failed to auto-create a PRISM project in organization {}",
                org.name
            )
        })?;

    notes.push(format!(
        "Created project {} because none were available",
        created.name
    ));
    Ok(SelectionOutcome {
        context: SelectedContext {
            org_id: Some(org.id),
            org_name: Some(org.name),
            project_id: Some(created.id),
            project_name: Some(created.name),
        },
        notes,
    })
}

async fn start_native_device_login(endpoints: &PlatformEndpoints) -> Result<DeviceCodeResponse> {
    let platform = PlatformClient::new(&endpoints.api_base);
    let http = platform.inner().clone();

    let start: DeviceCodeResponse =
        DeviceFlowAuth::start_device_flow(&http, &endpoints.api_base).await?;
    if let Err(error) = open_browser(&start.verification_uri) {
        tracing::warn!(error = %error, "failed to open browser automatically during login");
    }
    Ok(start)
}

async fn poll_native_device_login(
    endpoints: &PlatformEndpoints,
    start: &DeviceCodeResponse,
) -> Result<StoredCredentials> {
    let platform = PlatformClient::new(&endpoints.api_base);
    let http = platform.inner().clone();
    let token: TokenResponse = DeviceFlowAuth::poll_for_token(
        &http,
        &endpoints.api_base,
        &start.device_code,
        start.interval.max(1) as u64,
    )
    .await?;

    let expires_at = token.expires_in.and_then(|secs| {
        chrono::Utc::now().checked_add_signed(chrono::Duration::seconds(secs as i64))
    });

    Ok(StoredCredentials {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        platform_url: endpoints.api_base.trim_end_matches("/api/v1").to_string(),
        user_id: None,
        display_name: None,
        org_id: None,
        org_name: None,
        project_id: None,
        project_name: None,
        expires_at,
    })
}

fn sync_sdk_credentials(creds: &StoredCredentials) {
    let sdk_creds = serde_json::json!({
        "access_token": creds.access_token,
        "refresh_token": creds.refresh_token,
        "platform_url": creds.platform_url,
        "user_id": creds.user_id,
        "org_id": creds.org_id,
        "project_id": creds.project_id,
    });
    if let Some(home) = std::env::var_os("HOME") {
        let sdk_path = PathBuf::from(home).join(".prism").join("credentials.json");
        if let Some(parent) = sdk_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&sdk_creds) {
            let _ = fs::write(sdk_path, json);
        }
    }
}

fn clear_sdk_credentials() {
    if let Some(home) = std::env::var_os("HOME") {
        let sdk_path = PathBuf::from(home).join(".prism").join("credentials.json");
        let _ = fs::remove_file(sdk_path);
    }
}

fn apply_account_env(creds: Option<&StoredCredentials>) {
    const KEYS: &[&str] = &[
        "MARC27_TOKEN",
        "MARC27_PLATFORM_URL",
        "MARC27_PROJECT_ID",
        "PRISM_ACCOUNT_USER_ID",
        "PRISM_ACCOUNT_DISPLAY_NAME",
        "PRISM_ACCOUNT_ORG_ID",
        "PRISM_ACCOUNT_ORG_NAME",
        "PRISM_ACCOUNT_PROJECT_NAME",
    ];

    if let Some(creds) = creds {
        std::env::set_var("MARC27_TOKEN", &creds.access_token);
        std::env::set_var("MARC27_PLATFORM_URL", &creds.platform_url);
        if let Some(project_id) = &creds.project_id {
            std::env::set_var("MARC27_PROJECT_ID", project_id);
        }
        if let Some(user_id) = &creds.user_id {
            std::env::set_var("PRISM_ACCOUNT_USER_ID", user_id);
        }
        if let Some(display_name) = &creds.display_name {
            std::env::set_var("PRISM_ACCOUNT_DISPLAY_NAME", display_name);
        }
        if let Some(org_id) = &creds.org_id {
            std::env::set_var("PRISM_ACCOUNT_ORG_ID", org_id);
        }
        if let Some(org_name) = &creds.org_name {
            std::env::set_var("PRISM_ACCOUNT_ORG_NAME", org_name);
        }
        if let Some(project_name) = &creds.project_name {
            std::env::set_var("PRISM_ACCOUNT_PROJECT_NAME", project_name);
        }
    } else {
        for key in KEYS {
            std::env::remove_var(key);
        }
    }
}

fn open_browser(url: &str) -> Result<()> {
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .status()
    } else {
        std::process::Command::new("xdg-open").arg(url).status()
    }
    .context("failed to spawn browser opener")?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "browser opener exited with status {status}"
        ))
    }
}

fn parse_slash_command(command: &str) -> Result<Option<Vec<String>>> {
    let trimmed = command.trim();
    let Some(rest) = trimmed.strip_prefix('/') else {
        return Ok(None);
    };
    let rest = rest.trim();
    if rest.is_empty() {
        return Ok(Some(Vec::new()));
    }
    shlex::split(rest)
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("Unable to parse slash command: unmatched quotes"))
}

fn parse_command_tail(rest: &str) -> Result<Vec<String>> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    shlex::split(trimmed)
        .ok_or_else(|| anyhow::anyhow!("Unable to parse command arguments: unmatched quotes"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BashSlashAction {
    Execute {
        command: String,
        description: Option<String>,
        timeout: Option<u64>,
        run_in_background: bool,
    },
    Tasks,
    Read {
        task_id: String,
    },
    Stop {
        task_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PythonSlashAction {
    Execute {
        code: String,
        description: Option<String>,
        timeout: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WriteSlashAction {
    Write { path: String, content: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EditSlashAction {
    Edit {
        path: String,
        old_text: String,
        new_text: String,
        replace_all: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffSlashAction {
    Repo,
    Paths { paths: Vec<String> },
}

fn shell_command_join(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| {
            if token.is_empty() {
                return "''".to_string();
            }

            if !token
                .chars()
                .any(|ch| ch.is_whitespace() || matches!(ch, '\'' | '"' | '\\'))
            {
                return token.clone();
            }

            format!("'{}'", token.replace('\'', "'\"'\"'"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_bash_slash_action(command: &str) -> Result<BashSlashAction> {
    let Some(args) = parse_slash_command(command)? else {
        return Err(anyhow::anyhow!("Usage: /bash <command>"));
    };

    if args.first().map(String::as_str) != Some("bash") {
        return Err(anyhow::anyhow!("Usage: /bash <command>"));
    }

    if args.len() == 1 {
        return Err(anyhow::anyhow!(
            "Usage: /bash <command>\n       /bash tasks\n       /bash read <task-id>\n       /bash stop <task-id>"
        ));
    }

    match args[1].as_str() {
        "tasks" if args.len() == 2 => return Ok(BashSlashAction::Tasks),
        "read" if args.len() == 3 => {
            return Ok(BashSlashAction::Read {
                task_id: args[2].clone(),
            })
        }
        "stop" if args.len() == 3 => {
            return Ok(BashSlashAction::Stop {
                task_id: args[2].clone(),
            })
        }
        _ => {}
    }

    let mut run_in_background = false;
    let mut timeout = None;
    let mut description = None;
    let mut index = 1;
    let mut command_tokens: Vec<String> = Vec::new();

    while index < args.len() {
        match args[index].as_str() {
            "--background" | "-b" => {
                run_in_background = true;
                index += 1;
            }
            "--timeout" | "-t" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| anyhow::anyhow!("Usage: /bash --timeout <seconds> <command>"))?;
                timeout = Some(
                    value
                        .parse::<u64>()
                        .context("Invalid /bash timeout value")?,
                );
                index += 2;
            }
            "--description" | "-d" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    anyhow::anyhow!("Usage: /bash --description <text> <command>")
                })?;
                description = Some(value.clone());
                index += 2;
            }
            "--" => {
                command_tokens.extend(args.iter().skip(index + 1).cloned());
                break;
            }
            _ => {
                command_tokens.extend(args.iter().skip(index).cloned());
                break;
            }
        }
    }

    if command_tokens.is_empty() {
        return Err(anyhow::anyhow!("Usage: /bash <command>"));
    }

    Ok(BashSlashAction::Execute {
        command: shell_command_join(&command_tokens),
        description,
        timeout,
        run_in_background,
    })
}

fn split_python_option_prefix(rest: &str) -> Option<(&str, &str)> {
    rest.split_once(" -- ")
        .or_else(|| rest.split_once(" --\n"))
        .or_else(|| rest.split_once("\n--\n"))
}

fn parse_python_slash_action(command: &str) -> Result<PythonSlashAction> {
    let trimmed = command.trim();
    let Some(rest) = trimmed.strip_prefix("/python") else {
        return Err(anyhow::anyhow!("Usage: /python <code>"));
    };
    let rest = rest.trim_start();

    if rest.is_empty() {
        return Err(anyhow::anyhow!(
            "Usage: /python <code>\n       /python --timeout <seconds> --description <text> -- <code>"
        ));
    }

    // Python code should survive this parser verbatim. We only parse shell-like
    // flags when the caller opts into an explicit `--` separator; otherwise the
    // full tail after `/python` is treated as raw code.
    if !rest.starts_with('-') {
        return Ok(PythonSlashAction::Execute {
            code: rest.to_string(),
            description: None,
            timeout: None,
        });
    }

    let (header, code) = split_python_option_prefix(rest).ok_or_else(|| {
        anyhow::anyhow!(
            "Usage: /python <code>\n       /python --timeout <seconds> --description <text> -- <code>"
        )
    })?;

    let header_args = parse_command_tail(header)?;
    let mut timeout = None;
    let mut description = None;
    let mut index = 0;

    while index < header_args.len() {
        match header_args[index].as_str() {
            "--timeout" | "-t" => {
                let value = header_args.get(index + 1).ok_or_else(|| {
                    anyhow::anyhow!("Usage: /python --timeout <seconds> -- <code>")
                })?;
                timeout = Some(
                    value
                        .parse::<u64>()
                        .context("Invalid /python timeout value")?,
                );
                index += 2;
            }
            "--description" | "-d" => {
                let value = header_args.get(index + 1).ok_or_else(|| {
                    anyhow::anyhow!("Usage: /python --description <text> -- <code>")
                })?;
                description = Some(value.clone());
                index += 2;
            }
            other => {
                anyhow::bail!(
                    "Unexpected /python option: {other}. Use `--timeout`, `--description`, and `--` before the code body."
                );
            }
        }
    }

    let code = code.trim_start_matches('\n');
    if code.trim().is_empty() {
        return Err(anyhow::anyhow!("Usage: /python <code>"));
    }

    Ok(PythonSlashAction::Execute {
        code: code.to_string(),
        description,
        timeout,
    })
}

fn parse_read_slash_path(command: &str) -> Result<String> {
    let Some(args) = parse_slash_command(command)? else {
        return Err(anyhow::anyhow!("Usage: /read <path>"));
    };

    if args.first().map(String::as_str) != Some("read") || args.len() != 2 {
        return Err(anyhow::anyhow!("Usage: /read <path>"));
    }

    Ok(args[1].clone())
}

fn split_write_body(rest: &str) -> Option<(&str, &str)> {
    rest.split_once(" -- ")
        .or_else(|| rest.split_once(" --\n"))
        .or_else(|| rest.split_once("\n--\n"))
}

fn parse_write_slash_action(command: &str) -> Result<WriteSlashAction> {
    let trimmed = command.trim_start();
    let Some(rest) = trimmed.strip_prefix("/write") else {
        return Err(anyhow::anyhow!("Usage: /write <path> -- <content>"));
    };
    let rest = rest.trim_start();

    let (header, body) = split_write_body(rest)
        .ok_or_else(|| anyhow::anyhow!("Usage: /write <path> -- <content>"))?;
    let header_args = parse_command_tail(header)?;
    if header_args.len() != 1 {
        return Err(anyhow::anyhow!("Usage: /write <path> -- <content>"));
    }

    // Preserve the file body verbatim after the explicit separator so pasted
    // config blocks or source files do not get tokenized by the slash parser.
    let content = body.trim_start_matches('\n').to_string();
    Ok(WriteSlashAction::Write {
        path: header_args[0].clone(),
        content,
    })
}

fn split_edit_segment<'a>(input: &'a str, markers: &[&str]) -> Option<(&'a str, &'a str)> {
    for marker in markers {
        if let Some(parts) = input.split_once(marker) {
            return Some(parts);
        }
    }
    None
}

fn trim_command_body_padding(text: &str) -> &str {
    let text = text.strip_prefix(' ').unwrap_or(text);
    text.strip_prefix('\n').unwrap_or(text)
}

fn parse_edit_slash_action(command: &str) -> Result<EditSlashAction> {
    let trimmed = command.trim_start();
    let Some(rest) = trimmed.strip_prefix("/edit") else {
        return Err(anyhow::anyhow!(
            "Usage: /edit <path> --old -- <old> --new -- <new>"
        ));
    };
    let rest = rest.trim_start();

    let replace_all = rest.starts_with("--all ");
    let rest = if replace_all {
        rest.trim_start_matches("--all ").trim_start()
    } else {
        rest
    };

    let (header, old_and_new) = split_edit_segment(rest, &[" --old --", "--old --"])
        .ok_or_else(|| anyhow::anyhow!("Usage: /edit <path> --old -- <old> --new -- <new>"))?;
    let header_args = parse_command_tail(header)?;
    if header_args.len() != 1 {
        return Err(anyhow::anyhow!(
            "Usage: /edit <path> --old -- <old> --new -- <new>"
        ));
    }

    let (old_text, new_text) =
        split_edit_segment(old_and_new, &[" --new --", "\n--new --", "--new --"])
            .ok_or_else(|| anyhow::anyhow!("Usage: /edit <path> --old -- <old> --new -- <new>"))?;

    Ok(EditSlashAction::Edit {
        path: header_args[0].clone(),
        old_text: trim_command_body_padding(old_text).to_string(),
        new_text: trim_command_body_padding(new_text).to_string(),
        replace_all,
    })
}

fn parse_diff_slash_action(command: &str) -> Result<DiffSlashAction> {
    let Some(args) = parse_slash_command(command)? else {
        return Err(anyhow::anyhow!("Usage: /diff [path ...]"));
    };

    if args.first().map(String::as_str) != Some("diff") {
        return Err(anyhow::anyhow!("Usage: /diff [path ...]"));
    }

    if args.len() == 1 {
        return Ok(DiffSlashAction::Repo);
    }

    Ok(DiffSlashAction::Paths {
        paths: args.into_iter().skip(1).collect(),
    })
}

fn next_manual_call_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("cmd_{:08x}", (nanos ^ (nanos >> 32)) & 0xFFFF_FFFF)
}

fn interactive_policy_principal() -> String {
    std::env::var("PRISM_ACCOUNT_USER_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("PRISM_ACCOUNT_DISPLAY_NAME")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| "local-user".to_string())
}

fn interactive_policy_role() -> String {
    // Slash commands originate from an explicit local operator action rather
    // than an autonomous model step, so the policy default should line up with
    // the interactive CLI rather than the agent role.
    std::env::var("PRISM_ACCOUNT_ROLE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "operator".to_string())
}

fn manual_tool_preview(tool_name: &str, args: &Value) -> Option<String> {
    match tool_name {
        "read_file" => args
            .get("path")
            .and_then(|value| value.as_str())
            .map(|path| format!("read {}", path)),
        "edit_file" => args
            .get("path")
            .and_then(|value| value.as_str())
            .map(|path| format!("edit {}", path)),
        "write_file" => args
            .get("path")
            .and_then(|value| value.as_str())
            .map(|path| format!("write {}", path)),
        "execute_bash" => args
            .get("command")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|command| format!("$ {}", command.lines().next().unwrap_or(command))),
        "execute_python" => args
            .get("description")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|description| format!("python: {description}"))
            .or_else(|| {
                args.get("code")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|code| format!("python: {}", code.lines().next().unwrap_or(code)))
            }),
        "list_bash_tasks" => Some("list bash tasks".to_string()),
        "read_bash_task" => args
            .get("task_id")
            .and_then(|value| value.as_str())
            .map(|task_id| format!("read bash task {task_id}")),
        "stop_bash_task" => args
            .get("task_id")
            .and_then(|value| value.as_str())
            .map(|task_id| format!("stop bash task {task_id}")),
        _ => None,
    }
}

fn summarize_manual_tool_result(
    tool_name: &str,
    preview: Option<&str>,
    content: &str,
    is_error: bool,
) -> String {
    if is_error {
        let short = if content.len() > 80 {
            &content[..80]
        } else {
            content
        };
        return format!("{tool_name}: error — {short}");
    }

    if let Ok(value) = serde_json::from_str::<Value>(content) {
        if let Some(task) = value.get("task") {
            if let Some(task_id) = task.get("task_id").and_then(|item| item.as_str()) {
                let status = task
                    .get("status")
                    .and_then(|item| item.as_str())
                    .unwrap_or("unknown");
                return format!("{tool_name}: {task_id} ({status})");
            }
        }
        if let Some(tasks) = value.get("tasks").and_then(|item| item.as_array()) {
            return format!("{tool_name}: {} tasks", tasks.len());
        }
    }

    preview
        .map(str::to_string)
        .unwrap_or_else(|| format!("{tool_name}: completed"))
}

async fn execute_manual_tool_call(
    command_label: &str,
    tool_name: &str,
    args: Value,
    tool_server: &mut ToolServerHandle,
    session_store: &mut SessionStore,
    transcript: &mut TranscriptStore,
    permissions: &ToolPermissionContext,
    policy_engine: &mut Option<prism_policy::PolicyEngine>,
) -> Result<()> {
    let call_id = next_manual_call_id();
    let preview = manual_tool_preview(tool_name, &args);

    // Direct `/bash` commands are explicit user intent, so they bypass the
    // normal approval prompt while still respecting plan-mode and policy gates.
    session_store.append_message(
        "user",
        command_label,
        "",
        "",
        Some(serde_json::json!({ "command_kind": "slash" })),
    );
    transcript.append(TranscriptEntry::new("user", command_label));

    emit_agent_event(AgentEvent::ToolCallStart {
        tool_name: tool_name.to_string(),
        call_id: call_id.clone(),
        preview: preview.clone(),
    });

    let permission_decision = permissions.decision_for(tool_name, None);
    if permission_decision.blocked {
        let message = format!("Tool '{tool_name}' is blocked by the current permission mode.");
        emit_agent_event(AgentEvent::ToolCallResult {
            call_id: call_id.clone(),
            tool_name: tool_name.to_string(),
            content: message.clone(),
            summary: Some(format!("{tool_name}: blocked")),
            preview,
            elapsed_ms: 0,
            is_error: true,
        });
        session_store.append_message("tool", &message, tool_name, &call_id, None);
        transcript.append(TranscriptEntry::new("tool", &message).with_tool_name(tool_name));
        emit_notification("ui.turn.complete", serde_json::json!({}));
        return Ok(());
    }

    if let Some(ref mut pe) = policy_engine {
        let principal = interactive_policy_principal();
        let role = interactive_policy_role();
        let policy_input = prism_policy::PolicyInput {
            action: "tool.call".to_string(),
            principal,
            role,
            resource: tool_name.to_string(),
            context: args.clone(),
        };
        if let Ok(decision) = pe.evaluate(&policy_input) {
            if !decision.allowed {
                let reason = if decision.violations.is_empty() {
                    decision.reason
                } else {
                    decision.violations.join("; ")
                };
                let message = format!("Tool '{tool_name}' denied by policy: {reason}");
                emit_agent_event(AgentEvent::ToolCallResult {
                    call_id: call_id.clone(),
                    tool_name: tool_name.to_string(),
                    content: message.clone(),
                    summary: Some(format!("{tool_name}: denied by policy")),
                    preview,
                    elapsed_ms: 0,
                    is_error: true,
                });
                session_store.append_message("tool", &message, tool_name, &call_id, None);
                transcript.append(TranscriptEntry::new("tool", &message).with_tool_name(tool_name));
                emit_notification("ui.turn.complete", serde_json::json!({}));
                return Ok(());
            }
        }
    }

    let started = Instant::now();
    let result = tool_server.call_tool(tool_name, args).await;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let (raw_content, is_error) = match result {
        Ok(resp) => {
            if let Some(err) = resp.get("error").and_then(|value| value.as_str()) {
                (err.to_string(), true)
            } else if let Some(value) = resp.get("result") {
                (serde_json::to_string(value).unwrap_or_default(), false)
            } else {
                (serde_json::to_string(&resp).unwrap_or_default(), false)
            }
        }
        Err(error) => (format!("Tool error: {error}"), true),
    };

    let summary =
        summarize_manual_tool_result(tool_name, preview.as_deref(), &raw_content, is_error);
    let (display_content, _) = build_tool_card_payload(
        tool_name,
        &raw_content,
        preview.as_deref(),
        Some(summary.as_str()),
    );

    emit_agent_event(AgentEvent::ToolCallResult {
        call_id: call_id.clone(),
        tool_name: tool_name.to_string(),
        content: raw_content,
        summary: Some(summary),
        preview,
        elapsed_ms,
        is_error,
    });

    session_store.append_message("tool", &display_content, tool_name, &call_id, None);
    transcript.append(TranscriptEntry::new("tool", &display_content).with_tool_name(tool_name));
    emit_notification("ui.turn.complete", serde_json::json!({}));
    Ok(())
}

fn command_timeout_for_root(root: &str) -> Duration {
    match root {
        "workflow" | "ingest" | "query" | "run" | "research" | "deploy" | "publish" => {
            Duration::from_secs(300)
        }
        "node" | "mesh" => Duration::from_secs(60),
        _ => Duration::from_secs(30),
    }
}

fn truncate_for_ui(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect::<String>() + "\n\n[Output truncated]"
}

fn format_cli_output(
    invocation: &str,
    stdout: &str,
    stderr: &str,
    success: bool,
    code: Option<i32>,
) -> String {
    let stdout = stdout.trim();
    let stderr = stderr.trim();

    if success {
        match (stdout.is_empty(), stderr.is_empty()) {
            (true, true) => format!("`{invocation}` completed."),
            (false, true) => stdout.to_string(),
            (true, false) => format!("`{invocation}` completed with stderr:\n{stderr}"),
            (false, false) => format!("{stdout}\n\n[stderr]\n{stderr}"),
        }
    } else {
        let exit_code = code
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        match (stdout.is_empty(), stderr.is_empty()) {
            (true, true) => format!("`{invocation}` failed with exit code {exit_code}."),
            (false, true) => {
                format!("`{invocation}` failed with exit code {exit_code}.\n\n{stdout}")
            }
            (true, false) => {
                format!("`{invocation}` failed with exit code {exit_code}.\n\n{stderr}")
            }
            (false, false) => {
                format!("`{invocation}` failed with exit code {exit_code}.\n\n{stdout}\n\n[stderr]\n{stderr}")
            }
        }
    }
}

async fn run_cli_backed_slash_command(
    args: &[String],
    slash_ctx: &SlashCommandContext,
) -> Result<String> {
    if args.is_empty() {
        return Ok(
            "Enter a slash command such as `/status`, `/query ...`, or `/workflow list`."
                .to_string(),
        );
    }

    let root = args[0].as_str();
    if !is_cli_backed_slash_root(root) {
        return Ok(String::new());
    }

    if matches!(root, "setup" | "backend") {
        return Ok(format!(
            "`/{root}` is not available inside the embedded REPL. Run `prism {root}` from your shell."
        ));
    }

    let invocation = format!("prism {}", args.join(" "));
    let timeout_secs = command_timeout_for_root(root).as_secs();

    let mut cmd = TokioCommand::new(&slash_ctx.current_exe);
    cmd.arg("--project-root")
        .arg(&slash_ctx.project_root)
        .arg("--python")
        .arg(&slash_ctx.python_bin)
        .args(args)
        .current_dir(&slash_ctx.project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = match timeout(command_timeout_for_root(root), cmd.output()).await {
        Ok(result) => result.context("failed to run embedded CLI command")?,
        Err(_) => {
            return Ok(format!(
                "`{invocation}` is still running after {timeout_secs} seconds. Run it in your shell for an interactive or long-lived session."
            ));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(truncate_for_ui(
        &format_cli_output(
            &invocation,
            &stdout,
            &stderr,
            output.status.success(),
            output.status.code(),
        ),
        30_000,
    ))
}

fn transcript_text(transcript: &TranscriptStore) -> String {
    transcript
        .entries
        .iter()
        .filter(|entry| !entry.content.trim().is_empty())
        .map(|entry| entry.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn inline_list(items: &[String], empty: &str, limit: usize) -> String {
    if items.is_empty() {
        return empty.to_string();
    }

    let visible = items.iter().take(limit).cloned().collect::<Vec<_>>();
    if items.len() > limit {
        format!(
            "{}, ... (+{} more)",
            visible.join(", "),
            items.len() - limit
        )
    } else {
        visible.join(", ")
    }
}

fn numbered_section(items: &[String], empty: &str) -> String {
    if items.is_empty() {
        return format!("  {empty}");
    }

    items
        .iter()
        .enumerate()
        .map(|(index, item)| format!("  {}. {}", index + 1, item))
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_permission_context(mode: SessionMode, tools: &ToolCatalog) -> ToolPermissionContext {
    // Python tools already declare whether they need an approval prompt. PRISM
    // can safely auto-approve loaded read-only tools that opt out of approval
    // instead of keeping a second hand-maintained allowlist in Rust.
    let dynamic_auto_approved = tools
        .iter()
        .filter(|tool| tool.permission_mode == PermissionMode::ReadOnly && !tool.requires_approval)
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    let base = ToolPermissionContext::default().with_auto_approve(&dynamic_auto_approved);
    if mode == SessionMode::Chat {
        return base;
    }

    let (_, workspace_write, full_access, _, _) = loaded_tools_by_access(tools);
    let mut deny = workspace_write;
    deny.extend(full_access);
    base.with_deny(&deny, &[])
}

// Session-local overrides layer on top of the mode baseline. Plan mode still
// wins because a tool present in both allow and deny remains blocked.
fn build_effective_permission_context(
    mode: SessionMode,
    tools: &ToolCatalog,
    overrides: &PermissionOverrides,
) -> ToolPermissionContext {
    let base = build_permission_context(mode, tools);
    let deny_names = overrides.deny_names().cloned().collect::<Vec<_>>();
    let allow_names = overrides.allow_names().cloned().collect::<Vec<_>>();
    base.with_deny(&deny_names, &[])
        .with_auto_approve(&allow_names)
}

fn build_runtime_session_state(
    session_mode: SessionMode,
    overrides: &PermissionOverrides,
    plan_state: &PlanRuntimeState,
) -> RuntimeSessionState {
    RuntimeSessionState {
        session_mode: session_mode.as_str().to_string(),
        permission_allow: overrides.allow_names().cloned().collect::<Vec<_>>(),
        permission_deny: overrides.deny_names().cloned().collect::<Vec<_>>(),
        plan_status: plan_state
            .status
            .unwrap_or(PlanStatus::None)
            .as_str()
            .to_string(),
        approved_plan_body: plan_state.approved_plan_body.clone(),
    }
}

fn restore_runtime_session_state(
    state: RuntimeSessionState,
) -> (SessionMode, PermissionOverrides, PlanRuntimeState) {
    let session_mode = match state.session_mode.as_str() {
        "plan" => SessionMode::Plan,
        _ => SessionMode::Chat,
    };

    let mut overrides = PermissionOverrides::default();
    for tool_name in state.permission_allow {
        overrides.allow(&tool_name);
    }
    for tool_name in state.permission_deny {
        overrides.deny(&tool_name);
    }

    (
        session_mode,
        overrides,
        PlanRuntimeState {
            status: Some(PlanStatus::from_str(&state.plan_status)),
            approved_plan_body: state.approved_plan_body,
        },
    )
}

fn persist_runtime_state(
    session_store: &SessionStore,
    session_mode: SessionMode,
    overrides: &PermissionOverrides,
    plan_state: &PlanRuntimeState,
) {
    if let Some(session_id) = session_store.current_id() {
        let runtime = build_runtime_session_state(session_mode, overrides, plan_state);
        session_store.save_runtime_state(session_id, &runtime);
    }
}

async fn sync_live_permission_overrides(
    live_overrides: &SharedPermissionOverrides,
    overrides: &PermissionOverrides,
) {
    // The running turn consults this shared snapshot for later tool calls, so
    // protocol-side session edits need to land here immediately.
    *live_overrides.write().await = overrides.clone();
}

fn apply_deferred_runtime_updates(
    runtime: &mut ServerRuntime,
    tools: &ToolCatalog,
    updates: &mut Vec<DeferredRuntimeUpdate>,
) {
    if updates.is_empty() {
        return;
    }

    for update in updates.drain(..) {
        match update {
            DeferredRuntimeUpdate::AllowTool(tool_name) => {
                runtime.permission_overrides.allow(&tool_name)
            }
            DeferredRuntimeUpdate::DenyTool(tool_name) => {
                runtime.permission_overrides.deny(&tool_name)
            }
        }
    }

    runtime.permissions = build_effective_permission_context(
        runtime.session_mode,
        tools,
        &runtime.permission_overrides,
    );
}

fn emit_view(view_type: &str, title: &str, body: &str, tone: &str) {
    // `ui.view` is the portable command-screen primitive. The Ink TUI uses it
    // today, but the same payload shape is what a VSX/desktop renderer should
    // consume instead of reaching into backend internals directly.
    emit_notification(
        "ui.view",
        serde_json::json!({
            "view_type": view_type,
            "title": title,
            "body": body,
            "tone": tone,
        }),
    );
}

fn emit_tabbed_view(
    view_type: &str,
    title: &str,
    tabs: &[(&str, &str, &str, &str)],
    selected_tab: &str,
    tone: &str,
    footer: &str,
) {
    // Tabbed views keep richer command state out of the transcript transport
    // stream while still giving frontends enough structure to build settings,
    // permissions, workflow, or deploy screens natively.
    let tabs = tabs
        .iter()
        .map(|(id, title, body, tab_tone)| {
            serde_json::json!({
                "id": id,
                "title": title,
                "body": body,
                "tone": tab_tone,
            })
        })
        .collect::<Vec<_>>();

    emit_notification(
        "ui.view",
        serde_json::json!({
            "view_type": view_type,
            "title": title,
            "tone": tone,
            "tabs": tabs,
            "selected_tab": selected_tab,
            "footer": footer,
        }),
    );
}

fn emit_status_snapshot(
    auto_approve: bool,
    transcript: &TranscriptStore,
    session_mode: SessionMode,
    plan_state: &PlanRuntimeState,
) {
    emit_notification(
        "ui.status",
        serde_json::json!({
            "auto_approve": auto_approve,
            "message_count": transcript.entries.len(),
            "has_plan": session_mode == SessionMode::Plan,
            "session_mode": session_mode.as_str(),
            "plan_status": plan_state.status.unwrap_or(PlanStatus::None).as_str(),
        }),
    );
}

fn system_prompt_for_mode(
    mode: SessionMode,
    base_prompt: &str,
    plan_state: &PlanRuntimeState,
    tools: &ToolCatalog,
) -> String {
    let base_prompt = append_runtime_tool_guidance(base_prompt, tools);
    match mode {
        SessionMode::Chat => {
            if let Some(approved_plan) = &plan_state.approved_plan_body {
                // Keep approved planning work visible during execution turns so
                // the agent can follow through consistently after plan review.
                format!(
                    "{base_prompt}\n\nThe user approved the following execution plan. Follow it unless the user explicitly changes direction.\n<approved_plan>\n{approved_plan}\n</approved_plan>"
                )
            } else {
                base_prompt
            }
        }
        SessionMode::Plan => format!(
            "{base_prompt}\n\nYou are in plan mode. Focus on analysis, constraints, sequencing, and concrete implementation planning. Do not edit files or rely on write/execute tools; those actions are blocked in this mode. Produce clear planning output that can guide later execution."
        ),
    }
}

fn loaded_tools_by_access(
    tools: &ToolCatalog,
) -> (
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
) {
    let mut read_only = Vec::new();
    let mut workspace_write = Vec::new();
    let mut full_access = Vec::new();
    let mut approval_required = Vec::new();
    let mut tool_names = Vec::new();

    for tool in tools.iter() {
        let name = tool.name.clone();
        tool_names.push(name.clone());
        if tool.requires_approval {
            approval_required.push(name.clone());
        }
        match tool.permission_mode {
            PermissionMode::ReadOnly => read_only.push(name),
            PermissionMode::WorkspaceWrite => workspace_write.push(name),
            PermissionMode::FullAccess => full_access.push(name),
        }
    }

    read_only.sort();
    workspace_write.sort();
    full_access.sort();
    approval_required.sort();
    tool_names.sort();

    let mut all = read_only.clone();
    all.extend(workspace_write.clone());
    all.extend(full_access.clone());
    all.sort();

    (
        read_only,
        workspace_write,
        full_access,
        approval_required,
        tool_names,
    )
}

fn resolve_loaded_tool_name(tool_name: &str, tools: &ToolCatalog) -> Option<String> {
    tools.find(tool_name).map(|tool| tool.name.clone())
}

fn restore_history_and_transcript_from_messages(
    history: &mut Vec<ChatMessage>,
    transcript: &mut TranscriptStore,
    scratchpad: &mut Scratchpad,
    messages: &[serde_json::Value],
) {
    history.clear();
    let budget = transcript.budget.clone();
    *transcript = TranscriptStore::new(Some(budget));
    *scratchpad = Scratchpad::new();

    for msg in messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if role.is_empty() || content.is_empty() {
            continue;
        }

        history.push(ChatMessage {
            role: role.to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: msg
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .map(|value| value.to_string()),
        });

        let mut entry = TranscriptEntry::new(role, content);
        if let Some(tool_name) = msg.get("tool_name").and_then(|v| v.as_str()) {
            if !tool_name.is_empty() {
                entry = entry.with_tool_name(tool_name);
            }
        }
        transcript.append(entry);
    }
}

// Compaction replaces older history with a synthetic system summary. `/context`
// should report from that visible boundary onward because that is the history
// slice the model actually sees on subsequent turns.
fn is_compact_boundary_message(message: &ChatMessage) -> bool {
    message.role == "system"
        && message
            .content
            .as_deref()
            .unwrap_or_default()
            .starts_with("[Conversation context compacted]")
}

fn project_api_history(history: &[ChatMessage]) -> &[ChatMessage] {
    match history.iter().rposition(is_compact_boundary_message) {
        Some(index) => &history[index..],
        None => history,
    }
}

// Use a cheap, stable estimate for context reporting. This is not meant to be
// billing-accurate; it is only there to show relative prompt size in the TUI.
fn estimate_token_count(text: &str) -> usize {
    let chars = text.chars().count();
    let words = text.split_whitespace().count();
    std::cmp::max(words, (chars + 3) / 4)
}

fn preview_text(text: &str, max_chars: usize) -> String {
    let flat = text.replace('\n', " ");
    let total = flat.chars().count();
    if total <= max_chars {
        return flat;
    }
    flat.chars().take(max_chars).collect::<String>() + "..."
}

#[derive(Debug, Clone)]
struct ApiViewSummary {
    visible_messages: usize,
    system_messages: usize,
    user_messages: usize,
    assistant_messages: usize,
    tool_messages: usize,
    tool_call_count: usize,
    system_prompt_tokens: usize,
    visible_history_tokens: usize,
    total_estimated_tokens: usize,
    compact_boundary_preview: Option<String>,
    visible_previews: Vec<String>,
}

// Summarize the exact history slice PRISM sends to the LLM: current system
// prompt plus the compaction-aware message window.
fn summarize_api_view(history: &[ChatMessage], system_prompt: &str) -> ApiViewSummary {
    let visible = project_api_history(history);
    let mut system_messages = 0;
    let mut user_messages = 0;
    let mut assistant_messages = 0;
    let mut tool_messages = 0;
    let mut tool_call_count = 0;
    let mut visible_history_tokens = 0;
    let mut compact_boundary_preview = None;

    for message in visible {
        let content = message.content.as_deref().unwrap_or_default();
        visible_history_tokens += estimate_token_count(content);
        tool_call_count += message
            .tool_calls
            .as_ref()
            .map(|calls| calls.len())
            .unwrap_or(0);

        match message.role.as_str() {
            "system" => {
                system_messages += 1;
                if compact_boundary_preview.is_none() && is_compact_boundary_message(message) {
                    let summary = content
                        .lines()
                        .skip(1)
                        .take(2)
                        .collect::<Vec<_>>()
                        .join(" ");
                    if !summary.trim().is_empty() {
                        compact_boundary_preview = Some(preview_text(&summary, 120));
                    }
                }
            }
            "user" => user_messages += 1,
            "assistant" => assistant_messages += 1,
            "tool" => tool_messages += 1,
            _ => {}
        }
    }

    let visible_previews = visible
        .iter()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|message| {
            let role = match message.role.as_str() {
                "system" if is_compact_boundary_message(message) => "compact-boundary",
                other => other,
            };

            let body = if let Some(tool_calls) = &message.tool_calls {
                if !tool_calls.is_empty() {
                    let tools = tool_calls
                        .iter()
                        .map(|call| call.function.name.as_str())
                        .collect::<Vec<_>>();
                    format!("tool calls: {}", tools.join(", "))
                } else {
                    preview_text(message.content.as_deref().unwrap_or_default(), 100)
                }
            } else {
                let content = message.content.as_deref().unwrap_or_default();
                if content.trim().is_empty() && message.role == "tool" {
                    "(empty tool result)".to_string()
                } else {
                    preview_text(content, 100)
                }
            };

            format!("{role}: {body}")
        })
        .collect::<Vec<_>>();

    let system_prompt_tokens = estimate_token_count(system_prompt);

    ApiViewSummary {
        visible_messages: visible.len(),
        system_messages,
        user_messages,
        assistant_messages,
        tool_messages,
        tool_call_count,
        system_prompt_tokens,
        visible_history_tokens,
        total_estimated_tokens: system_prompt_tokens + visible_history_tokens,
        compact_boundary_preview,
        visible_previews,
    }
}

fn format_context_report(
    slash_ctx: &SlashCommandContext,
    session_store: &SessionStore,
    history: &[ChatMessage],
    llm_config: &LlmConfig,
    system_prompt: &str,
    transcript: &TranscriptStore,
    scratchpad: &Scratchpad,
    permissions: &ToolPermissionContext,
    tools: &ToolCatalog,
    plan_state: &PlanRuntimeState,
) -> String {
    let api_view = summarize_api_view(history, system_prompt);
    let transcript_blob = transcript_text(transcript);
    let pending = extract_pending_work(&transcript_blob, 5);
    let key_files = extract_key_files(&transcript_blob, 8);
    let warning = transcript
        .budget_warning()
        .unwrap_or_else(|| "none".to_string());
    let compacted = transcript
        .entries
        .first()
        .map(|entry| {
            entry.role == "system"
                && entry
                    .content
                    .starts_with("[Conversation context compacted]")
        })
        .unwrap_or(false);
    let (read_only, workspace_write, full_access, approval_required, tool_names) =
        loaded_tools_by_access(tools);
    let auto_approved = tool_names
        .iter()
        .filter(|name| permissions.auto_approves(name))
        .cloned()
        .collect::<Vec<_>>();
    let blocked = tool_names
        .iter()
        .filter(|name| permissions.blocks(name))
        .cloned()
        .collect::<Vec<_>>();
    let current_session = session_store
        .current_id()
        .unwrap_or(transcript.session_id.as_str());
    let meta_turns = session_store
        .meta()
        .map(|meta| meta.turn_count)
        .unwrap_or(0);
    let meta_compactions = session_store
        .meta()
        .map(|meta| meta.compaction_count)
        .unwrap_or(0);

    let lines = vec![
        "Context".to_string(),
        format!("  session: {current_session}"),
        format!("  model: {}", llm_config.model),
        format!("  project root: {}", slash_ctx.project_root.display()),
        format!("  python: {}", slash_ctx.python_bin.display()),
        format!("  raw history messages: {}", history.len()),
        format!(
            "  approved plan context: {}",
            if plan_state.approved_plan_body.is_some() {
                "loaded into execution prompt"
            } else {
                "none"
            }
        ),
        format!(
            "  model-facing API view: {} messages (system {}, user {}, assistant {}, tool {})",
            api_view.visible_messages,
            api_view.system_messages,
            api_view.user_messages,
            api_view.assistant_messages,
            api_view.tool_messages
        ),
        format!(
            "  estimated prompt tokens: {} total (system prompt {} + visible history {})",
            api_view.total_estimated_tokens,
            api_view.system_prompt_tokens,
            api_view.visible_history_tokens
        ),
        format!(
            "  assistant tool calls in view: {}",
            api_view.tool_call_count
        ),
        format!(
            "  compact boundary: {}",
            api_view
                .compact_boundary_preview
                .as_deref()
                .unwrap_or("none; full visible history is in play")
        ),
        format!(
            "  transcript: {} entries, {} turns (session meta: {} turns)",
            transcript.entries.len(),
            transcript.turn_count,
            meta_turns
        ),
        format!(
            "  token usage: {} input / {} output ({} events)",
            transcript.cost.total_input,
            transcript.cost.total_output,
            transcript.cost.events.len()
        ),
        format!(
            "  budget: {} / {} turns, {} / {} input tokens",
            transcript.turn_count,
            transcript.budget.max_turns,
            transcript.cost.total_input,
            transcript.budget.max_input_tokens
        ),
        format!("  warning: {warning}"),
        format!(
            "  compacted transcript: {}",
            if compacted { "yes" } else { "no" }
        ),
        format!("  compactions recorded: {meta_compactions}"),
        String::new(),
        "Tools".to_string(),
        format!(
            "  loaded: {} total (read-only {}, workspace-write {}, full-access {})",
            tools.len(),
            read_only.len(),
            workspace_write.len(),
            full_access.len()
        ),
        format!("  approval-required tools: {}", approval_required.len()),
        format!("  auto-approved now: {}", auto_approved.len()),
        format!("  blocked now: {}", blocked.len()),
        String::new(),
        "Scratchpad".to_string(),
        format!("  entries: {}", scratchpad.entries().len()),
        String::new(),
        "Recent API View".to_string(),
        numbered_section(&api_view.visible_previews, "(no visible messages yet)"),
        String::new(),
        "Pending Work".to_string(),
        numbered_section(&pending, "(none inferred yet)"),
        String::new(),
        "Key Files".to_string(),
        numbered_section(&key_files, "(none detected yet)"),
    ];

    truncate_for_ui(&lines.join("\n"), 30_000)
}

fn format_status_report(
    slash_ctx: &SlashCommandContext,
    session_store: &SessionStore,
    llm_config: &LlmConfig,
    transcript: &TranscriptStore,
    permissions: &ToolPermissionContext,
    tools: &ToolCatalog,
    session_mode: SessionMode,
    plan_state: &PlanRuntimeState,
    auto_approve: bool,
    account: Option<&StoredCredentials>,
) -> String {
    let (read_only, workspace_write, full_access, approval_required, tool_names) =
        loaded_tools_by_access(tools);
    let current_session = session_store
        .current_id()
        .unwrap_or(transcript.session_id.as_str());
    let auto_approved_count = tool_names
        .iter()
        .filter(|name| permissions.auto_approves(name))
        .count();
    let blocked_count = tool_names
        .iter()
        .filter(|name| permissions.blocks(name))
        .count();
    let account_summary = match account {
        Some(creds) => format!(
            "Account\n  user: {}\n  org: {}\n  project: {}\n  platform: {}",
            creds.display_name.as_deref().unwrap_or("(unknown)"),
            creds.org_name.as_deref().unwrap_or("(none)"),
            creds.project_name.as_deref().unwrap_or("(none)"),
            creds.platform_url,
        ),
        None => "Account\n  not logged in".to_string(),
    };

    truncate_for_ui(
        &format!(
            "Runtime\n  session: {current_session}\n  model: {}\n  mode: {}\n  plan status: {}\n  project root: {}\n  python: {}\n  auto-approve: {}\n\n{}\n\nConversation\n  transcript entries: {}\n  turns: {}\n  compactions: {}\n\nTools\n  loaded: {} total\n  read-only: {}\n  workspace-write: {}\n  full-access: {}\n  approval-required: {}\n  auto-approved now: {}\n  blocked now: {}",
            llm_config.model,
            session_mode.as_str(),
            plan_state.status.unwrap_or(PlanStatus::None).as_str(),
            slash_ctx.project_root.display(),
            slash_ctx.python_bin.display(),
            if auto_approve { "on" } else { "off" },
            account_summary,
            transcript.entries.len(),
            transcript.turn_count,
            session_store
                .meta()
                .map(|meta| meta.compaction_count)
                .unwrap_or(0),
            tools.len(),
            read_only.len(),
            workspace_write.len(),
            full_access.len(),
            approval_required.len(),
            auto_approved_count,
            blocked_count,
        ),
        30_000,
    )
}

fn format_permissions_report(
    permissions: &ToolPermissionContext,
    overrides: &PermissionOverrides,
    tools: &ToolCatalog,
) -> String {
    let (read_only, workspace_write, full_access, approval_required, tool_names) =
        loaded_tools_by_access(tools);
    let auto_approved = tool_names
        .iter()
        .filter(|name| permissions.auto_approves(name))
        .cloned()
        .collect::<Vec<_>>();
    let blocked = tool_names
        .iter()
        .filter(|name| permissions.blocks(name))
        .cloned()
        .collect::<Vec<_>>();
    let allow_overrides = overrides.allow_names().cloned().collect::<Vec<_>>();
    let deny_overrides = overrides.deny_names().cloned().collect::<Vec<_>>();

    truncate_for_ui(
        &format!(
            "Permissions\n  auto-approved loaded tools ({}): {}\n  blocked loaded tools ({}): {}\n  approval-required tools ({}): {}\n  session allow overrides ({}): {}\n  session deny overrides ({}): {}\n\nLoaded tools by minimum access\n  read-only ({}): {}\n  workspace-write ({}): {}\n  full-access ({}): {}",
            auto_approved.len(),
            inline_list(&auto_approved, "none", 12),
            blocked.len(),
            inline_list(&blocked, "none", 12),
            approval_required.len(),
            inline_list(&approval_required, "none", 12),
            allow_overrides.len(),
            inline_list(&allow_overrides, "none", 12),
            deny_overrides.len(),
            inline_list(&deny_overrides, "none", 12),
            read_only.len(),
            inline_list(&read_only, "none", 12),
            workspace_write.len(),
            inline_list(&workspace_write, "none", 12),
            full_access.len(),
            inline_list(&full_access, "none", 12),
        ),
        30_000,
    )
}

fn format_tool_entry(tool_name: &str, tools: &ToolCatalog) -> String {
    match tools.find(tool_name) {
        Some(tool) => format!(
            "{} · {} · {}\n  {}",
            tool.name,
            tool.permission_mode.as_str(),
            if tool.requires_approval {
                "approval required"
            } else {
                "no approval prompt by default"
            },
            tool.description
        ),
        None => tool_name.to_string(),
    }
}

fn format_tools_summary_report(tools: &ToolCatalog, permissions: &ToolPermissionContext) -> String {
    let (read_only, workspace_write, full_access, approval_required, tool_names) =
        loaded_tools_by_access(tools);
    let auto_approved = tool_names
        .iter()
        .filter(|name| permissions.auto_approves(name))
        .count();
    let blocked = tool_names
        .iter()
        .filter(|name| permissions.blocks(name))
        .count();
    let bash_status = tools.find("execute_bash").map(|tool| {
        format!(
            "loaded · {} · {}",
            tool.permission_mode.as_str(),
            if tool.requires_approval {
                "approval required"
            } else {
                "no approval prompt by default"
            }
        )
    });

    truncate_for_ui(
        &format!(
            "Tools\n  loaded: {}\n  read-only: {}\n  workspace-write: {}\n  full-access: {}\n  approval-required: {}\n  auto-approved now: {}\n  blocked now: {}\n  execute_bash: {}",
            tools.len(),
            read_only.len(),
            workspace_write.len(),
            full_access.len(),
            approval_required.len(),
            auto_approved,
            blocked,
            bash_status.as_deref().unwrap_or("not loaded"),
        ),
        30_000,
    )
}

fn format_usage_report(transcript: &TranscriptStore, session_store: &SessionStore) -> String {
    let budget_warning = transcript
        .budget_warning()
        .unwrap_or_else(|| "none".to_string());

    truncate_for_ui(
        &format!(
            "Usage\n  input tokens: {}\n  output tokens: {}\n  cost events: {}\n  budget warning: {}\n\nSession\n  turns: {}\n  stored turns: {}\n  compactions: {}",
            transcript.cost.total_input,
            transcript.cost.total_output,
            transcript.cost.events.len(),
            budget_warning,
            transcript.turn_count,
            session_store.meta().map(|meta| meta.turn_count).unwrap_or(0),
            session_store
                .meta()
                .map(|meta| meta.compaction_count)
                .unwrap_or(0),
        ),
        30_000,
    )
}

fn format_account_result(
    title: &str,
    creds: &StoredCredentials,
    notes: &[String],
    paths: &PrismPaths,
) -> String {
    let mut lines = vec![
        title.to_string(),
        format!(
            "  user: {}",
            creds.display_name.as_deref().unwrap_or("(unknown)")
        ),
        format!("  org: {}", creds.org_name.as_deref().unwrap_or("(none)")),
        format!(
            "  project: {}",
            creds.project_name.as_deref().unwrap_or("(none)")
        ),
        format!("  platform: {}", creds.platform_url),
        String::new(),
        "Stored state".to_string(),
        format!("  cli state: {}", paths.cli_state_path().display()),
        "  sdk credentials: ~/.prism/credentials.json".to_string(),
    ];

    if !notes.is_empty() {
        lines.push(String::new());
        lines.push("Selection notes".to_string());
        lines.push(numbered_section(notes, "(none)"));
    }

    truncate_for_ui(&lines.join("\n"), 30_000)
}

fn format_memory_report(transcript: &TranscriptStore, scratchpad: &Scratchpad) -> String {
    let transcript_blob = transcript_text(transcript);
    let pending = extract_pending_work(&transcript_blob, 5);
    let key_files = extract_key_files(&transcript_blob, 8);
    let recent_actions = scratchpad
        .entries()
        .iter()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|entry| match &entry.tool_name {
            Some(tool_name) => format!("[{}] {}: {}", entry.step_type, tool_name, entry.summary),
            None => format!("[{}] {}", entry.step_type, entry.summary),
        })
        .collect::<Vec<_>>();
    let recent_requests = transcript
        .entries
        .iter()
        .rev()
        .filter(|entry| entry.role == "user")
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|entry| entry.content.replace('\n', " "))
        .collect::<Vec<_>>();

    truncate_for_ui(
        &format!(
            "Session Memory\n  scratchpad entries: {}\n  transcript entries: {}\n\nRecent Actions\n{}\n\nRecent Requests\n{}\n\nPending Work\n{}\n\nKey Files\n{}",
            scratchpad.entries().len(),
            transcript.entries.len(),
            numbered_section(&recent_actions, "(no actions recorded)"),
            numbered_section(&recent_requests, "(no user requests recorded)"),
            numbered_section(&pending, "(none inferred yet)"),
            numbered_section(&key_files, "(none detected yet)"),
        ),
        30_000,
    )
}

fn format_files_report(transcript: &TranscriptStore, scratchpad: &Scratchpad) -> String {
    let transcript_blob = transcript_text(transcript);
    let key_files = extract_key_files(&transcript_blob, 12);
    let file_actions = scratchpad
        .entries()
        .iter()
        .rev()
        .filter(|entry| {
            matches!(
                entry.tool_name.as_deref(),
                Some("read_file" | "write_file" | "execute_bash" | "execute_python")
            )
        })
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|entry| match &entry.tool_name {
            Some(tool_name) => format!("{tool_name}: {}", entry.summary),
            None => entry.summary.clone(),
        })
        .collect::<Vec<_>>();

    truncate_for_ui(
        &format!(
            "Files In Focus\n{}\n\nRecent File-Oriented Actions\n{}",
            numbered_section(&key_files, "(none detected yet)"),
            numbered_section(&file_actions, "(no recent file-oriented actions)")
        ),
        30_000,
    )
}

fn format_tasks_report(transcript: &TranscriptStore, scratchpad: &Scratchpad) -> String {
    let transcript_blob = transcript_text(transcript);
    let pending = extract_pending_work(&transcript_blob, 10);
    let recent_decisions = scratchpad
        .entries()
        .iter()
        .rev()
        .filter(|entry| entry.step_type == "decision")
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|entry| entry.summary.clone())
        .collect::<Vec<_>>();
    let recent_requests = transcript
        .entries
        .iter()
        .rev()
        .filter(|entry| entry.role == "user")
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|entry| entry.content.replace('\n', " "))
        .collect::<Vec<_>>();

    truncate_for_ui(
        &format!(
            "Pending Tasks\n{}\n\nRecent Requests\n{}\n\nRecent Decisions\n{}",
            numbered_section(&pending, "(none inferred yet)"),
            numbered_section(&recent_requests, "(no user requests recorded)"),
            numbered_section(&recent_decisions, "(no decisions recorded yet)")
        ),
        30_000,
    )
}

fn format_plan_report(transcript: &TranscriptStore, scratchpad: &Scratchpad) -> String {
    let transcript_blob = transcript_text(transcript);
    let mut pending = extract_pending_work(&transcript_blob, 6);
    if pending.is_empty() {
        pending = transcript
            .entries
            .iter()
            .rev()
            .filter(|entry| entry.role == "user")
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|entry| format!("Follow up on: {}", entry.content.replace('\n', " ")))
            .collect::<Vec<_>>();
    }
    let decisions = scratchpad
        .entries()
        .iter()
        .rev()
        .filter(|entry| entry.step_type == "decision")
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|entry| entry.summary.clone())
        .collect::<Vec<_>>();
    let key_files = extract_key_files(&transcript_blob, 8);

    truncate_for_ui(
        &format!(
            "Current Plan\n{}\n\nRecent Decisions\n{}\n\nKey Files\n{}",
            numbered_section(&pending, "(no explicit plan items inferred yet)"),
            numbered_section(&decisions, "(no decisions recorded yet)"),
            numbered_section(&key_files, "(none detected yet)"),
        ),
        30_000,
    )
}

fn plan_snapshot_path(slash_ctx: &SlashCommandContext, session_id: &str) -> PathBuf {
    slash_ctx
        .project_root
        .join(".prism")
        .join("plans")
        .join(format!("{session_id}.md"))
}

// Persist plan-mode output into the project so the workflow has a durable
// artifact even if the live TUI session ends or the conversation is resumed.
fn persist_plan_snapshot(
    slash_ctx: &SlashCommandContext,
    session_id: &str,
    body: &str,
) -> Result<PathBuf> {
    let path = plan_snapshot_path(slash_ctx, session_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("failed to create plan snapshot directory")?;
    }
    fs::write(&path, body).context("failed to write plan snapshot")?;
    Ok(path)
}

fn load_plan_snapshot(slash_ctx: &SlashCommandContext, session_id: &str) -> Option<String> {
    fs::read_to_string(plan_snapshot_path(slash_ctx, session_id)).ok()
}

fn format_doctor_report(
    slash_ctx: &SlashCommandContext,
    llm_config: &LlmConfig,
    transcript: &TranscriptStore,
    tools: &ToolCatalog,
    session_mode: SessionMode,
) -> String {
    let warning = transcript
        .budget_warning()
        .unwrap_or_else(|| "none".to_string());

    truncate_for_ui(
        &format!(
            "Doctor\n  model: {}\n  session mode: {}\n  tool count: {}\n  project root: {}\n  python: {}\n  budget warning: {}\n  transcript entries: {}\n\nIf a command behaves unexpectedly, check:\n  1. active session mode and permissions\n  2. current model selection\n  3. python tool server availability\n  4. project root and working directory assumptions",
            llm_config.model,
            session_mode.as_str(),
            tools.len(),
            slash_ctx.project_root.display(),
            slash_ctx.python_bin.display(),
            warning,
            transcript.entries.len(),
        ),
        30_000,
    )
}

fn format_current_session_report(
    session_store: &SessionStore,
    llm_config: &LlmConfig,
    transcript: &TranscriptStore,
) -> String {
    match session_store.meta() {
        Some(meta) => format!(
            "Current Session\n  id: {}\n  model: {}\n  turns: {}\n  compactions: {}\n  transcript entries: {}",
            meta.session_id,
            meta.model,
            meta.turn_count,
            meta.compaction_count,
            transcript.entries.len(),
        ),
        None => format!(
            "Current Session\n  id: {}\n  model: {}\n  transcript entries: {}",
            session_store
                .current_id()
                .unwrap_or(transcript.session_id.as_str()),
            llm_config.model,
            transcript.entries.len(),
        ),
    }
}

fn format_workflow_arguments(spec: &WorkflowSpec) -> String {
    if spec.arguments.is_empty() {
        return "(no arguments)".to_string();
    }

    spec.arguments
        .iter()
        .map(|argument| {
            let required = if argument.required {
                "required"
            } else {
                "optional"
            };
            let flag = if argument.is_flag { " flag" } else { "" };
            let env_hint = if argument.env.is_empty() {
                String::new()
            } else {
                format!(" env={}", argument.env)
            };
            format!(
                "--{} [{}{}] {}{}",
                argument.name, argument.r#type, flag, required, env_hint
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_workflow_steps(spec: &WorkflowSpec) -> String {
    if spec.steps.is_empty() {
        return "(no steps)".to_string();
    }

    spec.steps
        .iter()
        .map(|step| {
            let keys = step.config.keys().cloned().collect::<Vec<_>>();
            let config_summary = if keys.is_empty() {
                String::new()
            } else {
                format!(" ({})", keys.join(", "))
            };
            format!("{}  {}{}", step.id, step.action, config_summary)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_workflow_context_preview(context: &BTreeMap<String, serde_json::Value>) -> String {
    if context.is_empty() {
        return "(empty context)".to_string();
    }

    context
        .iter()
        .take(12)
        .map(|(key, value)| {
            let preview = match value {
                serde_json::Value::String(text) => preview_text(text, 80),
                _ => preview_text(&value.to_string(), 80),
            };
            format!("{key}: {preview}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_workflow_result_steps(result: &WorkflowRunResult) -> String {
    if result.steps.is_empty() {
        return "(no steps recorded)".to_string();
    }

    result
        .steps
        .iter()
        .map(|step| {
            format!(
                "{}  {}  {}  {}",
                step.id, step.action, step.status, step.summary
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn emit_workflow_list_view(
    specs: &BTreeMap<String, WorkflowSpec>,
    slash_ctx: &SlashCommandContext,
) {
    if specs.is_empty() {
        emit_view(
            "workflow",
            "Workflows",
            "No workflows found in builtin, project, or user workflow directories.",
            "info",
        );
        return;
    }

    let list_body = specs
        .values()
        .map(|spec| {
            format!(
                "{}  /{}  {}",
                spec.name, spec.command_name, spec.description
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let search_paths = prism_workflows::workflow_search_paths(Some(&slash_ctx.project_root))
        .into_iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("\n");

    emit_tabbed_view(
        "workflow",
        "Workflows",
        &[
            ("list", "List", &list_body, "info"),
            ("paths", "Paths", &search_paths, "accent"),
        ],
        "list",
        "info",
        "yaml workflows • rust discovery • policy-aware execution",
    );
}

fn emit_workflow_spec_view(spec: &WorkflowSpec) {
    let summary = format!(
        "{}\n\ncommand: /{}\ndefault mode: {}\nsource: {}",
        spec.description, spec.command_name, spec.default_mode, spec.source_path
    );
    let arguments = format_workflow_arguments(spec);
    let steps = format_workflow_steps(spec);

    emit_tabbed_view(
        "workflow",
        &format!("Workflow · {}", spec.name),
        &[
            ("summary", "Summary", &summary, "info"),
            ("args", "Args", &arguments, "accent"),
            ("steps", "Steps", &steps, "info"),
        ],
        "summary",
        "info",
        "workflow manifest",
    );
}

fn emit_workflow_result_view(spec: &WorkflowSpec, result: &WorkflowRunResult) {
    let summary = format!(
        "{}\n\nworkflow: {}\nmode: {}\nsteps: {}",
        spec.description,
        result.workflow,
        result.mode,
        result.steps.len()
    );
    let steps = format_workflow_result_steps(result);
    let context = format_workflow_context_preview(&result.context);

    emit_tabbed_view(
        "workflow",
        &format!("Workflow Run · {}", spec.name),
        &[
            ("summary", "Summary", &summary, "info"),
            ("steps", "Steps", &steps, "accent"),
            ("context", "Context", &context, "info"),
        ],
        "steps",
        "accent",
        "policy-aware workflow result",
    );
}

async fn handle_workflow_slash_command(
    args: &[String],
    slash_ctx: &SlashCommandContext,
    policy_engine: &mut Option<prism_policy::PolicyEngine>,
) -> Result<bool> {
    if args.first().map(String::as_str) != Some("workflow") {
        return Ok(false);
    }

    let specs = discover_workflows(Some(&slash_ctx.project_root))?;
    let action = args.get(1).map(String::as_str).unwrap_or("list");
    let interactive_role = interactive_policy_role();

    match action {
        "list" => {
            emit_workflow_list_view(&specs, slash_ctx);
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "show" => {
            let Some(name) = args.get(2) else {
                emit_notification(
                    "ui.text.delta",
                    serde_json::json!({ "text": "Usage: /workflow show <name>" }),
                );
                emit_notification("ui.turn.complete", serde_json::json!({}));
                return Ok(true);
            };

            let spec = find_workflow(&specs, name)
                .ok_or_else(|| anyhow::anyhow!("Workflow not found: {name}"))?;
            emit_workflow_spec_view(spec);
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "run" => {
            let Some(name) = args.get(2) else {
                emit_notification(
                    "ui.text.delta",
                    serde_json::json!({
                        "text": "Usage: /workflow run <name> [--set key=value] [--execute]"
                    }),
                );
                emit_notification("ui.turn.complete", serde_json::json!({}));
                return Ok(true);
            };

            let spec = find_workflow(&specs, name)
                .ok_or_else(|| anyhow::anyhow!("Workflow not found: {name}"))?;
            let mut values = BTreeMap::new();
            let mut execute = false;
            let mut index = 3;

            while index < args.len() {
                match args[index].as_str() {
                    "--execute" => {
                        execute = true;
                        index += 1;
                    }
                    "--set" => {
                        let Some(pair) = args.get(index + 1) else {
                            anyhow::bail!("workflow --set requires key=value");
                        };
                        let (key, value) = pair.split_once('=').ok_or_else(|| {
                            anyhow::anyhow!("invalid --set value: {pair}. Expected key=value.")
                        })?;
                        values.insert(key.to_string(), value.to_string());
                        index += 2;
                    }
                    other => {
                        anyhow::bail!(
                            "unexpected workflow argument: {other}. Use `--set key=value` or `--execute`."
                        );
                    }
                }
            }

            let result = execute_workflow_with_policy(
                spec,
                &values,
                execute,
                policy_engine.as_mut(),
                Some(interactive_role.as_str()),
            )
            .await?;
            emit_workflow_result_view(spec, &result);
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        // Support `/workflow <name> ...` as a direct alias form so workflows
        // discovered from YAML feel native in the REPL as well.
        _ => {
            let request = parse_workflow_command_args(&args[1..])?;
            let spec = find_workflow(&specs, &request.name)
                .ok_or_else(|| anyhow::anyhow!("Workflow not found: {}", request.name))?;
            let result = execute_workflow_with_policy(
                spec,
                &request.values,
                request.execute,
                policy_engine.as_mut(),
                Some(interactive_role.as_str()),
            )
            .await?;
            emit_workflow_result_view(spec, &result);
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
    }
}

/// Map an [`AgentEvent`] to the `ui.*` JSON-RPC notifications that the Ink
/// frontend expects.  Event names and schemas MUST match
/// `frontend/src/bridge/types.ts` → `UI_EVENT_MAP`.
fn emit_agent_event(event: AgentEvent) {
    match event {
        AgentEvent::TextDelta { text } => {
            emit_notification("ui.text.delta", serde_json::json!({ "text": text }));
        }
        AgentEvent::ToolCallStart {
            tool_name,
            call_id,
            preview,
        } => {
            // Flush any buffered text before a tool starts
            emit_notification("ui.text.flush", serde_json::json!({ "text": "" }));
            emit_notification(
                "ui.tool.start",
                serde_json::json!({
                    "tool_name": tool_name,
                    "call_id": call_id,
                    "verb": format!("Running {tool_name}"),
                    "preview": preview,
                }),
            );
        }
        AgentEvent::ToolCallResult {
            call_id,
            tool_name,
            content,
            summary,
            preview,
            elapsed_ms,
            is_error,
        } => {
            let (display_content, extra_data) = build_tool_card_payload(
                &tool_name,
                &content,
                preview.as_deref(),
                summary.as_deref(),
            );
            let mut data = serde_json::Map::new();
            data.insert("call_id".to_string(), serde_json::json!(call_id));
            if let Some(summary) = summary {
                data.insert("summary".to_string(), serde_json::json!(summary));
            }
            if let Some(preview) = preview {
                data.insert("preview".to_string(), serde_json::json!(preview));
            }
            if let Some(extra) = extra_data.as_object() {
                for (key, value) in extra {
                    data.insert(key.clone(), value.clone());
                }
            }
            // Frontend expects "ui.card" with UiCard schema
            emit_notification(
                "ui.card",
                serde_json::json!({
                    "card_type": if is_error { "error" } else { "results" },
                    "tool_name": tool_name,
                    "elapsed_ms": elapsed_ms,
                    "content": display_content,
                    "data": data,
                }),
            );
        }
        AgentEvent::ToolApprovalRequest {
            tool_name,
            call_id: _,
            tool_args,
            tool_description,
            requires_approval,
            permission_mode,
        } => {
            // Frontend expects "ui.prompt" with UiPrompt schema
            emit_notification(
                "ui.prompt",
                serde_json::json!({
                    "prompt_type": "approval",
                    "message": format!("Allow {}?", tool_name),
                    "choices": ["y", "n", "a", "b"],
                    "tool_name": tool_name,
                    "tool_args": tool_args,
                    "tool_description": tool_description,
                    "requires_approval": requires_approval,
                    "permission_mode": permission_mode,
                }),
            );
        }
        AgentEvent::TurnComplete {
            text: _,
            has_more: _,
            usage,
            total_usage: _,
            estimated_cost,
        } => {
            // Emit ui.cost before ui.turn.complete (frontend expects both)
            let (input_tokens, output_tokens) = usage
                .as_ref()
                .map(|u| (u.input_tokens, u.output_tokens))
                .unwrap_or((0, 0));
            emit_notification(
                "ui.cost",
                serde_json::json!({
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens,
                    "turn_cost": estimated_cost.unwrap_or(0.0),
                    "session_cost": estimated_cost.unwrap_or(0.0),
                }),
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
        }
    }
}

fn build_tool_card_payload(
    tool_name: &str,
    content: &str,
    preview: Option<&str>,
    summary: Option<&str>,
) -> (String, Value) {
    let parsed = serde_json::from_str::<Value>(content).ok();
    match tool_name {
        "read_file" => {
            if let Some(object) = parsed.as_ref().and_then(|value| value.as_object()) {
                let path = object
                    .get("path")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let body = object
                    .get("content")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let size_bytes = object.get("size_bytes").and_then(|value| value.as_u64());

                let mut sections = Vec::new();
                if let Some(preview) = preview {
                    sections.push(preview.to_string());
                }
                if let Some(summary) = summary {
                    if Some(summary) != preview {
                        sections.push(summary.to_string());
                    }
                }
                if !path.is_empty() {
                    sections.push(format!("path: {path}"));
                }
                if let Some(size_bytes) = size_bytes {
                    sections.push(format!("size: {size_bytes} bytes"));
                }
                if !body.trim().is_empty() {
                    sections.push(format!(
                        "content\n{}",
                        truncate_for_ui(body.trim_end(), 20_000)
                    ));
                }

                return (
                    sections.join("\n\n"),
                    serde_json::json!({
                        "path": path,
                        "size_bytes": size_bytes,
                    }),
                );
            }
        }
        "edit_file" => {
            if let Some(object) = parsed.as_ref().and_then(|value| value.as_object()) {
                let path = object
                    .get("path")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let success = object
                    .get("success")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                let size_bytes = object.get("size_bytes").and_then(|value| value.as_u64());
                let replacements = object.get("replacements").and_then(|value| value.as_u64());

                let mut sections = Vec::new();
                if let Some(preview) = preview {
                    sections.push(preview.to_string());
                }
                if let Some(summary) = summary {
                    if Some(summary) != preview {
                        sections.push(summary.to_string());
                    }
                }
                if !path.is_empty() {
                    sections.push(format!("path: {path}"));
                }
                sections.push(format!(
                    "status: {}",
                    if success { "edited" } else { "failed" }
                ));
                if let Some(replacements) = replacements {
                    sections.push(format!("replacements: {replacements}"));
                }
                if let Some(size_bytes) = size_bytes {
                    sections.push(format!("size: {size_bytes} bytes"));
                }

                return (
                    sections.join("\n\n"),
                    serde_json::json!({
                        "path": path,
                        "size_bytes": size_bytes,
                        "replacements": replacements,
                        "success": success,
                    }),
                );
            }
        }
        "write_file" => {
            if let Some(object) = parsed.as_ref().and_then(|value| value.as_object()) {
                let path = object
                    .get("path")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let success = object
                    .get("success")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                let size_bytes = object.get("size_bytes").and_then(|value| value.as_u64());

                let mut sections = Vec::new();
                if let Some(preview) = preview {
                    sections.push(preview.to_string());
                }
                if let Some(summary) = summary {
                    if Some(summary) != preview {
                        sections.push(summary.to_string());
                    }
                }
                if !path.is_empty() {
                    sections.push(format!("path: {path}"));
                }
                sections.push(format!(
                    "status: {}",
                    if success { "written" } else { "failed" }
                ));
                if let Some(size_bytes) = size_bytes {
                    sections.push(format!("size: {size_bytes} bytes"));
                }

                return (
                    sections.join("\n\n"),
                    serde_json::json!({
                        "path": path,
                        "size_bytes": size_bytes,
                        "success": success,
                    }),
                );
            }
        }
        _ if command_tools::is_command_tool(tool_name) => {
            if let Some(object) = parsed.as_ref().and_then(|value| value.as_object()) {
                let root = object
                    .get("root")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let invocation = object
                    .get("invocation")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let stdout = object
                    .get("stdout")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let stderr = object
                    .get("stderr")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let exit_code = object.get("exit_code").and_then(|value| value.as_i64());
                let timed_out = object
                    .get("timed_out")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                let success = object
                    .get("success")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                let stdout_json = serde_json::from_str::<Value>(stdout.trim()).ok();

                let mut sections = Vec::new();
                if let Some(preview) = preview {
                    sections.push(preview.to_string());
                }
                if let Some(summary) = summary {
                    if Some(summary) != preview {
                        sections.push(summary.to_string());
                    }
                }
                if !invocation.is_empty() {
                    sections.push(format!("command: {invocation}"));
                }
                sections.push(format!(
                    "status: {}",
                    if success {
                        "completed"
                    } else {
                        "returned non-zero"
                    }
                ));
                if let Some(exit_code) = exit_code {
                    sections.push(format!("exit code: {exit_code}"));
                }
                if timed_out {
                    sections.push("timed out".to_string());
                }

                if matches!(root.as_str(), "models" | "deploy" | "discourse") {
                    let mut data = serde_json::json!({
                        "root": root,
                        "invocation": invocation,
                        "exit_code": exit_code,
                        "timed_out": timed_out,
                        "success": success,
                    });
                    if let Some(parsed_stdout) = stdout_json {
                        if let Some(data_object) = data.as_object_mut() {
                            data_object.insert("parsed_stdout".to_string(), parsed_stdout);
                        }
                    } else if !stdout.trim().is_empty() {
                        sections.push(format!("stdout\n{}", stdout.trim_end()));
                    }
                    if !stderr.trim().is_empty() {
                        if let Some(data_object) = data.as_object_mut() {
                            data_object.insert("stderr".to_string(), json!(stderr));
                        }
                    }
                    return (sections.join("\n\n"), data);
                }

                if !stdout.trim().is_empty() {
                    sections.push(format!("stdout\n{}", stdout.trim_end()));
                }
                if !stderr.trim().is_empty() {
                    sections.push(format!("stderr\n{}", stderr.trim_end()));
                }

                return (
                    sections.join("\n\n"),
                    serde_json::json!({
                        "root": root,
                        "invocation": invocation,
                        "stdout": stdout,
                        "stderr": stderr,
                        "exit_code": exit_code,
                        "timed_out": timed_out,
                        "success": success,
                    }),
                );
            }
        }
        "execute_bash" | "execute_python" => {
            if let Some(object) = parsed.as_ref().and_then(|value| value.as_object()) {
                if let Some(task) = object.get("task").and_then(|value| value.as_object()) {
                    let task_id = task
                        .get("task_id")
                        .and_then(|value| value.as_str())
                        .unwrap_or("unknown");
                    let status = task
                        .get("status")
                        .and_then(|value| value.as_str())
                        .unwrap_or("unknown");
                    let cwd = task
                        .get("cwd")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default();
                    let stdout_path = task
                        .get("stdout_path")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default();
                    let stderr_path = task
                        .get("stderr_path")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default();
                    let mut sections = Vec::new();
                    if let Some(preview) = preview {
                        sections.push(preview.to_string());
                    }
                    if let Some(summary) = summary {
                        if Some(summary) != preview {
                            sections.push(summary.to_string());
                        }
                    }
                    sections.push(format!("task: {task_id}"));
                    sections.push(format!("status: {status}"));
                    if !cwd.is_empty() {
                        sections.push(format!("cwd: {cwd}"));
                    }
                    if !stdout_path.is_empty() {
                        sections.push(format!("stdout log: {stdout_path}"));
                    }
                    if !stderr_path.is_empty() {
                        sections.push(format!("stderr log: {stderr_path}"));
                    }
                    return (
                        sections.join("\n"),
                        serde_json::json!({
                            "task_id": task_id,
                            "status": status,
                            "cwd": cwd,
                            "stdout_path": stdout_path,
                            "stderr_path": stderr_path,
                            "backgrounded": true,
                        }),
                    );
                }

                let stdout = object
                    .get("stdout")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let stderr = object
                    .get("stderr")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let error = object
                    .get("error")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let exit_code = object.get("exit_code").and_then(|value| value.as_i64());
                let cwd = object
                    .get("cwd")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string());
                let interpretation = object
                    .get("return_code_interpretation")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string());
                let timed_out = object
                    .get("timed_out")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);

                // Keep execution cards readable even when the raw tool result is
                // structured JSON. The frontend still gets the parsed fields in
                // `data`, but the fallback content stays human-friendly.
                let mut sections = Vec::new();
                if let Some(preview) = preview {
                    sections.push(preview.to_string());
                }
                if let Some(summary) = summary {
                    if Some(summary) != preview {
                        sections.push(summary.to_string());
                    }
                }
                if let Some(exit_code) = exit_code {
                    sections.push(format!("exit code: {exit_code}"));
                }
                if let Some(interpretation) = interpretation.as_deref() {
                    sections.push(interpretation.to_string());
                }
                if timed_out {
                    sections.push("timed out".to_string());
                }
                if let Some(cwd) = cwd.as_deref() {
                    sections.push(format!("cwd: {cwd}"));
                }
                if !stdout.trim().is_empty() {
                    sections.push(format!("stdout\n{}", stdout.trim_end()));
                }
                let stderr_block = if !stderr.trim().is_empty() {
                    stderr.trim_end()
                } else {
                    error.trim_end()
                };
                if !stderr_block.is_empty() {
                    sections.push(format!("stderr\n{stderr_block}"));
                }

                return (
                    sections.join("\n\n"),
                    serde_json::json!({
                        "stdout": stdout,
                        "stderr": stderr,
                        "error": error,
                        "exit_code": exit_code,
                        "cwd": cwd,
                        "return_code_interpretation": interpretation,
                        "timed_out": timed_out,
                    }),
                );
            }
        }
        "read_bash_task" | "stop_bash_task" => {
            if let Some(task) = parsed
                .as_ref()
                .and_then(|value| value.get("task"))
                .and_then(|value| value.as_object())
            {
                let task_id = task
                    .get("task_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown");
                let status = task
                    .get("status")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown");
                let command = task
                    .get("command")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let stdout_tail = task
                    .get("stdout_tail")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let stderr_tail = task
                    .get("stderr_tail")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let mut sections = Vec::new();
                if let Some(summary) = summary {
                    sections.push(summary.to_string());
                }
                sections.push(format!("task: {task_id}"));
                sections.push(format!("status: {status}"));
                if !command.is_empty() {
                    sections.push(format!("command: {command}"));
                }
                if !stdout_tail.trim().is_empty() {
                    sections.push(format!("stdout\n{}", stdout_tail.trim_end()));
                }
                if !stderr_tail.trim().is_empty() {
                    sections.push(format!("stderr\n{}", stderr_tail.trim_end()));
                }
                return (
                    sections.join("\n\n"),
                    serde_json::json!({
                        "task_id": task_id,
                        "status": status,
                        "command": command,
                        "stdout_tail": stdout_tail,
                        "stderr_tail": stderr_tail,
                    }),
                );
            }
        }
        "list_bash_tasks" => {
            if let Some(tasks) = parsed
                .as_ref()
                .and_then(|value| value.get("tasks"))
                .and_then(|value| value.as_array())
            {
                let lines = tasks
                    .iter()
                    .filter_map(|task| {
                        let task_id = task.get("task_id")?.as_str()?;
                        let status = task.get("status")?.as_str().unwrap_or("unknown");
                        let command = task.get("command")?.as_str().unwrap_or("");
                        Some(format!("{task_id} · {status} · {command}"))
                    })
                    .collect::<Vec<_>>();
                return (
                    if lines.is_empty() {
                        "No background bash tasks.".to_string()
                    } else {
                        lines.join("\n")
                    },
                    serde_json::json!({ "task_count": lines.len() }),
                );
            }
        }
        _ => {}
    }

    (content.to_string(), Value::Object(Default::default()))
}

fn spawn_agent_turn(
    mut runtime: ServerRuntime,
    user_text: String,
    tools: Arc<ToolCatalog>,
    config: Arc<AgentConfig>,
    hooks: Arc<HookRegistry>,
    slash_ctx: SlashCommandContext,
    approval_rx: agent_loop::SharedApprovalReceiver,
    live_permission_overrides: SharedPermissionOverrides,
) -> oneshot::Receiver<ServerRuntime> {
    let (result_tx, result_rx) = oneshot::channel();

    tokio::spawn(async move {
        let llm = LlmClient::new(runtime.llm_config.clone());
        let mut turn_config = config.as_ref().clone();
        turn_config.system_prompt = system_prompt_for_mode(
            runtime.session_mode,
            &config.system_prompt,
            &runtime.plan_state,
            tools.as_ref(),
        );

        let turn_result = agent_loop::run_turn(
            &llm,
            &mut runtime.tool_server,
            &runtime.command_tool_runtime,
            &mut runtime.history,
            tools.as_ref(),
            &turn_config,
            &user_text,
            &mut runtime.transcript,
            hooks.as_ref(),
            &runtime.permissions,
            Some(live_permission_overrides),
            &mut runtime.scratchpad,
            &mut |event| {
                match &event {
                    AgentEvent::TurnComplete {
                        text: Some(text), ..
                    } if !text.is_empty() => {
                        runtime
                            .session_store
                            .append_message("assistant", text, "", "", None);
                    }
                    AgentEvent::ToolCallResult {
                        call_id,
                        tool_name,
                        content,
                        ..
                    } => {
                        runtime
                            .session_store
                            .append_message("tool", content, tool_name, call_id, None);
                    }
                    _ => {}
                }
                emit_agent_event(event);
            },
            Some(approval_rx),
            runtime.policy_engine.as_mut(),
        )
        .await;

        match turn_result {
            Ok(()) => {
                if runtime.session_mode == SessionMode::Plan {
                    // Planning turns keep refreshing the durable plan artifact
                    // so resume/accept always point at the latest draft.
                    let plan_body = format_plan_report(&runtime.transcript, &runtime.scratchpad);
                    runtime.plan_state.status = Some(PlanStatus::Draft);
                    if let Some(session_id) = runtime.session_store.current_id() {
                        if let Err(error) =
                            persist_plan_snapshot(&slash_ctx, session_id, &plan_body)
                        {
                            tracing::warn!(error = %error, "failed to persist plan snapshot");
                        }
                    }
                }
                persist_runtime_state(
                    &runtime.session_store,
                    runtime.session_mode,
                    &runtime.permission_overrides,
                    &runtime.plan_state,
                );
                emit_status_snapshot(
                    config.auto_approve,
                    &runtime.transcript,
                    runtime.session_mode,
                    &runtime.plan_state,
                );
            }
            Err(error) => {
                tracing::error!(error = %error, "agent turn failed");
                emit_notification(
                    "ui.text.delta",
                    serde_json::json!({ "text": format!("Error: {error}") }),
                );
                emit_notification("ui.turn.complete", serde_json::json!({}));
            }
        }

        let _ = result_tx.send(runtime);
    });

    result_rx
}

// ── Command handlers ──────────────────────────────────────────────

/// Handle built-in slash commands. Returns `true` if the command was handled.
async fn handle_command(
    command: &str,
    slash_ctx: &SlashCommandContext,
    config: &AgentConfig,
    tool_server: &mut ToolServerHandle,
    session_store: &mut SessionStore,
    history: &mut Vec<ChatMessage>,
    llm_config: &mut LlmConfig,
    transcript: &mut TranscriptStore,
    permissions: &mut ToolPermissionContext,
    permission_overrides: &mut PermissionOverrides,
    scratchpad: &mut Scratchpad,
    tools: &ToolCatalog,
    session_mode: &mut SessionMode,
    plan_state: &mut PlanRuntimeState,
    policy_engine: &mut Option<prism_policy::PolicyEngine>,
) -> Result<bool> {
    let trimmed = command.trim();

    match trimmed {
        "/tools" => {
            // Reuse the live tool catalog here so the user sees the same
            // approval/access facts that the runtime uses for actual calls.
            let (read_only, workspace_write, full_access, approval_required, _all) =
                loaded_tools_by_access(tools);
            let auto_approved = tools
                .iter()
                .filter(|tool| permissions.auto_approves(&tool.name))
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>();
            let blocked = tools
                .iter()
                .filter(|tool| permissions.blocks(&tool.name))
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>();
            let summary = format_tools_summary_report(tools, permissions);
            let approval_body = numbered_section(
                &approval_required
                    .iter()
                    .map(|tool_name| format_tool_entry(tool_name, tools))
                    .collect::<Vec<_>>(),
                "(none)",
            );
            let active_body = format!(
                "Auto-approved now\n{}\n\nBlocked now\n{}",
                numbered_section(&auto_approved, "(none)"),
                numbered_section(&blocked, "(none)"),
            );
            let read_only_body = numbered_section(
                &read_only
                    .iter()
                    .map(|tool_name| format_tool_entry(tool_name, tools))
                    .collect::<Vec<_>>(),
                "(none)",
            );
            let workspace_write_body = numbered_section(
                &workspace_write
                    .iter()
                    .map(|tool_name| format_tool_entry(tool_name, tools))
                    .collect::<Vec<_>>(),
                "(none)",
            );
            let full_access_body = numbered_section(
                &full_access
                    .iter()
                    .map(|tool_name| format_tool_entry(tool_name, tools))
                    .collect::<Vec<_>>(),
                "(none)",
            );
            emit_tabbed_view(
                "tools",
                "Tools",
                &[
                    ("summary", "Summary", &summary, "info"),
                    ("approval", "Approval", &approval_body, "warning"),
                    ("active", "Active", &active_body, "accent"),
                    ("read-only", "Read", &read_only_body, "info"),
                    ("workspace-write", "Write", &workspace_write_body, "accent"),
                    ("full-access", "Full", &full_access_body, "warning"),
                ],
                "summary",
                "info",
                "Use `/permissions allow|deny|ask <tool>` to adjust this session.",
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "/clear" => {
            history.clear();
            let budget = transcript.budget.clone();
            *transcript = TranscriptStore::new(Some(budget));
            *scratchpad = Scratchpad::new();
            *session_mode = SessionMode::Chat;
            *plan_state = PlanRuntimeState::default();
            permission_overrides.reset();
            *permissions =
                build_effective_permission_context(*session_mode, tools, permission_overrides);
            let new_session_id = session_store.new_session(&llm_config.model);
            persist_runtime_state(
                session_store,
                *session_mode,
                permission_overrides,
                plan_state,
            );
            emit_status_snapshot(config.auto_approve, transcript, *session_mode, plan_state);
            emit_notification(
                "ui.text.delta",
                serde_json::json!({
                    "text": format!("Conversation cleared. Started session {new_session_id}.")
                }),
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "/help" => {
            let help_text = builtin_help_text();
            emit_view("help", "Commands", &help_text, "info");
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        _ if trimmed.starts_with("/read") => {
            let path = parse_read_slash_path(trimmed)?;
            execute_manual_tool_call(
                &format!("/read {path}"),
                "read_file",
                serde_json::json!({ "path": path }),
                tool_server,
                session_store,
                transcript,
                permissions,
                policy_engine,
            )
            .await?;
            Ok(true)
        }
        _ if trimmed.starts_with("/edit") => {
            match parse_edit_slash_action(trimmed)? {
                EditSlashAction::Edit {
                    path,
                    old_text,
                    new_text,
                    replace_all,
                } => {
                    execute_manual_tool_call(
                        &format!("/edit {path}"),
                        "edit_file",
                        serde_json::json!({
                            "path": path,
                            "old_text": old_text,
                            "new_text": new_text,
                            "replace_all": replace_all,
                        }),
                        tool_server,
                        session_store,
                        transcript,
                        permissions,
                        policy_engine,
                    )
                    .await?;
                }
            }
            Ok(true)
        }
        _ if trimmed.starts_with("/diff") => {
            let diff_command = match parse_diff_slash_action(trimmed)? {
                DiffSlashAction::Repo => "git diff -- .".to_string(),
                DiffSlashAction::Paths { paths } => {
                    format!("git diff -- {}", shell_command_join(&paths))
                }
            };
            execute_manual_tool_call(
                trimmed,
                "execute_bash",
                serde_json::json!({
                    "command": diff_command,
                    "description": "Show git diff for the requested scope",
                }),
                tool_server,
                session_store,
                transcript,
                permissions,
                policy_engine,
            )
            .await?;
            Ok(true)
        }
        _ if trimmed.starts_with("/write") => {
            match parse_write_slash_action(trimmed)? {
                WriteSlashAction::Write { path, content } => {
                    execute_manual_tool_call(
                        &format!("/write {path}"),
                        "write_file",
                        serde_json::json!({ "path": path, "content": content }),
                        tool_server,
                        session_store,
                        transcript,
                        permissions,
                        policy_engine,
                    )
                    .await?;
                }
            }
            Ok(true)
        }
        _ if trimmed.starts_with("/python") => {
            match parse_python_slash_action(trimmed)? {
                PythonSlashAction::Execute {
                    code,
                    description,
                    timeout,
                } => {
                    let label = if let Some(description) = description.as_deref() {
                        format!(
                            "/python --description {:?} -- {}",
                            description,
                            code.lines().next().unwrap_or("")
                        )
                    } else {
                        format!("/python {}", code.lines().next().unwrap_or(""))
                    };
                    let mut args = serde_json::json!({ "code": code });
                    if let Some(description) = description {
                        args["description"] = serde_json::json!(description);
                    }
                    if let Some(timeout) = timeout {
                        args["timeout"] = serde_json::json!(timeout);
                    }
                    execute_manual_tool_call(
                        &label,
                        "execute_python",
                        args,
                        tool_server,
                        session_store,
                        transcript,
                        permissions,
                        policy_engine,
                    )
                    .await?;
                }
            }
            Ok(true)
        }
        _ if trimmed.starts_with("/bash") => {
            match parse_bash_slash_action(trimmed)? {
                BashSlashAction::Tasks => {
                    execute_manual_tool_call(
                        "/bash tasks",
                        "list_bash_tasks",
                        serde_json::json!({}),
                        tool_server,
                        session_store,
                        transcript,
                        permissions,
                        policy_engine,
                    )
                    .await?;
                }
                BashSlashAction::Read { task_id } => {
                    execute_manual_tool_call(
                        &format!("/bash read {task_id}"),
                        "read_bash_task",
                        serde_json::json!({ "task_id": task_id }),
                        tool_server,
                        session_store,
                        transcript,
                        permissions,
                        policy_engine,
                    )
                    .await?;
                }
                BashSlashAction::Stop { task_id } => {
                    execute_manual_tool_call(
                        &format!("/bash stop {task_id}"),
                        "stop_bash_task",
                        serde_json::json!({ "task_id": task_id }),
                        tool_server,
                        session_store,
                        transcript,
                        permissions,
                        policy_engine,
                    )
                    .await?;
                }
                BashSlashAction::Execute {
                    command,
                    description,
                    timeout,
                    run_in_background,
                } => {
                    let label = format!("/bash {command}");
                    let mut args = serde_json::json!({
                        "command": command,
                        "run_in_background": run_in_background,
                    });
                    if let Some(description) = description {
                        args["description"] = serde_json::json!(description);
                    }
                    if let Some(timeout) = timeout {
                        args["timeout"] = serde_json::json!(timeout);
                    }
                    execute_manual_tool_call(
                        &label,
                        "execute_bash",
                        args,
                        tool_server,
                        session_store,
                        transcript,
                        permissions,
                        policy_engine,
                    )
                    .await?;
                }
            }
            Ok(true)
        }
        "/setup" => {
            let paths = PrismPaths::discover()?;
            let mut state = paths.load_cli_state()?;
            state.preferred_python = Some(slash_ctx.python_bin.display().to_string());

            if let Some(creds) = state.credentials.as_ref() {
                paths.save_cli_state(&state)?;
                apply_account_env(Some(creds));
                emit_view(
                    "account",
                    "Setup Complete",
                    &format_account_result("PRISM is already configured.", creds, &[], &paths),
                    "info",
                );
            } else {
                let endpoints = PlatformEndpoints::from_env();
                let start = match start_native_device_login(&endpoints).await {
                    Ok(value) => value,
                    Err(error) => {
                        emit_view(
                            "account",
                            "Setup Failed",
                            &format!("Device login failed.\n\n{error}"),
                            "warning",
                        );
                        emit_notification("ui.turn.complete", serde_json::json!({}));
                        return Ok(true);
                    }
                };
                emit_view(
                    "account",
                    "Approve Login",
                    &format!(
                        "Open this URL in your browser and approve the device.\n\n{}\n\nCode\n  {}\n\nIf the browser did not open automatically, copy the URL above.",
                        start.verification_uri,
                        start.user_code,
                    ),
                    "accent",
                );
                let base_creds = match poll_native_device_login(&endpoints, &start).await {
                    Ok(value) => value,
                    Err(error) => {
                        emit_view(
                            "account",
                            "Setup Failed",
                            &format!("Device approval did not complete.\n\n{error}"),
                            "warning",
                        );
                        emit_notification("ui.turn.complete", serde_json::json!({}));
                        return Ok(true);
                    }
                };

                let platform =
                    PlatformClient::new(&endpoints.api_base).with_token(&base_creds.access_token);
                let profile = platform.fetch_current_user().await.ok();
                let selected = select_project_context_automatically(
                    &platform,
                    profile
                        .as_ref()
                        .and_then(|user| user.display_name.as_deref()),
                    None,
                )
                .await?;
                let creds = StoredCredentials {
                    access_token: base_creds.access_token,
                    refresh_token: base_creds.refresh_token,
                    platform_url: base_creds.platform_url,
                    user_id: profile.as_ref().map(|p| p.id.clone()),
                    display_name: profile.and_then(|p| p.display_name),
                    org_id: selected.context.org_id,
                    org_name: selected.context.org_name,
                    project_id: selected.context.project_id,
                    project_name: selected.context.project_name,
                    expires_at: base_creds.expires_at,
                };
                state.credentials = Some(creds.clone());
                paths.save_cli_state(&state)?;
                sync_sdk_credentials(&creds);
                apply_account_env(Some(&creds));
                emit_view(
                    "account",
                    "Setup Complete",
                    &format_account_result(
                        "PRISM account setup finished.",
                        &creds,
                        &selected.notes,
                        &paths,
                    ),
                    "info",
                );
            }
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "/login" => {
            let endpoints = PlatformEndpoints::from_env();
            let paths = PrismPaths::discover()?;
            let mut state = paths.load_cli_state()?;
            state.preferred_python = Some(slash_ctx.python_bin.display().to_string());

            let start = match start_native_device_login(&endpoints).await {
                Ok(value) => value,
                Err(error) => {
                    emit_view(
                        "account",
                        "Login Failed",
                        &format!("Device login failed.\n\n{error}"),
                        "warning",
                    );
                    emit_notification("ui.turn.complete", serde_json::json!({}));
                    return Ok(true);
                }
            };

            emit_view(
                "account",
                "Approve Login",
                &format!(
                    "Open this URL in your browser and approve the device.\n\n{}\n\nCode\n  {}\n\nIf the browser did not open automatically, copy the URL above.",
                    start.verification_uri,
                    start.user_code,
                ),
                "accent",
            );
            let base_creds = match poll_native_device_login(&endpoints, &start).await {
                Ok(value) => value,
                Err(error) => {
                    emit_view(
                        "account",
                        "Login Failed",
                        &format!("Device approval did not complete.\n\n{error}"),
                        "warning",
                    );
                    emit_notification("ui.turn.complete", serde_json::json!({}));
                    return Ok(true);
                }
            };

            let platform =
                PlatformClient::new(&endpoints.api_base).with_token(&base_creds.access_token);
            let profile = platform.fetch_current_user().await.ok();
            let selected = select_project_context_automatically(
                &platform,
                profile
                    .as_ref()
                    .and_then(|user| user.display_name.as_deref()),
                state.credentials.as_ref(),
            )
            .await?;

            let creds = StoredCredentials {
                access_token: base_creds.access_token,
                refresh_token: base_creds.refresh_token,
                platform_url: base_creds.platform_url,
                user_id: profile.as_ref().map(|p| p.id.clone()),
                display_name: profile.and_then(|p| p.display_name),
                org_id: selected.context.org_id,
                org_name: selected.context.org_name,
                project_id: selected.context.project_id,
                project_name: selected.context.project_name,
                expires_at: base_creds.expires_at,
            };
            state.credentials = Some(creds.clone());
            paths.save_cli_state(&state)?;
            sync_sdk_credentials(&creds);
            apply_account_env(Some(&creds));
            emit_view(
                "account",
                "Login Complete",
                &format_account_result(
                    "Stored MARC27 account credentials.",
                    &creds,
                    &selected.notes,
                    &paths,
                ),
                "info",
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "/logout" => {
            let paths = PrismPaths::discover()?;
            let mut state = paths.load_cli_state()?;
            state.credentials = None;
            paths.save_cli_state(&state)?;
            clear_sdk_credentials();
            apply_account_env(None);
            emit_view(
                "account",
                "Logged Out",
                "Removed stored MARC27 credentials from the CLI state and Python SDK cache.",
                "warning",
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "/context" => {
            let system_prompt =
                system_prompt_for_mode(*session_mode, &config.system_prompt, plan_state, tools);
            let text = format_context_report(
                slash_ctx,
                session_store,
                history,
                llm_config,
                &system_prompt,
                transcript,
                scratchpad,
                permissions,
                tools,
                plan_state,
            );
            emit_view("context", "Context", &text, "info");
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        _ if trimmed == "/permissions" || trimmed.starts_with("/permissions ") => {
            let rest = trimmed.strip_prefix("/permissions").unwrap().trim();
            if !rest.is_empty() {
                let args = parse_command_tail(rest)?;
                let action = args.first().map(String::as_str).unwrap_or("");

                if action == "reset" {
                    permission_overrides.reset();
                    *permissions = build_effective_permission_context(
                        *session_mode,
                        tools,
                        permission_overrides,
                    );
                    persist_runtime_state(
                        session_store,
                        *session_mode,
                        permission_overrides,
                        plan_state,
                    );
                    emit_notification(
                        "ui.text.delta",
                        serde_json::json!({
                            "text": "Cleared all session-local permission overrides."
                        }),
                    );
                    emit_notification("ui.turn.complete", serde_json::json!({}));
                    return Ok(true);
                }

                if matches!(action, "allow" | "deny" | "ask") {
                    let Some(raw_tool_name) = args.get(1) else {
                        emit_notification(
                            "ui.text.delta",
                            serde_json::json!({
                                "text": "Usage: /permissions <allow|deny|ask> <tool>"
                            }),
                        );
                        emit_notification("ui.turn.complete", serde_json::json!({}));
                        return Ok(true);
                    };

                    let Some(tool_name) = resolve_loaded_tool_name(raw_tool_name, tools) else {
                        emit_notification(
                            "ui.text.delta",
                            serde_json::json!({
                                "text": format!("Loaded tool not found: {}", raw_tool_name)
                            }),
                        );
                        emit_notification("ui.turn.complete", serde_json::json!({}));
                        return Ok(true);
                    };

                    // These overrides only affect the current loaded session.
                    // They are the first step toward a fuller rule editor.
                    match action {
                        "allow" => permission_overrides.allow(&tool_name),
                        "deny" => permission_overrides.deny(&tool_name),
                        "ask" => permission_overrides.clear(&tool_name),
                        _ => {}
                    }
                    *permissions = build_effective_permission_context(
                        *session_mode,
                        tools,
                        permission_overrides,
                    );
                    persist_runtime_state(
                        session_store,
                        *session_mode,
                        permission_overrides,
                        plan_state,
                    );

                    let detail = match action {
                        "allow" => format!("Session override: `{tool_name}` is now auto-approved."),
                        "deny" => format!("Session override: `{tool_name}` is now blocked."),
                        "ask" => format!(
                            "Session override cleared for `{tool_name}`. The tool is back to mode/default rules."
                        ),
                        _ => unreachable!(),
                    };
                    emit_notification("ui.text.delta", serde_json::json!({ "text": detail }));
                    emit_notification("ui.turn.complete", serde_json::json!({}));
                    return Ok(true);
                }
            }

            let (read_only, workspace_write, full_access, approval_required, tool_names) =
                loaded_tools_by_access(tools);
            let auto_approved = tool_names
                .iter()
                .filter(|name| permissions.auto_approves(name))
                .cloned()
                .collect::<Vec<_>>();
            let blocked = tool_names
                .iter()
                .filter(|name| permissions.blocks(name))
                .cloned()
                .collect::<Vec<_>>();
            let summary = format_permissions_report(permissions, permission_overrides, tools);
            let auto_body = format!(
                "Auto-approved tools\n{}",
                numbered_section(&auto_approved, "(none)")
            );
            let blocked_body = format!("Blocked tools\n{}", numbered_section(&blocked, "(none)"));
            let approval_body = format!(
                "Approval-required tools\n{}",
                numbered_section(&approval_required, "(none)")
            );
            let read_only_body = format!(
                "Read-only tools\n{}",
                numbered_section(&read_only, "(none)")
            );
            let workspace_write_body = format!(
                "Workspace-write tools\n{}",
                numbered_section(&workspace_write, "(none)")
            );
            let full_access_body = format!(
                "Full-access tools\n{}",
                numbered_section(&full_access, "(none)")
            );
            let allow_overrides = permission_overrides
                .allow_names()
                .cloned()
                .collect::<Vec<_>>();
            let deny_overrides = permission_overrides
                .deny_names()
                .cloned()
                .collect::<Vec<_>>();
            let overrides_body = format!(
                "Session overrides\n\nAllow\n{}\n\nDeny\n{}\n\nCommands\n  /permissions allow <tool>\n  /permissions deny <tool>\n  /permissions ask <tool>\n  /permissions reset",
                numbered_section(&allow_overrides, "(none)"),
                numbered_section(&deny_overrides, "(none)"),
            );
            emit_tabbed_view(
                "permissions",
                "Permissions",
                &[
                    ("summary", "Summary", &summary, "warning"),
                    ("auto", "Auto", &auto_body, "info"),
                    ("blocked", "Blocked", &blocked_body, "warning"),
                    ("approval", "Approval", &approval_body, "warning"),
                    ("read-only", "Read", &read_only_body, "info"),
                    ("workspace-write", "Write", &workspace_write_body, "accent"),
                    ("full-access", "Full", &full_access_body, "warning"),
                    ("overrides", "Overrides", &overrides_body, "accent"),
                ],
                if blocked.is_empty() {
                    "summary"
                } else {
                    "blocked"
                },
                "warning",
                "tab switch • esc close",
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "/memory" => {
            let text = format_memory_report(transcript, scratchpad);
            emit_view("memory", "Memory", &text, "accent");
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "/files" => {
            let text = format_files_report(transcript, scratchpad);
            emit_view("files", "Files", &text, "info");
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "/tasks" => {
            let text = format_tasks_report(transcript, scratchpad);
            emit_view("tasks", "Tasks", &text, "accent");
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        _ if trimmed == "/plan" || trimmed.starts_with("/plan ") => {
            let rest = trimmed.strip_prefix("/plan").unwrap().trim();
            let args = parse_command_tail(rest)?;
            let action = args.first().map(String::as_str).unwrap_or("");
            let session_id = session_store
                .current_id()
                .unwrap_or(transcript.session_id.as_str())
                .to_string();

            if matches!(action, "accept" | "apply" | "execute") {
                let plan_body = load_plan_snapshot(slash_ctx, &session_id)
                    .unwrap_or_else(|| format_plan_report(transcript, scratchpad));
                let plan_path = persist_plan_snapshot(slash_ctx, &session_id, &plan_body)?;
                *session_mode = SessionMode::Chat;
                *permissions =
                    build_effective_permission_context(*session_mode, tools, permission_overrides);
                // Store the approved plan body so execution turns can follow it
                // even after plan mode has been exited.
                plan_state.status = Some(PlanStatus::Approved);
                plan_state.approved_plan_body = Some(plan_body.clone());
                scratchpad.log("decision", None, "approved current execution plan", None);
                persist_runtime_state(
                    session_store,
                    *session_mode,
                    permission_overrides,
                    plan_state,
                );
                emit_status_snapshot(config.auto_approve, transcript, *session_mode, plan_state);
                emit_view(
                    "plan",
                    "Plan Approved",
                    &format!(
                        "The current plan is approved for execution.\n\nExecution mode is active again and the approved plan will be carried into future turns until you clear or replace it.\nPlan snapshot: {}\n\n{}",
                        plan_path.display(),
                        plan_body
                    ),
                    "accent",
                );
                emit_notification("ui.turn.complete", serde_json::json!({}));
                return Ok(true);
            }

            if matches!(action, "reject" | "revise") {
                plan_state.status = Some(PlanStatus::Rejected);
                plan_state.approved_plan_body = None;
                persist_runtime_state(
                    session_store,
                    *session_mode,
                    permission_overrides,
                    plan_state,
                );
                emit_status_snapshot(config.auto_approve, transcript, *session_mode, plan_state);
                emit_view(
                    "plan",
                    "Plan Rejected",
                    "The current plan stays in review mode. Keep refining it or use `/plan accept` once it is ready to execute.",
                    "warning",
                );
                emit_notification("ui.turn.complete", serde_json::json!({}));
                return Ok(true);
            }

            if action == "clear" {
                plan_state.status = Some(PlanStatus::None);
                plan_state.approved_plan_body = None;
                persist_runtime_state(
                    session_store,
                    *session_mode,
                    permission_overrides,
                    plan_state,
                );
                emit_status_snapshot(config.auto_approve, transcript, *session_mode, plan_state);
                emit_view(
                    "plan",
                    "Plan State Cleared",
                    "Cleared the stored approved-plan context. Future execution turns will use only live conversation context.",
                    "info",
                );
                emit_notification("ui.turn.complete", serde_json::json!({}));
                return Ok(true);
            }

            if matches!(action, "off" | "exit" | "disable") {
                let plan_body = load_plan_snapshot(slash_ctx, &session_id)
                    .unwrap_or_else(|| format_plan_report(transcript, scratchpad));
                let plan_path = persist_plan_snapshot(slash_ctx, &session_id, &plan_body)?;
                *session_mode = SessionMode::Chat;
                *permissions =
                    build_effective_permission_context(*session_mode, tools, permission_overrides);
                if plan_state.status == Some(PlanStatus::Draft) {
                    plan_state.approved_plan_body = None;
                }
                persist_runtime_state(
                    session_store,
                    *session_mode,
                    permission_overrides,
                    plan_state,
                );
                emit_status_snapshot(config.auto_approve, transcript, *session_mode, plan_state);
                emit_view(
                    "plan",
                    "Plan Mode",
                    &format!(
                        "Plan mode disabled.\n\nThe agent is back in normal execution mode.\nLatest plan snapshot: {}",
                        plan_path.display()
                    ),
                    "accent",
                );
                emit_notification("ui.turn.complete", serde_json::json!({}));
                return Ok(true);
            }

            if action == "path" {
                let path = plan_snapshot_path(slash_ctx, &session_id);
                emit_view(
                    "plan",
                    "Plan Snapshot",
                    &format!("Plan snapshot path\n\n{}", path.display()),
                    "accent",
                );
                emit_notification("ui.turn.complete", serde_json::json!({}));
                return Ok(true);
            }

            if *session_mode != SessionMode::Plan {
                *session_mode = SessionMode::Plan;
                *permissions =
                    build_effective_permission_context(*session_mode, tools, permission_overrides);
                // A fresh planning cycle supersedes any previously approved plan.
                plan_state.status = Some(PlanStatus::Draft);
                plan_state.approved_plan_body = None;
                persist_runtime_state(
                    session_store,
                    *session_mode,
                    permission_overrides,
                    plan_state,
                );
                emit_status_snapshot(config.auto_approve, transcript, *session_mode, plan_state);
                let plan_body = format_plan_report(transcript, scratchpad);
                let plan_path = persist_plan_snapshot(slash_ctx, &session_id, &plan_body)?;
                let body = format!(
                    "Plan mode enabled.\n\nWrite and execution tools are blocked in this mode.\nThe next prompt will be treated as planning-first work.\nUse `/plan accept` to carry the approved plan into execution mode, `/plan reject` to keep iterating, or `/plan off` to leave planning without approval.\nPlan snapshot: {}\n\n{}",
                    plan_path.display(),
                    plan_body
                );
                emit_view("plan", "Plan Mode", &body, "accent");
                emit_notification("ui.turn.complete", serde_json::json!({}));
                return Ok(true);
            }

            let text = load_plan_snapshot(slash_ctx, &session_id).unwrap_or_else(|| {
                let plan_body = format_plan_report(transcript, scratchpad);
                let _ = persist_plan_snapshot(slash_ctx, &session_id, &plan_body);
                plan_body
            });
            emit_view(
                "plan",
                "Current Plan",
                &format!(
                    "Plan status: {}\n\n{}\n\nCommands\n  /plan accept\n  /plan reject\n  /plan clear\n  /plan off",
                    plan_state.status.unwrap_or(PlanStatus::None).as_str(),
                    text
                ),
                "accent",
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "/compact" => {
            match transcript.compact(6) {
                Some(summary) => {
                    agent_loop::compact_history(history, &summary, 6);
                    session_store.append_compaction(&summary);
                    emit_notification(
                        "ui.text.delta",
                        serde_json::json!({
                            "text": format!("Conversation context compacted.\n\n{summary}")
                        }),
                    );
                }
                None => {
                    emit_notification(
                        "ui.text.delta",
                        serde_json::json!({
                            "text": "Not enough conversation history to compact yet."
                        }),
                    );
                }
            }
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "/sessions" => {
            let sessions = session_store.list_sessions(20);
            // Keep sessions structured so the TUI can render them as part of
            // the current command turn instead of flattening them into text.
            emit_notification(
                "ui.session.list",
                serde_json::json!({
                    "sessions": sessions,
                }),
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "/session" => {
            let text = format_current_session_report(session_store, llm_config, transcript);
            emit_view("session", "Current Session", &text, "info");
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "/model" => {
            emit_view(
                "model",
                "Model",
                &format!("Current model: {}", llm_config.model),
                "info",
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        _ if trimmed.starts_with("/model ") => {
            let new_model = trimmed.strip_prefix("/model ").unwrap().trim();
            if new_model.is_empty() {
                emit_view(
                    "model",
                    "Model",
                    &format!("Current model: {}", llm_config.model),
                    "info",
                );
            } else {
                let old = llm_config.model.clone();
                llm_config.model = new_model.to_string();
                emit_view(
                    "model",
                    "Model",
                    &format!("Model switched: {} → {}", old, new_model),
                    "info",
                );
            }
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        _ if trimmed == "/resume" || trimmed.starts_with("/resume ") => {
            let reference = trimmed.strip_prefix("/resume").unwrap().trim();
            let reference = if reference.is_empty() {
                "latest"
            } else {
                reference
            };
            match session_store.resume_session(reference) {
                Some((sid, messages)) => {
                    restore_history_and_transcript_from_messages(
                        history, transcript, scratchpad, &messages,
                    );
                    if let Some(runtime_state) = session_store.load_runtime_state(&sid) {
                        let (restored_mode, restored_overrides, restored_plan_state) =
                            restore_runtime_session_state(runtime_state);
                        *session_mode = restored_mode;
                        *permission_overrides = restored_overrides;
                        *plan_state = restored_plan_state;
                    } else {
                        permission_overrides.reset();
                        *plan_state = PlanRuntimeState::default();
                        *session_mode = SessionMode::Chat;
                    }
                    *permissions = build_effective_permission_context(
                        *session_mode,
                        tools,
                        permission_overrides,
                    );
                    emit_status_snapshot(
                        config.auto_approve,
                        transcript,
                        *session_mode,
                        plan_state,
                    );
                    emit_notification(
                        "ui.text.delta",
                        serde_json::json!({
                            "text": if load_plan_snapshot(slash_ctx, &sid).is_some() {
                                format!(
                                    "Resumed session {} ({} messages). A plan snapshot is available; use `/plan` to inspect it.",
                                    sid,
                                    messages.len()
                                )
                            } else {
                                format!("Resumed session {} ({} messages)", sid, messages.len())
                            }
                        }),
                    );
                }
                None => {
                    emit_notification(
                        "ui.text.delta",
                        serde_json::json!({ "text": format!("Session not found: {}", reference) }),
                    );
                }
            }
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        _ if trimmed.starts_with("/session resume") => {
            let rest = trimmed.strip_prefix("/session resume").unwrap().trim();
            let reference = if rest.is_empty() { "latest" } else { rest };
            match session_store.resume_session(reference) {
                Some((sid, messages)) => {
                    restore_history_and_transcript_from_messages(
                        history, transcript, scratchpad, &messages,
                    );
                    if let Some(runtime_state) = session_store.load_runtime_state(&sid) {
                        let (restored_mode, restored_overrides, restored_plan_state) =
                            restore_runtime_session_state(runtime_state);
                        *session_mode = restored_mode;
                        *permission_overrides = restored_overrides;
                        *plan_state = restored_plan_state;
                    } else {
                        permission_overrides.reset();
                        *plan_state = PlanRuntimeState::default();
                        *session_mode = SessionMode::Chat;
                    }
                    *permissions = build_effective_permission_context(
                        *session_mode,
                        tools,
                        permission_overrides,
                    );
                    emit_status_snapshot(
                        config.auto_approve,
                        transcript,
                        *session_mode,
                        plan_state,
                    );
                    emit_notification(
                        "ui.text.delta",
                        serde_json::json!({
                            "text": if load_plan_snapshot(slash_ctx, &sid).is_some() {
                                format!(
                                    "Resumed session {} ({} messages). A plan snapshot is available; use `/plan` to inspect it.",
                                    sid,
                                    messages.len()
                                )
                            } else {
                                format!("Resumed session {} ({} messages)", sid, messages.len())
                            }
                        }),
                    );
                }
                None => {
                    emit_notification(
                        "ui.text.delta",
                        serde_json::json!({ "text": format!("Session not found: {}", reference) }),
                    );
                }
            }
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        _ if trimmed.starts_with("/session fork") => {
            let name = trimmed.strip_prefix("/session fork").unwrap().trim();
            let new_id = session_store.fork_session(name);
            emit_notification(
                "ui.text.delta",
                serde_json::json!({
                    "text": format!("Forked to new session: {}", new_id)
                }),
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "/status" => {
            let account = PrismPaths::discover()
                .ok()
                .and_then(|paths| paths.load_cli_state().ok())
                .and_then(|state| state.credentials);
            let status = format_status_report(
                slash_ctx,
                session_store,
                llm_config,
                transcript,
                permissions,
                tools,
                *session_mode,
                plan_state,
                config.auto_approve,
                account.as_ref(),
            );
            let config_output = run_cli_backed_slash_command(
                &[String::from("configure"), String::from("--show")],
                slash_ctx,
            )
            .await?;
            let usage = format_usage_report(transcript, session_store);
            emit_tabbed_view(
                "settings",
                "Settings",
                &[
                    ("status", "Status", &status, "info"),
                    ("config", "Config", &config_output, "info"),
                    ("usage", "Usage", &usage, "accent"),
                ],
                "status",
                "info",
                "tab switch • esc close",
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "/usage" => {
            let account = PrismPaths::discover()
                .ok()
                .and_then(|paths| paths.load_cli_state().ok())
                .and_then(|state| state.credentials);
            let status = format_status_report(
                slash_ctx,
                session_store,
                llm_config,
                transcript,
                permissions,
                tools,
                *session_mode,
                plan_state,
                config.auto_approve,
                account.as_ref(),
            );
            let config_output = run_cli_backed_slash_command(
                &[String::from("configure"), String::from("--show")],
                slash_ctx,
            )
            .await?;
            let usage = format_usage_report(transcript, session_store);
            emit_tabbed_view(
                "settings",
                "Settings",
                &[
                    ("status", "Status", &status, "info"),
                    ("config", "Config", &config_output, "info"),
                    ("usage", "Usage", &usage, "accent"),
                ],
                "usage",
                "info",
                "tab switch • esc close",
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        _ if trimmed == "/doctor" || trimmed.starts_with("/doctor ") => {
            let output =
                format_doctor_report(slash_ctx, llm_config, transcript, tools, *session_mode);
            emit_view("doctor", "Doctor", &output, "warning");
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        "/config" => {
            let account = PrismPaths::discover()
                .ok()
                .and_then(|paths| paths.load_cli_state().ok())
                .and_then(|state| state.credentials);
            let status = format_status_report(
                slash_ctx,
                session_store,
                llm_config,
                transcript,
                permissions,
                tools,
                *session_mode,
                plan_state,
                config.auto_approve,
                account.as_ref(),
            );
            let config_output = run_cli_backed_slash_command(
                &[String::from("configure"), String::from("--show")],
                slash_ctx,
            )
            .await?;
            let usage = format_usage_report(transcript, session_store);
            emit_tabbed_view(
                "settings",
                "Settings",
                &[
                    ("status", "Status", &status, "info"),
                    ("config", "Config", &config_output, "info"),
                    ("usage", "Usage", &usage, "accent"),
                ],
                "config",
                "info",
                "tab switch • esc close",
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        _ if trimmed.starts_with("/config ") => {
            let rest = trimmed.strip_prefix("/config").unwrap().trim();
            let mut args = vec![String::from("configure")];
            args.extend(parse_command_tail(rest)?);
            let output = run_cli_backed_slash_command(&args, slash_ctx).await?;
            emit_view("config", "Configuration", &output, "info");
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
        _ => {
            let Some(args) = parse_slash_command(trimmed)? else {
                return Ok(false);
            };

            if args.is_empty() {
                emit_notification(
                    "ui.text.delta",
                    serde_json::json!({ "text": "Enter a slash command or plain text." }),
                );
                emit_notification("ui.turn.complete", serde_json::json!({}));
                return Ok(true);
            }

            if handle_workflow_slash_command(&args, slash_ctx, policy_engine).await? {
                return Ok(true);
            }

            let output = run_cli_backed_slash_command(&args, slash_ctx).await?;
            if output.is_empty() {
                return Ok(false);
            }

            emit_notification("ui.text.delta", serde_json::json!({ "text": output }));
            emit_notification("ui.turn.complete", serde_json::json!({}));
            Ok(true)
        }
    }
}

// ── Main server loop ──────────────────────────────────────────────

/// Run the JSON-RPC stdio server. Blocks until stdin is closed.
pub async fn run_server(llm_config: LlmConfig, tool_server_config: ToolServer) -> Result<()> {
    // LlmClient is rebuilt per-turn so /model switches take effect.
    let _verify_config = LlmClient::new(llm_config.clone());

    tracing::info!("spawning python tool server");
    let mut tool_server: ToolServerHandle = tool_server_config
        .spawn()
        .await
        .context("failed to spawn tool server")?;

    // Fetch tool definitions from Python
    let tools_json = tool_server
        .list_tools()
        .await
        .context("failed to list tools")?;
    let mut tool_catalog = ToolCatalog::from_tool_server_json(&tools_json);
    tool_catalog.extend(command_tools::command_tools());
    let tools = Arc::new(tool_catalog);
    tracing::info!(tool_count = tools.len(), "loaded tool catalog");

    let config = Arc::new(AgentConfig {
        system_prompt: build_system_prompt(true),
        ..Default::default()
    });
    let slash_ctx = SlashCommandContext {
        current_exe: std::env::current_exe()
            .context("failed to locate current prism executable")?,
        project_root: tool_server_config.project_root.clone(),
        python_bin: tool_server_config.python_bin.clone(),
    };
    let command_tool_runtime = CommandToolRuntime {
        current_exe: slash_ctx.current_exe.clone(),
        project_root: slash_ctx.project_root.clone(),
        python_bin: slash_ctx.python_bin.clone(),
    };
    let hooks = Arc::new(build_default_hooks());

    let session_mode = SessionMode::Chat;
    let plan_state = PlanRuntimeState::default();
    let permission_overrides = PermissionOverrides::default();
    let permissions =
        build_effective_permission_context(session_mode, tools.as_ref(), &permission_overrides);
    let scratchpad = Scratchpad::new();

    // Session persistence
    let mut session_store = SessionStore::new(None);
    let startup_latest_session = session_store
        .list_sessions(1)
        .into_iter()
        .find(|session| session.is_latest)
        .map(|session| session.session_id);
    let session_id = session_store.new_session(&llm_config.model);
    persist_runtime_state(
        &session_store,
        session_mode,
        &permission_overrides,
        &plan_state,
    );

    // Approval channel — protocol sends responses, agent loop receives
    let (approval_tx, approval_rx) = tokio::sync::mpsc::channel::<agent_loop::ApprovalResponse>(1);
    let approval_rx = Arc::new(tokio::sync::Mutex::new(approval_rx));
    let live_permission_overrides =
        Arc::new(tokio::sync::RwLock::new(permission_overrides.clone()));

    // OPA policy engine — loads built-in + user/project policies
    let policy_engine = match prism_policy::PolicyEngine::with_discovery(None) {
        Ok(pe) => {
            tracing::info!(policies = pe.policy_count(), "OPA policy engine loaded");
            Some(pe)
        }
        Err(e) => {
            tracing::warn!(error = %e, "OPA policy engine failed to load — running without policies");
            None
        }
    };
    tracing::info!(session_id = %session_id, "started new session");

    let mut runtime = Some(ServerRuntime {
        tool_server,
        command_tool_runtime,
        llm_config,
        history: Vec::new(),
        transcript: TranscriptStore::new(None),
        session_mode,
        plan_state,
        permission_overrides,
        permissions,
        scratchpad,
        session_store,
        policy_engine,
    });
    let mut pending_turn: Option<oneshot::Receiver<ServerRuntime>> = None;
    let mut deferred_updates: Vec<DeferredRuntimeUpdate> = Vec::new();

    // Read JSON-RPC lines from stdin
    let stdin = io::stdin();
    let reader = stdin.lock();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        if line.trim().is_empty() {
            continue;
        }

        if let Some(mut receiver) = pending_turn.take() {
            match receiver.try_recv() {
                Ok(mut restored_runtime) => {
                    apply_deferred_runtime_updates(
                        &mut restored_runtime,
                        tools.as_ref(),
                        &mut deferred_updates,
                    );
                    persist_runtime_state(
                        &restored_runtime.session_store,
                        restored_runtime.session_mode,
                        &restored_runtime.permission_overrides,
                        &restored_runtime.plan_state,
                    );
                    sync_live_permission_overrides(
                        &live_permission_overrides,
                        &restored_runtime.permission_overrides,
                    )
                    .await;
                    emit_status_snapshot(
                        config.auto_approve,
                        &restored_runtime.transcript,
                        restored_runtime.session_mode,
                        &restored_runtime.plan_state,
                    );
                    runtime = Some(restored_runtime);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    pending_turn = Some(receiver);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    emit_notification(
                        "ui.text.delta",
                        serde_json::json!({
                            "text": "The active turn exited unexpectedly. You can continue with a new prompt."
                        }),
                    );
                    emit_notification("ui.turn.complete", serde_json::json!({}));
                }
            }
        }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                emit_error(-32700, &format!("Parse error: {e}"), Value::Null);
                continue;
            }
        };

        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let turn_active = runtime.is_none();

        match method {
            "init" => {
                let Some(runtime) = runtime.as_mut() else {
                    emit_error(-32000, "Cannot initialize while a turn is active", id);
                    continue;
                };

                let resume_ref = params.get("resume").and_then(|v| v.as_str()).unwrap_or("");
                let resume_ref = if resume_ref == "latest" {
                    startup_latest_session.as_deref().unwrap_or("latest")
                } else {
                    resume_ref
                };

                let mut welcome = serde_json::json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "tool_count": tools.len(),
                    "session_id": runtime.session_store.current_id().unwrap_or(""),
                });

                if !resume_ref.is_empty() {
                    if let Some((sid, messages)) = runtime.session_store.resume_session(resume_ref)
                    {
                        restore_history_and_transcript_from_messages(
                            &mut runtime.history,
                            &mut runtime.transcript,
                            &mut runtime.scratchpad,
                            &messages,
                        );
                        welcome["resumed"] = serde_json::json!(true);
                        welcome["session_id"] = serde_json::json!(sid);
                        welcome["resumed_messages"] = serde_json::json!(messages.len());
                        if let Some(runtime_state) = runtime.session_store.load_runtime_state(&sid)
                        {
                            let (restored_mode, restored_overrides, restored_plan_state) =
                                restore_runtime_session_state(runtime_state);
                            runtime.session_mode = restored_mode;
                            runtime.permission_overrides = restored_overrides;
                            runtime.plan_state = restored_plan_state;
                            runtime.permissions = build_effective_permission_context(
                                runtime.session_mode,
                                tools.as_ref(),
                                &runtime.permission_overrides,
                            );
                            sync_live_permission_overrides(
                                &live_permission_overrides,
                                &runtime.permission_overrides,
                            )
                            .await;
                        }
                        tracing::info!(
                            session_id = %sid,
                            messages = messages.len(),
                            "resumed session"
                        );
                    }
                }

                emit_response(id, serde_json::json!({ "status": "ok" }));
                emit_notification("ui.welcome", welcome);
                emit_status_snapshot(
                    config.auto_approve,
                    &runtime.transcript,
                    runtime.session_mode,
                    &runtime.plan_state,
                );
            }

            "input.message" => {
                let text = params.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if text.is_empty() {
                    emit_error(-32602, "Missing params.text", id);
                    continue;
                }

                emit_response(id, serde_json::json!({ "status": "ok" }));

                if turn_active {
                    emit_notification(
                        "ui.text.delta",
                        serde_json::json!({
                            "text": "A turn is already in progress. Respond to the approval prompt or wait for the current turn to finish."
                        }),
                    );
                    emit_notification("ui.turn.complete", serde_json::json!({}));
                    continue;
                }

                let mut bundle = runtime.take().expect("runtime should exist");
                bundle
                    .session_store
                    .append_message("user", text, "", "", None);
                sync_live_permission_overrides(
                    &live_permission_overrides,
                    &bundle.permission_overrides,
                )
                .await;
                pending_turn = Some(spawn_agent_turn(
                    bundle,
                    text.to_string(),
                    Arc::clone(&tools),
                    Arc::clone(&config),
                    Arc::clone(&hooks),
                    slash_ctx.clone(),
                    Arc::clone(&approval_rx),
                    Arc::clone(&live_permission_overrides),
                ));
            }

            "input.command" => {
                let command = params.get("command").and_then(|c| c.as_str()).unwrap_or("");
                emit_response(id.clone(), serde_json::json!({ "status": "ok" }));

                if turn_active {
                    emit_notification(
                        "ui.text.delta",
                        serde_json::json!({
                            "text": "A turn is already in progress. Respond to the approval prompt or wait for the current turn to finish."
                        }),
                    );
                    emit_notification("ui.turn.complete", serde_json::json!({}));
                    continue;
                }

                let runtime_ref = runtime.as_mut().expect("runtime should exist");
                let handled = match handle_command(
                    command,
                    &slash_ctx,
                    config.as_ref(),
                    &mut runtime_ref.tool_server,
                    &mut runtime_ref.session_store,
                    &mut runtime_ref.history,
                    &mut runtime_ref.llm_config,
                    &mut runtime_ref.transcript,
                    &mut runtime_ref.permissions,
                    &mut runtime_ref.permission_overrides,
                    &mut runtime_ref.scratchpad,
                    tools.as_ref(),
                    &mut runtime_ref.session_mode,
                    &mut runtime_ref.plan_state,
                    &mut runtime_ref.policy_engine,
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::error!(error = %error, command, "slash command failed");
                        emit_notification(
                            "ui.text.delta",
                            serde_json::json!({ "text": format!("Command error: {error}") }),
                        );
                        emit_notification("ui.turn.complete", serde_json::json!({}));
                        true
                    }
                };

                if handled {
                    let runtime_ref = runtime.as_mut().expect("runtime should exist");
                    persist_runtime_state(
                        &runtime_ref.session_store,
                        runtime_ref.session_mode,
                        &runtime_ref.permission_overrides,
                        &runtime_ref.plan_state,
                    );
                    sync_live_permission_overrides(
                        &live_permission_overrides,
                        &runtime_ref.permission_overrides,
                    )
                    .await;
                    emit_status_snapshot(
                        config.auto_approve,
                        &runtime_ref.transcript,
                        runtime_ref.session_mode,
                        &runtime_ref.plan_state,
                    );
                    continue;
                }

                let text = command.trim_start_matches('/').to_string();
                let mut bundle = runtime.take().expect("runtime should exist");
                bundle
                    .session_store
                    .append_message("user", &text, "", "", None);
                sync_live_permission_overrides(
                    &live_permission_overrides,
                    &bundle.permission_overrides,
                )
                .await;
                pending_turn = Some(spawn_agent_turn(
                    bundle,
                    text,
                    Arc::clone(&tools),
                    Arc::clone(&config),
                    Arc::clone(&hooks),
                    slash_ctx.clone(),
                    Arc::clone(&approval_rx),
                    Arc::clone(&live_permission_overrides),
                ));
            }

            "input.prompt_response" => {
                let response_str = params
                    .get("response")
                    .and_then(|r| r.as_str())
                    .unwrap_or("n");
                let tool_name = params.get("tool_name").and_then(|value| value.as_str());

                if let Some(tool_name) = tool_name {
                    match response_str {
                        "a" | "always" | "allow-session" => {
                            if let Some(runtime) = runtime.as_mut() {
                                runtime.permission_overrides.allow(tool_name);
                                runtime.permissions = build_effective_permission_context(
                                    runtime.session_mode,
                                    tools.as_ref(),
                                    &runtime.permission_overrides,
                                );
                                persist_runtime_state(
                                    &runtime.session_store,
                                    runtime.session_mode,
                                    &runtime.permission_overrides,
                                    &runtime.plan_state,
                                );
                                sync_live_permission_overrides(
                                    &live_permission_overrides,
                                    &runtime.permission_overrides,
                                )
                                .await;
                                emit_status_snapshot(
                                    config.auto_approve,
                                    &runtime.transcript,
                                    runtime.session_mode,
                                    &runtime.plan_state,
                                );
                            } else {
                                live_permission_overrides.write().await.allow(tool_name);
                                deferred_updates
                                    .push(DeferredRuntimeUpdate::AllowTool(tool_name.to_string()));
                            }
                        }
                        "b" | "block" | "deny-session" => {
                            if let Some(runtime) = runtime.as_mut() {
                                runtime.permission_overrides.deny(tool_name);
                                runtime.permissions = build_effective_permission_context(
                                    runtime.session_mode,
                                    tools.as_ref(),
                                    &runtime.permission_overrides,
                                );
                                persist_runtime_state(
                                    &runtime.session_store,
                                    runtime.session_mode,
                                    &runtime.permission_overrides,
                                    &runtime.plan_state,
                                );
                                sync_live_permission_overrides(
                                    &live_permission_overrides,
                                    &runtime.permission_overrides,
                                )
                                .await;
                                emit_status_snapshot(
                                    config.auto_approve,
                                    &runtime.transcript,
                                    runtime.session_mode,
                                    &runtime.plan_state,
                                );
                            } else {
                                live_permission_overrides.write().await.deny(tool_name);
                                deferred_updates
                                    .push(DeferredRuntimeUpdate::DenyTool(tool_name.to_string()));
                            }
                        }
                        _ => {}
                    }
                }

                let approval = match response_str {
                    "y" | "yes" | "allow" => agent_loop::ApprovalResponse::Allow,
                    "a" | "all" | "always" | "allow-session" => agent_loop::ApprovalResponse::Allow,
                    _ => agent_loop::ApprovalResponse::Deny,
                };
                let _ = approval_tx.try_send(approval);
                emit_response(id, serde_json::json!({ "status": "ok" }));
            }

            _ => {
                emit_error(-32601, &format!("Method not found: {method}"), id);
            }
        }
    }

    tracing::info!("stdin closed, shutting down");
    if let Some(mut receiver) = pending_turn.take() {
        if let Ok(mut restored_runtime) = receiver.try_recv() {
            apply_deferred_runtime_updates(
                &mut restored_runtime,
                tools.as_ref(),
                &mut deferred_updates,
            );
            sync_live_permission_overrides(
                &live_permission_overrides,
                &restored_runtime.permission_overrides,
            )
            .await;
            runtime = Some(restored_runtime);
        }
    }
    if let Some(mut runtime) = runtime {
        let _ = runtime.tool_server.shutdown().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_effective_permission_context, build_tool_card_payload, inline_list,
        load_plan_snapshot, parse_bash_slash_action, parse_command_tail, parse_diff_slash_action,
        parse_edit_slash_action, parse_python_slash_action, parse_read_slash_path,
        parse_slash_command, parse_write_slash_action, persist_plan_snapshot, pick_organization,
        pick_project, plan_snapshot_path, project_api_history, shell_command_join,
        summarize_api_view, truncate_for_ui, BashSlashAction, DiffSlashAction, EditSlashAction,
        PythonSlashAction, SessionMode, SlashCommandContext, WriteSlashAction,
    };
    use crate::commands::is_cli_backed_slash_root;
    use crate::permissions::PermissionOverrides;
    use crate::tool_catalog::ToolCatalog;
    use prism_client::api::{OrgInfo, ProjectInfo};
    use prism_ingest::llm::{ChatMessage, FunctionCall, ToolCallResponse};
    use prism_runtime::StoredCredentials;
    use tempfile::TempDir;

    #[test]
    fn parse_slash_command_handles_quotes() {
        let parsed = parse_slash_command(r#"/query "band gap materials" --json"#)
            .expect("quoted slash command should parse")
            .expect("slash command should return args");
        assert_eq!(parsed, vec!["query", "band gap materials", "--json"]);
    }

    #[test]
    fn parse_slash_command_rejects_unbalanced_quotes() {
        let error =
            parse_slash_command(r#"/query "broken"#).expect_err("unbalanced quotes should fail");
        assert!(error.to_string().contains("unmatched quotes"));
    }

    #[test]
    fn parse_slash_command_ignores_plain_text() {
        let parsed =
            parse_slash_command("just talk to the agent").expect("plain text should not error");
        assert!(parsed.is_none());
    }

    #[test]
    fn parse_command_tail_handles_quotes() {
        let parsed = parse_command_tail(r#"--model "gemma 4""#).expect("tail args should parse");
        assert_eq!(parsed, vec!["--model", "gemma 4"]);
    }

    #[test]
    fn shell_command_join_preserves_globs_and_quotes_spaced_args() {
        let joined = shell_command_join(&[
            "rg".to_string(),
            "TODO".to_string(),
            "*.rs".to_string(),
            "path with spaces".to_string(),
            "2>&1".to_string(),
        ]);
        assert_eq!(joined, "rg TODO *.rs 'path with spaces' 2>&1");
    }

    #[test]
    fn parse_bash_slash_action_supports_background_runs() {
        let parsed = parse_bash_slash_action(
            r#"/bash --background --timeout 45 --description "Run tests" cargo test -p prism-agent"#,
        )
        .expect("bash slash command should parse");

        assert_eq!(
            parsed,
            BashSlashAction::Execute {
                command: "cargo test -p prism-agent".to_string(),
                description: Some("Run tests".to_string()),
                timeout: Some(45),
                run_in_background: true,
            }
        );
    }

    #[test]
    fn parse_bash_slash_action_supports_task_subcommands() {
        assert_eq!(
            parse_bash_slash_action("/bash tasks").expect("tasks subcommand should parse"),
            BashSlashAction::Tasks
        );
        assert_eq!(
            parse_bash_slash_action("/bash read task_123").expect("read subcommand should parse"),
            BashSlashAction::Read {
                task_id: "task_123".to_string()
            }
        );
        assert_eq!(
            parse_bash_slash_action("/bash stop task_123").expect("stop subcommand should parse"),
            BashSlashAction::Stop {
                task_id: "task_123".to_string()
            }
        );
    }

    #[test]
    fn parse_python_slash_action_preserves_raw_code() {
        let parsed = parse_python_slash_action(r#"/python print("hello world")"#)
            .expect("python slash command should parse");

        assert_eq!(
            parsed,
            PythonSlashAction::Execute {
                code: r#"print("hello world")"#.to_string(),
                description: None,
                timeout: None,
            }
        );
    }

    #[test]
    fn parse_python_slash_action_supports_options_with_separator() {
        let parsed = parse_python_slash_action(
            "/python --timeout 30 --description \"quick math\" -- print(2 + 2)",
        )
        .expect("python slash command with options should parse");

        assert_eq!(
            parsed,
            PythonSlashAction::Execute {
                code: "print(2 + 2)".to_string(),
                description: Some("quick math".to_string()),
                timeout: Some(30),
            }
        );
    }

    #[test]
    fn parse_read_slash_path_supports_quoted_paths() {
        let parsed = parse_read_slash_path(r#"/read "src/path with spaces.rs""#)
            .expect("read slash command should parse");
        assert_eq!(parsed, "src/path with spaces.rs");
    }

    #[test]
    fn parse_write_slash_action_preserves_body_verbatim() {
        let parsed = parse_write_slash_action(
            "/write src/main.rs -- fn main() {\n    println!(\"hi\");\n}\n",
        )
        .expect("write slash command should parse");

        assert_eq!(
            parsed,
            WriteSlashAction::Write {
                path: "src/main.rs".to_string(),
                content: "fn main() {\n    println!(\"hi\");\n}\n".to_string(),
            }
        );
    }

    #[test]
    fn parse_edit_slash_action_preserves_old_and_new_blocks() {
        let parsed =
            parse_edit_slash_action("/edit src/main.rs --old -- old line\n--new -- new line\n")
                .expect("edit slash command should parse");

        assert_eq!(
            parsed,
            EditSlashAction::Edit {
                path: "src/main.rs".to_string(),
                old_text: "old line".to_string(),
                new_text: "new line\n".to_string(),
                replace_all: false,
            }
        );
    }

    #[test]
    fn parse_diff_slash_action_supports_repo_and_paths() {
        assert_eq!(
            parse_diff_slash_action("/diff").expect("repo diff should parse"),
            DiffSlashAction::Repo
        );
        assert_eq!(
            parse_diff_slash_action(r#"/diff "src/path with spaces.rs" Cargo.toml"#)
                .expect("path diff should parse"),
            DiffSlashAction::Paths {
                paths: vec![
                    "src/path with spaces.rs".to_string(),
                    "Cargo.toml".to_string(),
                ],
            }
        );
    }

    #[test]
    fn slash_root_recognizes_cli_commands_only() {
        assert!(is_cli_backed_slash_root("status"));
        assert!(is_cli_backed_slash_root("workflow"));
        assert!(is_cli_backed_slash_root("job-status"));
        assert!(!is_cli_backed_slash_root("session"));
        assert!(!is_cli_backed_slash_root("help"));
        assert!(!is_cli_backed_slash_root("unknown"));
    }

    #[test]
    fn truncate_for_ui_marks_truncation() {
        let truncated = truncate_for_ui("abcdef", 4);
        assert_eq!(truncated, "abcd\n\n[Output truncated]");
    }

    #[test]
    fn inline_list_summarizes_extra_items() {
        let items = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(inline_list(&items, "none", 2), "a, b, ... (+1 more)");
    }

    #[test]
    fn api_history_starts_at_last_compact_boundary() {
        let history = vec![
            ChatMessage {
                role: "user".to_string(),
                content: Some("old request".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "system".to_string(),
                content: Some(
                    "[Conversation context compacted]\nConversation summary\nPending work: finish the parser"
                        .to_string(),
                ),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some("new request".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        let visible = project_api_history(&history);
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].role, "system");
        assert_eq!(visible[1].content.as_deref(), Some("new request"));
    }

    #[test]
    fn api_view_summary_counts_tool_calls_and_previews_visible_messages() {
        let history = vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(
                    "[Conversation context compacted]\nConversation summary\nKey files: src/main.rs"
                        .to_string(),
                ),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCallResponse {
                    id: "call_1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "read_file".to_string(),
                        arguments: "{\"path\":\"src/main.rs\"}".to_string(),
                    },
                }]),
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some("fn main() {}".to_string()),
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
            },
        ];

        let summary = summarize_api_view(&history, "You are PRISM.");
        assert_eq!(summary.visible_messages, 3);
        assert_eq!(summary.tool_call_count, 1);
        assert_eq!(summary.assistant_messages, 1);
        assert_eq!(summary.tool_messages, 1);
        assert!(summary
            .visible_previews
            .iter()
            .any(|preview| preview.contains("tool calls: read_file")));
        assert_eq!(
            summary.compact_boundary_preview.as_deref(),
            Some("Conversation summary Key files: src/main.rs")
        );
    }

    #[test]
    fn session_permission_overrides_apply_in_chat_mode() {
        let tools = ToolCatalog::from_tool_server_json(&serde_json::json!({
            "tools": [
                {
                    "name": "execute_bash",
                    "description": "Guarded local shell access",
                    "input_schema": { "type": "object", "properties": {} },
                    "requires_approval": true
                }
            ]
        }));
        let mut overrides = PermissionOverrides::default();
        overrides.allow("execute_bash");
        let permissions = build_effective_permission_context(SessionMode::Chat, &tools, &overrides);
        assert!(permissions.auto_approves("execute_bash"));
        assert!(!permissions.blocks("execute_bash"));
    }

    #[test]
    fn plan_mode_denials_still_win_over_allow_override() {
        let tools = ToolCatalog::from_tool_server_json(&serde_json::json!({
            "tools": [
                {
                    "name": "execute_bash",
                    "description": "Guarded local shell access",
                    "input_schema": { "type": "object", "properties": {} },
                    "requires_approval": true
                }
            ]
        }));
        let mut overrides = PermissionOverrides::default();
        overrides.allow("execute_bash");
        let permissions = build_effective_permission_context(SessionMode::Plan, &tools, &overrides);
        assert!(permissions.auto_approves("execute_bash"));
        assert!(permissions.blocks("execute_bash"));
    }

    #[test]
    fn plan_snapshot_roundtrip_uses_project_local_path() {
        let tmp = TempDir::new().expect("temp dir");
        let slash_ctx = SlashCommandContext {
            current_exe: std::path::PathBuf::from("/tmp/prism"),
            project_root: tmp.path().to_path_buf(),
            python_bin: std::path::PathBuf::from("python3"),
        };

        let path = persist_plan_snapshot(&slash_ctx, "session123", "Current Plan\n  1. Audit")
            .expect("plan snapshot should persist");
        assert_eq!(path, plan_snapshot_path(&slash_ctx, "session123"));
        assert!(path.ends_with(".prism/plans/session123.md"));
        assert_eq!(
            load_plan_snapshot(&slash_ctx, "session123").as_deref(),
            Some("Current Plan\n  1. Audit")
        );
    }

    #[test]
    fn organization_picker_prefers_previous_selection() {
        let orgs = vec![
            OrgInfo {
                id: "org-a".to_string(),
                name: "Alpha".to_string(),
                slug: "alpha".to_string(),
            },
            OrgInfo {
                id: "org-b".to_string(),
                name: "Beta".to_string(),
                slug: "beta".to_string(),
            },
        ];
        let prior = StoredCredentials {
            org_id: Some("org-b".to_string()),
            ..StoredCredentials::default()
        };

        let (selected, note) =
            pick_organization(&orgs, Some(&prior)).expect("org should be selected");
        assert_eq!(selected.id, "org-b");
        assert!(note.contains("Reused organization"));
    }

    #[test]
    fn project_picker_prefers_sandbox_without_prior_selection() {
        let projects = vec![
            ProjectInfo {
                id: "proj-z".to_string(),
                name: "Zeta".to_string(),
                slug: "zeta".to_string(),
                org_id: "org-1".to_string(),
            },
            ProjectInfo {
                id: "proj-s".to_string(),
                name: "Sandbox".to_string(),
                slug: "sandbox".to_string(),
                org_id: "org-1".to_string(),
            },
        ];

        let (selected, note) = pick_project(&projects, None).expect("project should be selected");
        assert_eq!(selected.id, "proj-s");
        assert!(note.contains("default project"));
    }

    #[test]
    fn build_tool_card_payload_extracts_execute_bash_fields() {
        let (content, data) = build_tool_card_payload(
            "execute_bash",
            r#"{"success":true,"exit_code":0,"stdout":"ok\n","stderr":"","cwd":"/tmp/demo","return_code_interpretation":"No matches found"}"#,
            Some("$ rg missing src"),
            Some("$ rg missing src"),
        );

        assert!(content.contains("$ rg missing src"));
        assert!(content.contains("exit code: 0"));
        assert_eq!(data["cwd"], "/tmp/demo");
        assert_eq!(data["exit_code"], 0);
        assert_eq!(data["stdout"], "ok\n");
        assert_eq!(data["return_code_interpretation"], "No matches found");
    }

    #[test]
    fn build_tool_card_payload_extracts_prism_command_fields() {
        let (content, data) = build_tool_card_payload(
            "query",
            r#"{"success":true,"timed_out":false,"exit_code":0,"invocation":"prism query \"band gap materials\" --json","stdout":"{\"results\":[]}\n","stderr":""}"#,
            Some("prism query 'band gap materials' --json"),
            Some("query: prism query \"band gap materials\" --json"),
        );

        assert!(content.contains("command: prism query"));
        assert!(content.contains("status: completed"));
        assert!(content.contains("stdout"));
        assert_eq!(
            data["invocation"],
            "prism query \"band gap materials\" --json"
        );
        assert_eq!(data["exit_code"], 0);
        assert_eq!(data["success"], true);
    }

    #[test]
    fn build_tool_card_payload_parses_models_stdout_json() {
        let (content, data) = build_tool_card_payload(
            "models_list",
            r#"{"root":"models","success":true,"timed_out":false,"exit_code":0,"invocation":"prism models list --provider google --json","stdout":"[{\"model_id\":\"gemini-3.1-pro-preview\",\"display_name\":\"Gemini 3.1 Pro Preview\",\"provider\":\"google\"}]","stderr":""}"#,
            Some("prism models list --provider google --json"),
            Some("models_list: 1 results"),
        );

        assert!(content.contains("status: completed"));
        assert_eq!(data["root"], "models");
        assert_eq!(
            data["parsed_stdout"][0]["model_id"],
            "gemini-3.1-pro-preview"
        );
    }

    #[test]
    fn build_tool_card_payload_parses_discourse_stdout_json() {
        let (content, data) = build_tool_card_payload(
            "discourse_run",
            r#"{"root":"discourse","success":true,"timed_out":false,"exit_code":0,"invocation":"prism discourse run abc --json","stdout":"{\"instance_id\":\"inst-1\",\"events\":[{\"step\":\"started\"},{\"step\":\"complete\",\"total_turns\":2}]}","stderr":""}"#,
            Some("prism discourse run abc --json"),
            Some("discourse_run: 2 results"),
        );

        assert!(content.contains("status: completed"));
        assert_eq!(data["root"], "discourse");
        assert_eq!(data["parsed_stdout"]["instance_id"], "inst-1");
        assert_eq!(data["parsed_stdout"]["events"][1]["step"], "complete");
    }

    #[test]
    fn build_tool_card_payload_formats_read_file_results() {
        let (content, data) = build_tool_card_payload(
            "read_file",
            r#"{"path":"/tmp/demo/src/main.rs","content":"fn main() {}\n","size_bytes":12}"#,
            Some("read src/main.rs"),
            Some("read_file: /tmp/demo/src/main.rs (12 bytes)"),
        );

        assert!(content.contains("path: /tmp/demo/src/main.rs"));
        assert!(content.contains("size: 12 bytes"));
        assert!(content.contains("content\nfn main() {}"));
        assert_eq!(data["path"], "/tmp/demo/src/main.rs");
        assert_eq!(data["size_bytes"], 12);
    }

    #[test]
    fn build_tool_card_payload_formats_write_file_results() {
        let (content, data) = build_tool_card_payload(
            "write_file",
            r#"{"success":true,"path":"/tmp/demo/src/main.rs","size_bytes":17}"#,
            Some("write src/main.rs"),
            Some("write_file: /tmp/demo/src/main.rs (17 bytes)"),
        );

        assert!(content.contains("path: /tmp/demo/src/main.rs"));
        assert!(content.contains("status: written"));
        assert!(content.contains("size: 17 bytes"));
        assert_eq!(data["path"], "/tmp/demo/src/main.rs");
        assert_eq!(data["size_bytes"], 17);
        assert_eq!(data["success"], true);
    }

    #[test]
    fn build_tool_card_payload_formats_edit_file_results() {
        let (content, data) = build_tool_card_payload(
            "edit_file",
            r#"{"success":true,"path":"/tmp/demo/src/main.rs","size_bytes":19,"replacements":1}"#,
            Some("edit src/main.rs"),
            Some("edit_file: /tmp/demo/src/main.rs (1 replacements, 19 bytes)"),
        );

        assert!(content.contains("path: /tmp/demo/src/main.rs"));
        assert!(content.contains("status: edited"));
        assert!(content.contains("replacements: 1"));
        assert!(content.contains("size: 19 bytes"));
        assert_eq!(data["path"], "/tmp/demo/src/main.rs");
        assert_eq!(data["replacements"], 1);
        assert_eq!(data["success"], true);
    }
}
