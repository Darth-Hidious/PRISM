export type JsonRpcId = number | string;

export interface JsonRpcRequest {
  jsonrpc: "2.0";
  id: JsonRpcId;
  method: string;
  params?: unknown;
}

export interface JsonRpcNotification {
  jsonrpc: "2.0";
  method: string;
  params?: unknown;
}

export interface JsonRpcErrorPayload {
  code: number;
  message: string;
  data?: unknown;
}

export interface JsonRpcResponse {
  jsonrpc: "2.0";
  id: JsonRpcId;
  result?: unknown;
  error?: JsonRpcErrorPayload;
}

export type JsonRpcMessage =
  | JsonRpcRequest
  | JsonRpcNotification
  | JsonRpcResponse;

export interface UiWelcome {
  version: string;
  tool_count: number;
  session_id: string;
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

export interface UiTextDelta {
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
  data: Record<string, unknown>;
}

export interface UiPrompt {
  prompt_type: string;
  message: string;
  choices: string[];
  tool_name: string;
  tool_args: Record<string, unknown>;
  tool_description?: string;
  requires_approval: boolean;
  permission_mode?: string;
}

export interface UiCost {
  input_tokens: number;
  output_tokens: number;
  turn_cost: number;
  session_cost: number;
}

export interface UiViewTab {
  id: string;
  title: string;
  body: string;
  tone?: string;
}

export interface UiView {
  view_type: string;
  title: string;
  tone: string;
  tabs: UiViewTab[];
  selected_tab: string;
  footer?: string;
}

export interface UiPermissionTool {
  name: string;
  permission_mode: string;
  requires_approval: boolean;
  description: string;
  source?: string;
  source_detail?: string;
  current_behavior: string;
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

export interface BackendNotification {
  method: string;
  params?: unknown;
  receivedAt: string;
}

export interface OkResult {
  status: "ok";
}
