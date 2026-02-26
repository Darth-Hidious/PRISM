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
}

export interface UiWelcome {
  version: string;
  provider: string;
  capabilities: Record<string, any>;
  tool_count: number;
  skill_count: number;
  auto_approve: boolean;
}

export interface UiStatus {
  auto_approve: boolean;
  message_count: number;
  has_plan: boolean;
}

export interface UiTurnComplete {
}

export interface UiSessionList {
  sessions: any[];
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
}

export interface InputPromptResponse {
  prompt_type?: string;
  response?: string;
}

export interface InputLoadSession {
  session_id?: string;
}

export type UIEvent = UiTextDelta | UiTextFlush | UiToolStart | UiCard | UiCost | UiPrompt | UiWelcome | UiStatus | UiTurnComplete | UiSessionList;
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
};

