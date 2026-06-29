//! App state — the Model in TEA.

use crate::backend::BackendHandle;
use crate::msg::{AgentMsg, parse_notification};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_textarea::TextArea;
use serde_json::Value;

/// A single line in the chat scrollback.
#[derive(Debug, Clone)]
pub struct ChatLine {
    pub role: Role,
    pub text: String,
    pub kind: LineKind,
}

#[derive(Debug, Clone)]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone)]
pub enum LineKind {
    Text,
    /// Reasoning/thinking tokens (from reasoning_content) — dimmed, collapsible
    Thinking,
    ToolStart {
        tool_name: String,
        elapsed_ms: Option<u64>,
    },
    ToolResult {
        tool_name: String,
        content: String,
        elapsed_ms: u64,
        success: bool,
    },
    Approval {
        tool_name: String,
        message: String,
    },
    Status(String),
    Error(String),
    View {
        title: String,
        body: String,
    },
}

/// The focus state — which panel has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Focus {
    Chat,
    Input,
    Approval,
}

pub struct App {
    pub backend: BackendHandle,
    pub messages: Vec<ChatLine>,
    /// Maximum number of messages to keep in memory. Older messages
    /// are dropped (the backend keeps the full transcript for context).
    pub max_messages: usize,
    pub input: TextArea<'static>,
    pub focus: Focus,
    pub scroll_offset: u16,
    pub auto_scroll: bool,
    pub model: String,
    pub session_mode: String,
    pub message_count: usize,
    pub session_cost: f64,
    pub turn_cost: f64,
    pub is_waiting: bool,
    pub approval_pending: Option<(String, String)>,
    pub should_quit: bool,
    pub status_text: String,
    pub tool_count: u64,
    pub prism_version: String,
    // Streaming performance metrics
    pub tokens_received: u64,
    pub first_token_time: Option<std::time::Instant>,
    pub last_token_time: Option<std::time::Instant>,
    pub tokens_per_sec: f64,
    pub show_cost: bool,
    pub show_metrics: bool,
    // Thinking token state — separate from response text
    pub is_thinking: bool,
    pub thinking_expanded: bool,
}

impl App {
    pub fn new(backend: BackendHandle) -> Self {
        let mut input = TextArea::default();
        input.set_placeholder_text("Type a message... (Enter=send, Ctrl-C=quit)");

        Self {
            backend,
            messages: Vec::new(),
            input,
            focus: Focus::Input,
            scroll_offset: 0,
            auto_scroll: true,
            model: String::new(),
            session_mode: "chat".to_string(),
            message_count: 0,
            session_cost: 0.0,
            turn_cost: 0.0,
            is_waiting: false,
            approval_pending: None,
            should_quit: false,
            status_text: "Ready".to_string(),
            tool_count: 0,
            prism_version: String::new(),
            max_messages: 500,
            tokens_received: 0,
            first_token_time: None,
            last_token_time: None,
            tokens_per_sec: 0.0,
            show_cost: true,
            show_metrics: true,
            is_thinking: false,
            thinking_expanded: false,
        }
    }

