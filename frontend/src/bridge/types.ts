// Auto-generated from app/backend/protocol.py — DO NOT EDIT
// Regenerate: python3 -m app.backend.protocol --emit-ts

export interface UiTextDelta {
  text: string;
}

export interface UiTextFlush {
  text: string;
}

export interface UiToolStart {
  tool_name: string;
  call_id: string;
  verb: string;
  preview?: string;
}

export interface UiCard {
  card_type: string;
  tool_name: string;
  elapsed_ms: number;
  content: string;
  data: Record<string, any>;
}

export interface UiCost {
  input_tokens: number;
  output_tokens: number;
  turn_cost: number;
  session_cost: number;
}

export interface UiPrompt {
  prompt_type: string;
  message: string;
  choices: any[];
  tool_name: string;
  tool_args: Record<string, any>;
  tool_description?: string;
  requires_approval?: boolean;
  permission_mode?: string;
}

export interface UiWelcome {
  version: string;
  status?: Record<string, any>;
  auto_approve?: boolean;
  tool_count?: number;
  session_id?: string;
  resumed?: boolean;
  resumed_messages?: number;
}

export interface UiStatus {
  auto_approve: boolean;
  message_count: number;
  has_plan: boolean;
  session_mode: string;
  plan_status?: string;
  model?: string;
  project_root?: string;
}

export interface UiTurnComplete {
}

export interface UiSessionList {
  sessions: Array<{
    session_id: string;
    created_at: number;
    turn_count: number;
    model: string;
    size_kb: number;
    is_latest: boolean;
  }>;
}

export interface UiView {
  view_type: string;
  title: string;
  body?: string;
  tone: string;
  tabs?: UiViewTab[];
  selected_tab?: string;
  footer?: string;
}

export interface UiViewTab {
  id: string;
  title: string;
  body: string;
  tone?: string;
}

export interface UiPermissions {
  mode: string;
  auto_approved: UiPermissionTool[];
  blocked: UiPermissionTool[];
  approval_required: UiPermissionTool[];
  read_only: UiPermissionTool[];
  workspace_write: UiPermissionTool[];
  full_access: UiPermissionTool[];
  allow_overrides: string[];
  deny_overrides: string[];
  notice?: string;
}

export interface UiPermissionTool {
  name: string;
  permission_mode: string;
  requires_approval: boolean;
  description: string;
  current_behavior: string;
}

export interface Init {
  provider?: string;
  auto_approve?: boolean;
  resume?: string;
}

export interface InputMessage {
  text?: string;
}

export interface InputCommand {
  command?: string;
  silent?: boolean;
}

export interface InputPromptResponse {
  prompt_type?: string;
  response?: string;
  tool_name?: string;
}

export interface InputLoadSession {
  session_id?: string;
}

export type UIEvent = UiTextDelta | UiTextFlush | UiToolStart | UiCard | UiCost | UiPrompt | UiWelcome | UiStatus | UiTurnComplete | UiSessionList | UiView | UiPermissions;
export type InputEvent = Init | InputMessage | InputCommand | InputPromptResponse | InputLoadSession;

export const UI_EVENT_MAP: Record<string, string> = {
  "ui.text.delta": "UiTextDelta",
  "ui.text.flush": "UiTextFlush",
  "ui.tool.start": "UiToolStart",
  "ui.card": "UiCard",
  "ui.cost": "UiCost",
  "ui.prompt": "UiPrompt",
  "ui.welcome": "UiWelcome",
  "ui.status": "UiStatus",
  "ui.turn.complete": "UiTurnComplete",
  "ui.session.list": "UiSessionList",
  "ui.view": "UiView",
  "ui.permissions": "UiPermissions",
};
