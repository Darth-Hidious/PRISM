#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Serialize, Debug, Clone)]
pub struct InitParams {
    pub auto_approve: bool,
    pub resume: String,
}

#[derive(Serialize, Debug, Clone)]
pub struct InitRequest {
    pub jsonrpc: String,
    pub method: String,
    pub id: u64,
    pub params: InitParams,
}

#[derive(Serialize, Debug, Clone)]
pub struct InputMessageParams {
    pub text: String,
}

#[derive(Serialize, Debug, Clone)]
pub struct InputMessageRequest {
    pub jsonrpc: String,
    pub method: String,
    pub id: u64,
    pub params: InputMessageParams,
}

#[derive(Serialize, Debug, Clone)]
pub struct InputCommandParams {
    pub command: String,
    pub silent: bool,
}

#[derive(Serialize, Debug, Clone)]
pub struct InputCommandRequest {
    pub jsonrpc: String,
    pub method: String,
    pub id: u64,
    pub params: InputCommandParams,
}

#[derive(Serialize, Debug, Clone)]
pub struct ApprovalRespondParams {
    pub response: String,
}

#[derive(Serialize, Debug, Clone)]
pub struct ApprovalRespondRequest {
    pub jsonrpc: String,
    pub method: String,
    pub id: u64,
    pub params: ApprovalRespondParams,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UiWelcome {
    pub version: String,
    pub tool_count: usize,
    pub session_id: String,
    pub resumed: Option<bool>,
    pub resumed_messages: Option<usize>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UiStatus {
    pub auto_approve: bool,
    pub message_count: usize,
    pub has_plan: bool,
    pub session_mode: String,
    pub plan_status: Option<String>,
    pub model: Option<String>,
    pub project_root: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UiTextDelta {
    pub text: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UiTextFlush {
    pub text: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UiToolStart {
    pub tool_name: String,
    pub call_id: Option<String>,
    pub verb: String,
    pub preview: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UiCard {
    pub card_type: String,
    pub tool_name: String,
    pub elapsed_ms: u64,
    pub content: String,
    pub data: serde_json::Value,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UiPrompt {
    pub prompt_type: String,
    pub message: String,
    pub choices: Vec<String>,
    pub tool_name: String,
    pub tool_args: serde_json::Value,
    pub tool_description: Option<String>,
    pub requires_approval: bool,
    pub permission_mode: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UiCost {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub turn_cost: f64,
    pub session_cost: f64,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UiTurnComplete {}

#[derive(Deserialize, Debug, Clone)]
pub struct UiViewTab {
    pub id: String,
    pub title: String,
    pub body: String,
    pub tone: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UiView {
    pub view_type: String,
    pub title: String,
    pub tone: String,
    pub tabs: Vec<UiViewTab>,
    pub selected_tab: String,
    pub footer: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UiPermissionTool {
    pub name: String,
    pub permission_mode: String,
    pub requires_approval: bool,
    pub description: String,
    pub source: Option<String>,
    pub source_detail: Option<String>,
    pub current_behavior: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UiPermissions {
    pub mode: String,
    pub auto_approved: Vec<UiPermissionTool>,
    pub blocked: Vec<UiPermissionTool>,
    pub approval_required: Vec<UiPermissionTool>,
    pub read_only: Vec<UiPermissionTool>,
    pub workspace_write: Vec<UiPermissionTool>,
    pub full_access: Vec<UiPermissionTool>,
    pub allow_overrides: Vec<String>,
    pub deny_overrides: Vec<String>,
    pub notice: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UiSessionMeta {
    pub session_id: String,
    pub created_at: i64,
    pub turn_count: usize,
    pub model: String,
    pub size_kb: usize,
    pub is_latest: bool,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UiSessionList {
    pub sessions: Vec<UiSessionMeta>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "method", content = "params")]
pub enum ProtocolNotification {
    #[serde(rename = "ui.welcome")]
    Welcome(UiWelcome),
    #[serde(rename = "ui.status")]
    Status(UiStatus),
    #[serde(rename = "ui.text.delta")]
    TextDelta(UiTextDelta),
    #[serde(rename = "ui.text.flush")]
    TextFlush(UiTextFlush),
    #[serde(rename = "ui.tool.start")]
    ToolStart(UiToolStart),
    #[serde(rename = "ui.card")]
    Card(UiCard),
    #[serde(rename = "ui.prompt")]
    Prompt(UiPrompt),
    #[serde(rename = "ui.cost")]
    Cost(UiCost),
    #[serde(rename = "ui.turn.complete")]
    TurnComplete(UiTurnComplete),
    #[serde(rename = "ui.view")]
    View(UiView),
    #[serde(rename = "ui.permissions")]
    Permissions(UiPermissions),
    #[serde(rename = "ui.session.list")]
    SessionList(UiSessionList),
}

#[derive(Deserialize, Debug, Clone)]
pub struct RpcNotification {
    pub jsonrpc: String,
    #[serde(flatten)]
    pub notification: ProtocolNotification,
}