    /// Handle a crossterm key event.
    pub fn handle_key(&mut self, key: KeyEvent) {
        // Global: Ctrl-C always quits
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        // If approval is pending, handle approval keys
        if self.approval_pending.is_some() {
            self.handle_approval_key(key);
            return;
        }

        // Global: Ctrl-L clears chat
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('l') {
            self.messages.clear();
            self.push_system("[chat cleared]");
            return;
        }

        // Global: Ctrl-T toggles thinking expansion
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('t') {
            self.thinking_expanded = !self.thinking_expanded;
            return;
        }

        // Global: Ctrl-M toggles metrics display
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('m') {
            self.show_metrics = !self.show_metrics;
            return;
        }

        // Global: Ctrl-$ toggles cost display
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('4') {
            self.show_cost = !self.show_cost;
            return;
        }

        // Tab cycles focus between chat and input
        if key.code == KeyCode::Tab {
            self.focus = match self.focus {
                Focus::Chat => Focus::Input,
                Focus::Input => Focus::Chat,
                Focus::Approval => Focus::Input,
            };
            return;
        }

        match self.focus {
            Focus::Input => self.handle_input_key(key),
            Focus::Chat => self.handle_chat_key(key),
            Focus::Approval => self.handle_approval_key(key),
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                // Submit the message
                let text = self.input.lines().join("\n");
                if !text.trim().is_empty() {
                    self.send_message(&text);
                    self.input = TextArea::default();
                    self.input
                        .set_placeholder_text("Type a message... (Enter=send, Ctrl-C=quit)");
                }
            }
            _ => {
                // Manual key handling for the textarea
                self.handle_textarea_key(key);
            }
        }
    }

    /// Convert crossterm key events to textarea operations manually.
    /// This avoids the ratatui-crossterm dependency mismatch.
    fn handle_textarea_key(&mut self, key: KeyEvent) {
        use ratatui_textarea::CursorMove;

        // Handle modifiers first
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('a') => {
                    self.input.move_cursor(CursorMove::Head);
                }
                KeyCode::Char('e') => {
                    self.input.move_cursor(CursorMove::End);
                }
                KeyCode::Char('u') => {
                    self.input.delete_line_by_head();
                }
                KeyCode::Char('k') => {
                    self.input.delete_line_by_end();
                }
                KeyCode::Char('w') => {
                    self.input.delete_word();
                }
                KeyCode::Char('d') => {
                    self.input.delete_char();
                }
                KeyCode::Left => {
                    self.input.move_cursor(CursorMove::Head);
                }
                KeyCode::Right => {
                    self.input.move_cursor(CursorMove::End);
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char(c) => {
                self.input.insert_char(c);
            }
            KeyCode::Backspace => {
                self.input.delete_char();
            }
            KeyCode::Delete => {
                self.input.delete_next_char();
            }
            KeyCode::Left => {
                self.input.move_cursor(CursorMove::Back);
            }
            KeyCode::Right => {
                self.input.move_cursor(CursorMove::Forward);
            }
            KeyCode::Up => {
                self.input.move_cursor(CursorMove::Up);
            }
            KeyCode::Down => {
                self.input.move_cursor(CursorMove::Down);
            }
            KeyCode::Home => {
                self.input.move_cursor(CursorMove::Head);
            }
            KeyCode::End => {
                self.input.move_cursor(CursorMove::End);
            }
            KeyCode::Esc => {
                self.focus = Focus::Chat;
            }
            _ => {}
        }
    }

    fn handle_chat_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.scroll_offset > 0 {
                    self.scroll_offset -= 1;
                }
                self.auto_scroll = false;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_offset += 1;
            }
            KeyCode::Char('G') => {
                self.auto_scroll = true;
                self.scroll_offset = 0;
            }
            KeyCode::Char('i') | KeyCode::Enter => {
                self.focus = Focus::Input;
            }
            _ => {}
        }
    }

    fn handle_approval_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                let _ = self.backend.send_approval("y");
                if let Some((tool, _)) = self.approval_pending.take() {
                    self.push_system(&format!("[approved {tool}]"));
                }
                self.focus = Focus::Input;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                let _ = self.backend.send_approval("n");
                if let Some((tool, _)) = self.approval_pending.take() {
                    self.push_system(&format!("[denied {tool}]"));
                }
                self.focus = Focus::Input;
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                let _ = self.backend.send_approval("a");
                if let Some((tool, _)) = self.approval_pending.take() {
                    self.push_system(&format!("[allow-all {tool}]"));
                }
                self.focus = Focus::Input;
            }
            _ => {}
        }
    }

    fn send_message(&mut self, text: &str) {
        let trimmed = text.trim();
        self.push_user(trimmed);

        if trimmed.starts_with('/') {
            let _ = self.backend.send_command(trimmed);
        } else {
            let _ = self.backend.send_message(trimmed);
        }
        self.is_waiting = true;
        self.is_thinking = true;
        self.status_text = "Thinking…".to_string();
        self.auto_scroll = true;
        // Reset streaming metrics
        self.tokens_received = 0;
        self.first_token_time = None;
        self.last_token_time = None;
        self.tokens_per_sec = 0.0;
    }

    /// Handle an agent backend JSON-RPC message.
    pub fn handle_backend_message(&mut self, msg: &Value) {
        let agent_msg = parse_notification(msg);
        self.apply_agent_msg(agent_msg);
    }

    pub fn apply_agent_msg(&mut self, msg: AgentMsg) {
        match msg {
            AgentMsg::Welcome {
                version,
                tool_count,
            } => {
                self.prism_version = version;
                self.tool_count = tool_count;
                self.push_system(&format!(
                    "PRISM v{} — {} tools available",
                    self.prism_version, self.tool_count
                ));
            }
            AgentMsg::Permissions {
                mode,
                auto_approved,
                ..
            } => {
                // Preserve current behavior: update mode if present.
                // The existing TUI doesn't display a permissions panel,
                // so we only surface auto-approve as a system line (if
                // the backend says it's on).  The full `raw` payload is
                // retained in the variant for the approval-state patch.
                if let Some(m) = mode {
                    self.session_mode = m;
                }
                if auto_approved.unwrap_or(false) {
                    self.push_system("[auto-approve enabled for this session]");
                }
            }
            AgentMsg::SessionList { sessions, .. } => {
                // Current behavior: display as a system line listing
                // the session count.  The full JSON objects are retained
                // in the variant for future rendering.
                if sessions.is_empty() {
                    self.push_system("[no previous sessions]");
                } else {
                    self.push_system(&format!("[{} previous session(s)]", sessions.len()));
                }
            }
            AgentMsg::Status {
                model,
                mode,
                message_count,
            } => {
                self.model = model;
                self.session_mode = mode;
                self.message_count = message_count;
            }
            AgentMsg::TextDelta(text) => {
                // Track streaming metrics
                let now = std::time::Instant::now();
                if self.first_token_time.is_none() {
                    self.first_token_time = Some(now);
                }
                self.last_token_time = Some(now);
                self.tokens_received += 1;

                // Calculate tokens/sec
                if let (Some(first), Some(last)) = (self.first_token_time, self.last_token_time) {
                    let elapsed = last.duration_since(first).as_secs_f64();
                    if elapsed > 0.0 {
                        self.tokens_per_sec = self.tokens_received as f64 / elapsed;
                    }
                }

                self.append_assistant_text(&text);
                self.is_waiting = false;
                self.is_thinking = false;
            }
            AgentMsg::ThinkingDelta(text) => {
                // Reasoning tokens — track metrics but render separately
                let now = std::time::Instant::now();
                if self.first_token_time.is_none() {
                    self.first_token_time = Some(now);
                }
                self.last_token_time = Some(now);
                self.tokens_received += 1;

                if let (Some(first), Some(last)) = (self.first_token_time, self.last_token_time) {
                    let elapsed = last.duration_since(first).as_secs_f64();
                    if elapsed > 0.0 {
                        self.tokens_per_sec = self.tokens_received as f64 / elapsed;
                    }
                }

                self.append_thinking_text(&text);
                self.is_waiting = false;
            }
            AgentMsg::TextFlush => {
                self.is_waiting = false;
                self.status_text = "Ready".to_string();
            }
            AgentMsg::ToolStart {
                tool_name, verb, ..
            } => {
                // `..` ignores call_id, preview, approval_required —
                // current behavior only pushes a tool-start line.
                self.push_message(ChatLine {
                    role: Role::Tool,
                    text: format!("{} {}", verb, tool_name),
                    kind: LineKind::ToolStart {
                        tool_name,
                        elapsed_ms: None,
                    },
                });
                self.is_waiting = false;
            }
            AgentMsg::ToolCard {
                tool_name,
                content,
                card_type,
                elapsed_ms,
                ..
            } => {
                // `..` ignores call_id, provenance_id, data —
                // current behavior only pushes a result/error line.
                let success = card_type != "error";
                let elapsed = elapsed_ms.unwrap_or(0);
                if !success {
                    self.push_message(ChatLine {
                        role: Role::Tool,
                        text: format!("{}: {}", tool_name, content),
                        kind: LineKind::Error(format!("{}: {}", tool_name, content)),
                    });
                } else {
                    self.push_message(ChatLine {
                        role: Role::Tool,
                        text: format!("{}: {}", tool_name, content),
                        kind: LineKind::ToolResult {
                            tool_name,
                            content,
                            elapsed_ms: elapsed,
                            success,
                        },
                    });
                }
            }
            AgentMsg::ApprovalPrompt {
                tool_name, message, ..
            } => {
                // `..` ignores call_id, tool_args, tool_description,
                // requires_approval, permission_mode, choices, prompt_type —
                // current behavior uses only tool_name + message.
                self.approval_pending = Some((tool_name.clone(), message.clone()));
                self.focus = Focus::Approval;
                self.push_message(ChatLine {
                    role: Role::System,
                    text: format!("{}: {}", tool_name, message),
                    kind: LineKind::Approval { tool_name, message },
                });
            }
            AgentMsg::Cost {
                turn_cost,
                session_cost,
                ..
            } => {
                // `..` ignores input_tokens, output_tokens, cache_tokens —
                // current behavior only updates cost figures.
                self.turn_cost = turn_cost;
                self.session_cost = session_cost;
            }
            AgentMsg::TurnComplete => {
                self.is_waiting = false;
                self.status_text = "Ready".to_string();
            }
            AgentMsg::View { title, tabs } => {
                for (tab_title, body) in tabs {
                    self.push_message(ChatLine {
                        role: Role::System,
                        text: format!("[{} > {}]\n{}", title, tab_title, body),
                        kind: LineKind::View {
                            title: title.clone(),
                            body,
                        },
                    });
                }
            }
            AgentMsg::BackendWarning { code, message } => {
                let label = code.unwrap_or_else(|| "warning".to_string());
                self.push_system(&format!("[{label}] {message}"));
            }
            AgentMsg::BackendError {
                code,
                message,
                recoverable,
            } => {
                let prefix = match (code, recoverable) {
                    (Some(c), Some(false)) => format!("[fatal error {c}]"),
                    (Some(c), _) => format!("[error {c}]"),
                    (None, Some(false)) => "[fatal error]".to_string(),
                    (None, _) => "[error]".to_string(),
                };
                self.push_error(&format!("{prefix} {message}"));
            }
            AgentMsg::Error(e) => {
                self.push_error(&e);
            }
            AgentMsg::Unknown(_) => {}
        }
    }

    // ── Message helpers ───────────────────────────────────────────

    /// Append a message and trim if over the max.
    fn push_message(&mut self, line: ChatLine) {
        self.messages.push(line);
        self.trim_messages();
    }

    /// Drop oldest messages when over the max. Keeps a sliding window
    /// of the most recent messages. The backend keeps the full
    /// transcript for context — the TUI only needs the visible portion.
    fn trim_messages(&mut self) {
        while self.messages.len() > self.max_messages {
            self.messages.remove(0);
        }
    }

    pub fn push_user(&mut self, text: &str) {
        self.push_message(ChatLine {
            role: Role::User,
            text: text.to_string(),
            kind: LineKind::Text,
        });
    }

    pub fn push_system(&mut self, text: &str) {
        self.push_message(ChatLine {
            role: Role::System,
            text: text.to_string(),
            kind: LineKind::Status(text.to_string()),
        });
    }

    pub fn push_error(&mut self, text: &str) {
        self.push_message(ChatLine {
            role: Role::System,
            text: text.to_string(),
            kind: LineKind::Error(text.to_string()),
        });
    }

    pub fn append_assistant_text(&mut self, delta: &str) {
        if let Some(last) = self.messages.last_mut()
            && matches!(last.role, Role::Assistant)
            && matches!(last.kind, LineKind::Text)
        {
            last.text.push_str(delta);
            return;
        }
        self.messages.push(ChatLine {
            role: Role::Assistant,
            text: delta.to_string(),
            kind: LineKind::Text,
        });
    }

    /// Append thinking/reasoning tokens to a separate thinking buffer.
    /// Rendered dimmed and collapsible.
    pub fn append_thinking_text(&mut self, delta: &str) {
        if let Some(last) = self.messages.last_mut()
            && matches!(last.role, Role::Assistant)
            && matches!(last.kind, LineKind::Thinking)
        {
            last.text.push_str(delta);
            return;
        }
        self.messages.push(ChatLine {
            role: Role::Assistant,
            text: delta.to_string(),
            kind: LineKind::Thinking,
        });
    }
}
