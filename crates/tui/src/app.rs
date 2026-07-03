//! App state — the Model in TEA.

use crate::backend::BackendHandle;
use crate::command;
use crate::form::{self, Form, FormField, FormOutcome};
use crate::gh::{self, GhPanel, GhTab};
use crate::keymap;
use crate::knowledge::{self, IngestPhase, KnowledgePane, KnowledgeTab};
use crate::msg::{AgentMsg, parse_notification};
use crate::sanitize::sanitize_for_render;
use crate::theme;
use crate::toast::{self, ToastKind};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
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
    Workspace,
    Approval,
}

/// Which tab of the Workspace sidebar is active.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WorkspaceTab {
    Activity,
    Tools,
    Files,
}

/// A transient full-overlay modal, dismissed by any key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Modal {
    Help,
    Cost,
    Model,
    Tools,
}

/// Command palette state (Ctrl-P) — the opencode-style command launcher.
///
/// While `open`, the palette intercepts every keypress (see
/// [`App::handle_palette_key`]); the transcript is dimmed behind it.
/// `query` drives fuzzy filtering of [`command::CATALOG`] and `selected`
/// is the highlighted row.
#[derive(Debug, Clone, Default)]
pub struct CommandPalette {
    pub open: bool,
    pub query: String,
    pub selected: usize,
}

/// Which-key panel state (`?`) — the opencode-style keymap reference.
///
/// A persistent, grouped, scrollable overlay of every TUI keybinding
/// (see [`keymap::KEYMAP`]). Unlike the Help modal it stays open until
/// explicitly closed and can be scrolled. `scroll` is clamped against
/// `whichkey_max_scroll`, which the renderer recomputes each frame
/// (content height − viewport), mirroring the chat-scroll pattern.
#[derive(Debug, Clone, Default)]
pub struct WhichKey {
    pub open: bool,
    pub scroll: u16,
}

/// Theme picker state — opencode-style `dialog-theme-list`.
///
/// Reached via the palette command `theme.list`. j/k move, Enter applies,
/// Esc cancels. `selected` tracks the highlighted row; the active theme is
/// only changed on Enter (or kept on cancel).
#[derive(Debug, Clone, Default)]
pub struct ThemePicker {
    pub open: bool,
    pub selected: usize,
}

/// Model picker state — opencode-style fuzzy model switcher.
/// Populated from the `ui.model.list` notification; selecting an entry sends
/// `/model <id>` (the backend switches and replies with `ui.status`).
#[derive(Debug, Clone, Default)]
pub struct ModelPicker {
    pub open: bool,
    pub models: Vec<Value>,
    pub current: String,
    pub selected: usize,
    pub query: String,
    pub loading: bool,
}

/// GPU picker state — the live compute-procurement catalog (palette entry
/// `compute.gpus`). Populated from the `ui.gpu.list` notification; Enter
/// pre-fills the prompt with a provision request for the selected offer.
/// The visible window follows `selected` (model-picker style scrolling).
#[derive(Debug, Clone, Default)]
pub struct GpuPicker {
    pub open: bool,
    pub gpus: Vec<Value>,
    pub selected: usize,
    pub loading: bool,
}

/// Account status read from `~/.prism/credentials.json` (client-side).
#[derive(Debug, Clone, Default)]
pub struct AccountStatus {
    pub logged_in: bool,
    pub user: String,
    pub org: String,
    pub project: String,
}

/// Account dialog — MARC27 login/logout. Status is read locally; Login/Logout
/// dispatch to the backend's existing `/login` (device flow) and `/logout`.
#[derive(Debug, Clone, Default)]
pub struct AccountDialog {
    pub open: bool,
    pub status: AccountStatus,
    pub busy: bool,
}

/// Session picker — list/resume saved sessions. Populated from
/// `ui.session.list`; Enter sends `/resume <id>`.
#[derive(Debug, Clone, Default)]
pub struct SessionPicker {
    pub open: bool,
    pub sessions: Vec<Value>,
    pub selected: usize,
    pub query: String,
    pub loading: bool,
}

/// View panel — a tabbed, scrollable surface for `ui.view` results (tools,
/// status, context, files, tasks, memory, permissions, usage, doctor,
/// config, diff, …). One panel upgrades every view-emitting command.
#[derive(Debug, Clone, Default)]
pub struct ViewPanel {
    pub open: bool,
    pub title: String,
    pub tabs: Vec<(String, String)>,
    pub active_tab: usize,
    pub scroll: u16,
    pub max_scroll: std::cell::Cell<u16>,
}

/// Bespoke Tools window — the live tool catalog grouped by approval, with a
/// fuzzy filter and scroll. (A purpose-built window, not the generic view.)
#[derive(Debug, Clone, Default)]
pub struct ToolsWindow {
    pub open: bool,
    pub query: String,
    pub selected: usize,
}

/// Bespoke Status window — a live runtime dashboard built from App state
/// (model / mode / session / counts / cost / tokens), not the `/status` text.
#[derive(Debug, Clone, Default)]
pub struct StatusWindow {
    pub open: bool,
}

/// Bespoke Config window — a file viewer for prism.toml / .mcp.json /
/// ~/.prism/config.toml / credentials (redacted), with file switching + scroll.
#[derive(Debug, Clone, Default)]
pub struct ConfigWindow {
    pub open: bool,
    pub files: Vec<(String, String)>, // (label, content)
    pub active: usize,
    pub scroll: u16,
    pub max_scroll: std::cell::Cell<u16>,
}

/// API-key window — enter/store provider keys (Anthropic, OpenAI, etc.).
#[derive(Debug, Clone, Default)]
pub struct ApiKeyWindow {
    pub open: bool,
    pub provider_idx: usize,
    pub key_input: String,
    pub status: Vec<(String, bool)>, // (env_var, has_key)
}

/// One row of the Workspace *Activity* tab, tied back to the transcript
/// message it was derived from (`msg_index`) so Enter can show the full
/// underlying event.
#[derive(Debug, Clone)]
pub struct ActivityEntry {
    /// Row type: "prompt" | "tool" | "file".
    pub kind: &'static str,
    pub label: String,
    /// Index into [`App::messages`] of the source [`ChatLine`].
    pub msg_index: usize,
    /// Tool success for "tool" rows; `None` for prompt/file rows.
    pub ok: Option<bool>,
}

/// One row of the Workspace *Files* tab (a file touched by a tool).
#[derive(Debug, Clone)]
pub struct TouchedFile {
    pub path: String,
    /// Index into [`App::messages`] of the tool result that touched it.
    pub msg_index: usize,
}

/// Tools whose results are treated as file modifications.
pub(crate) fn is_file_tool(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "edit" | "edit_file" | "create_file" | "apply_patch" | "file"
    )
}

/// First non-empty line of a tool result.
pub(crate) fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// Extract a file path from a write/edit tool's result, e.g.
/// "Updated crates/tui/src/app.rs" -> "crates/tui/src/app.rs".
pub(crate) fn extract_path(content: &str) -> Option<String> {
    let first = first_line(content);
    for kw in ["Updated ", "Wrote ", "Created ", "Modified ", "Edited "] {
        if let Some(rest) = first.strip_prefix(kw) {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Link picker (`o` in chat focus) — collects http(s) URLs from the
/// transcript (newest turn first) and opens the selected one in the
/// system browser after an explicit confirm dialog.
#[derive(Debug, Clone, Default)]
pub struct LinkPicker {
    pub open: bool,
    pub urls: Vec<String>,
    pub selected: usize,
    /// When true, show the "do you want to go to this website?" confirm
    /// dialog for `urls[selected]` instead of the list.
    pub confirm: bool,
}

/// What a submitted form drives. The [`Form`] widget is generic; this
/// enum is the dispatch table from "user pressed Enter" to an action.
#[derive(Debug, Clone, PartialEq)]
pub enum FormTarget {
    /// Set/clear the standing session goal (palette `goal.set`).
    Goal,
    /// Deep-research launch (palette `sci.research`) — composes a
    /// `start_background_research` instruction into the prompt box.
    Research,
}

/// An open form pane: the widget plus what submit dispatches to.
#[derive(Debug, Clone)]
pub struct FormPane {
    pub form: Form,
    pub target: FormTarget,
}

pub const API_PROVIDERS: &[(&str, &str)] = &[
    ("Anthropic", "ANTHROPIC_API_KEY"),
    ("OpenAI", "OPENAI_API_KEY"),
    ("Google", "GOOGLE_API_KEY"),
    ("Mistral", "MISTRAL_API_KEY"),
    ("Cohere", "COHERE_API_KEY"),
];

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
    /// Session title shown in the header. Derived from the first user message
    /// (opencode-style) since the backend doesn't send one; "New session" until then.
    pub session_title: String,
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
    // Workspace sidebar — the right-hand panel (Activity / Tools / Files)
    pub workspace_tab: WorkspaceTab,
    pub workspace_selected: usize,
    pub workspace_expanded: bool,
    /// Max chat scroll offset, recomputed by the renderer each frame
    /// (content height − viewport). Lets key handlers clamp/anchor scrolling
    /// without knowing the terminal size.
    pub view_max_scroll: std::cell::Cell<u16>,
    /// Transient overlay modal (help / cost / model), dismissed by any key.
    pub modal: Option<Modal>,
    /// Optional session goal shown in the Workspace sidebar (set via /goal).
    pub goal: Option<String>,
    /// Command palette (Ctrl-P) overlay state.
    pub palette: CommandPalette,
    /// Which-key panel (`?`) overlay state.
    pub which_key: WhichKey,
    /// Max which-key scroll offset, recomputed by the renderer each frame.
    pub whichkey_max_scroll: std::cell::Cell<u16>,
    /// Active theme index into [`theme::THEMES`].
    pub theme_index: usize,
    /// Theme picker overlay state.
    pub theme_picker: ThemePicker,
    /// Active toast notifications (auto-expiring, non-blocking).
    pub toasts: Vec<toast::Toast>,
    /// GitHub panel state (Issues / PRs / CI).
    pub gh: GhPanel,
    /// Model picker state (fuzzy switcher over the hosted catalog).
    pub model_picker: ModelPicker,
    /// GPU picker state (live compute catalog → provision prompt).
    pub gpu_picker: GpuPicker,
    /// Account dialog (MARC27 login/logout + status).
    pub account: AccountDialog,
    /// Session picker (list/resume).
    pub session_picker: SessionPicker,
    /// View panel (tabbed/scrollable results for /tools /status /context …).
    pub view: ViewPanel,
    /// Live tool catalog (names) for the sidebar Tools tab, from `/tools`.
    pub tool_catalog: Vec<Value>,
    /// Bespoke Tools window.
    pub tools_window: ToolsWindow,
    /// Bespoke Status window.
    pub status_window: StatusWindow,
    /// Bespoke Config window (file viewer).
    pub config_window: ConfigWindow,
    /// API-key window.
    pub apikey_window: ApiKeyWindow,
    /// Link picker (`o`): open a transcript URL in the browser.
    pub link_picker: LinkPicker,
    /// Open form pane (generic structured input), if any.
    pub form: Option<FormPane>,
    /// Knowledge pane (Search | Ingest tabs + file browser).
    pub knowledge: KnowledgePane,
}

impl App {
    pub fn new(backend: BackendHandle) -> Self {
        let mut input = TextArea::default();
        input.set_placeholder_text("Type a message... (Enter=send, /help, Ctrl-C=quit)");

        Self {
            backend,
            messages: Vec::new(),
            input,
            focus: Focus::Input,
            scroll_offset: 0,
            auto_scroll: true,
            model: String::new(),
            session_mode: "chat".to_string(),
            session_title: "New session".to_string(),
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
            workspace_tab: WorkspaceTab::Activity,
            workspace_selected: 0,
            workspace_expanded: false,
            view_max_scroll: std::cell::Cell::new(0),
            modal: None,
            goal: None,
            palette: CommandPalette::default(),
            which_key: WhichKey::default(),
            whichkey_max_scroll: std::cell::Cell::new(0),
            theme_index: theme::DEFAULT,
            theme_picker: ThemePicker::default(),
            toasts: Vec::new(),
            gh: GhPanel::default(),
            model_picker: ModelPicker::default(),
            gpu_picker: GpuPicker::default(),
            account: AccountDialog::default(),
            session_picker: SessionPicker::default(),
            view: ViewPanel::default(),
            tool_catalog: Vec::new(),
            tools_window: ToolsWindow::default(),
            status_window: StatusWindow::default(),
            config_window: ConfigWindow::default(),
            apikey_window: ApiKeyWindow::default(),
            link_picker: LinkPicker::default(),
            form: None,
            knowledge: KnowledgePane::default(),
        }
    }

    /// Handle a crossterm key event.
    pub fn handle_key(&mut self, key: KeyEvent) {
        // The command palette (Ctrl-P) intercepts all keys while open,
        // mirroring opencode's DialogSelect. Inside the palette, Ctrl-C
        // and Esc *cancel the palette* — they do not quit the app. Only
        // a closed palette lets the global Ctrl-C exit.
        if self.palette.open {
            self.handle_palette_key(key);
            return;
        }

        // An open form pane intercepts keys (typed input + navigation),
        // mirroring the palette: Esc/Ctrl-C cancel the pane, not the app.
        if self.form.is_some() {
            self.handle_form_key(key);
            return;
        }

        // The Knowledge pane intercepts keys while open.
        if self.knowledge.open {
            self.handle_knowledge_key(key);
            return;
        }

        // The which-key panel (`?`) intercepts keys while open: j/k scroll,
        // `?`/q/Esc/Ctrl-C close it. Like the palette, Ctrl-C here cancels
        // the panel rather than quitting the app.
        if self.which_key.open {
            self.handle_whichkey_key(key);
            return;
        }

        // The theme picker intercepts keys while open: j/k move, Enter
        // applies, Esc/Ctrl-C cancels.
        if self.theme_picker.open {
            self.handle_theme_picker_key(key);
            return;
        }

        // GitHub panel intercepts keys while open.
        if self.gh.open {
            self.handle_gh_key(key);
            return;
        }

        // Model picker intercepts keys while open.
        if self.model_picker.open {
            self.handle_model_picker_key(key);
            return;
        }

        // GPU picker intercepts keys while open.
        if self.gpu_picker.open {
            self.handle_gpu_picker_key(key);
            return;
        }

        // Account dialog intercepts keys while open.
        if self.account.open {
            self.handle_account_key(key);
            return;
        }

        // Session picker intercepts keys while open.
        if self.session_picker.open {
            self.handle_session_picker_key(key);
            return;
        }

        // View panel intercepts keys while open.
        if self.view.open {
            self.handle_view_key(key);
            return;
        }

        // Tools window intercepts keys while open.
        if self.tools_window.open {
            self.handle_tools_window_key(key);
            return;
        }

        // Status window intercepts keys while open.
        if self.status_window.open {
            self.handle_status_window_key(key);
            return;
        }

        // Config window intercepts keys while open.
        if self.config_window.open {
            self.handle_config_window_key(key);
            return;
        }

        // API-key window intercepts keys while open.
        if self.apikey_window.open {
            self.handle_apikey_key(key);
            return;
        }

        // Link picker intercepts keys while open.
        if self.link_picker.open {
            self.handle_link_picker_key(key);
            return;
        }

        // Global: Ctrl-C always quits
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        // An open modal overlay swallows the next keypress to dismiss itself.
        if self.modal.is_some() {
            self.modal = None;
            return;
        }

        // If approval is pending, handle approval keys
        if self.approval_pending.is_some() {
            self.handle_approval_key(key);
            return;
        }

        // Global: Ctrl-P opens the command palette (opencode primitive).
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p') {
            self.open_palette();
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

        // Global: PageUp/PageDown scroll the transcript from any focus, so the
        // user never has to hunt for the chat pane to scroll.
        if key.code == KeyCode::PageUp {
            self.scroll_up(10);
            return;
        }
        if key.code == KeyCode::PageDown {
            self.scroll_down(10);
            return;
        }

        // Tab cycles focus: Input → Workspace → Chat → Input
        if key.code == KeyCode::Tab {
            self.focus = match self.focus {
                Focus::Input => Focus::Workspace,
                Focus::Workspace => Focus::Chat,
                Focus::Chat => Focus::Input,
                Focus::Approval => Focus::Input,
            };
            return;
        }

        match self.focus {
            Focus::Input => self.handle_input_key(key),
            Focus::Chat => self.handle_chat_key(key),
            Focus::Workspace => self.handle_workspace_key(key),
            Focus::Approval => self.handle_approval_key(key),
        }
    }

    /// Handle a mouse event — the wheel scrolls the transcript, so scrolling
    /// works the way people expect without hunting for a focus mode.
    pub fn handle_mouse(&mut self, ev: MouseEvent) {
        match ev.kind {
            MouseEventKind::ScrollUp => self.mouse_scroll(-3),
            MouseEventKind::ScrollDown => self.mouse_scroll(3),
            _ => {}
        }
    }

    /// Route a mouse-wheel delta to the scrollable surface that is active:
    /// the which-key panel when it's open, otherwise the chat transcript.
    /// (`delta > 0` scrolls down toward newer content.)
    fn mouse_scroll(&mut self, delta: i32) {
        if self.which_key.open {
            let max = self.whichkey_max_scroll.get();
            let next = (self.which_key.scroll as i32).saturating_add(delta);
            self.which_key.scroll = next.clamp(0, max as i32) as u16;
            return;
        }
        if delta >= 0 {
            self.scroll_down(delta as u16);
        } else {
            self.scroll_up((-delta) as u16);
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
                        .set_placeholder_text("Type a message... (Enter=send, /help, Ctrl-C=quit)");
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
            KeyCode::Up | KeyCode::Char('k') => self.scroll_up(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_down(1),
            KeyCode::Char('g') | KeyCode::Home => {
                self.auto_scroll = false;
                self.scroll_offset = 0;
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.auto_scroll = true;
            }
            KeyCode::Char('i') | KeyCode::Enter => {
                self.focus = Focus::Input;
            }
            KeyCode::Char('o') => self.open_link_picker(),
            KeyCode::Char('?') => self.open_which_key(),
            KeyCode::Backspace => self.new_session(),
            _ => {}
        }
    }

    /// Scroll the transcript up by `n` lines (toward older messages).
    fn scroll_up(&mut self, n: u16) {
        if self.auto_scroll {
            // Leaving auto-follow: anchor at the current bottom first so the
            // first PageUp lands one page above the newest line, not the top.
            self.scroll_offset = self.view_max_scroll.get();
            self.auto_scroll = false;
        }
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Scroll the transcript down by `n` lines; re-enable auto-follow at bottom.
    fn scroll_down(&mut self, n: u16) {
        let max = self.view_max_scroll.get();
        self.scroll_offset = self.scroll_offset.saturating_add(n).min(max);
        if self.scroll_offset >= max {
            self.auto_scroll = true;
        }
    }

    /// Navigate the Workspace sidebar: ←/→ switch tab, ↑/↓ move selection,
    /// Enter opens a detail modal for the selected item, Space expands it
    /// inline, i/Esc jump back to input.
    fn handle_workspace_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => self.workspace_prev_tab(),
            KeyCode::Right | KeyCode::Char('l') => self.workspace_next_tab(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.workspace_selected = self.workspace_selected.saturating_sub(1);
                self.workspace_expanded = false;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.workspace_selected = self.workspace_selected.saturating_add(1);
                self.workspace_expanded = false;
            }
            KeyCode::Enter => self.open_workspace_detail(),
            KeyCode::Char(' ') => {
                self.workspace_expanded = !self.workspace_expanded;
            }
            KeyCode::Char('?') => self.open_which_key(),
            KeyCode::Char('i') | KeyCode::Esc => self.focus = Focus::Input,
            _ => {}
        }
    }

    // ── Workspace derivations & detail modal ────────────────────────

    /// Reconstruct the Activity feed from the message stream. Shared by
    /// the sidebar renderer and the Enter detail modal so both always
    /// agree on row order.
    pub fn derive_activity(&self) -> Vec<ActivityEntry> {
        let mut out: Vec<ActivityEntry> = Vec::new();
        for (i, m) in self.messages.iter().enumerate() {
            match (&m.role, &m.kind) {
                (Role::User, LineKind::Text) => out.push(ActivityEntry {
                    kind: "prompt",
                    label: format!("\"{}\"", m.text.trim()),
                    msg_index: i,
                    ok: None,
                }),
                (
                    _,
                    LineKind::ToolResult {
                        tool_name,
                        content,
                        success,
                        ..
                    },
                ) => {
                    out.push(ActivityEntry {
                        kind: "tool",
                        label: tool_name.clone(),
                        msg_index: i,
                        ok: Some(*success),
                    });
                    if is_file_tool(tool_name)
                        && let Some(path) = extract_path(content)
                    {
                        out.push(ActivityEntry {
                            kind: "file",
                            label: path,
                            msg_index: i,
                            ok: None,
                        });
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Files touched by file-modifying tools, deduplicated by path.
    pub fn derive_files(&self) -> Vec<TouchedFile> {
        let mut out: Vec<TouchedFile> = Vec::new();
        for (i, m) in self.messages.iter().enumerate() {
            if let LineKind::ToolResult {
                tool_name,
                content,
                success,
                ..
            } = &m.kind
                && *success
                && is_file_tool(tool_name)
                && let Some(path) = extract_path(content)
                && !out.iter().any(|f| f.path == path)
            {
                out.push(TouchedFile { path, msg_index: i });
            }
        }
        out
    }

    /// Enter in the Workspace sidebar: open a detail modal for the
    /// selected item, reusing the existing view panel (scroll/Esc).
    ///   - Tools:    name, approval, description, schema (if present) and
    ///     the per-tool config file at ~/.prism/tools.d/<tool>.toml.
    ///   - Files:    the file's content (text files, capped at 200 KB).
    ///   - Activity: the underlying event of that row as pretty JSON.
    pub fn open_workspace_detail(&mut self) {
        match self.workspace_tab {
            WorkspaceTab::Tools => {
                if self.tool_catalog.is_empty() {
                    self.toast("tool catalog not loaded yet", ToastKind::Info);
                    return;
                }
                let sel = self.workspace_selected.min(self.tool_catalog.len() - 1);
                let tool = self.tool_catalog[sel].clone();
                let name = tool
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                self.open_detail_view(format!("Tool — {name}"), tool_detail_body(&tool));
            }
            WorkspaceTab::Files => {
                let files = self.derive_files();
                if files.is_empty() {
                    self.toast("no files touched yet", ToastKind::Info);
                    return;
                }
                let sel = self.workspace_selected.min(files.len() - 1);
                let path = files[sel].path.clone();
                self.open_detail_view(format!("File — {path}"), read_file_capped(&path));
            }
            WorkspaceTab::Activity => {
                let items = self.derive_activity();
                if items.is_empty() {
                    self.toast("no activity yet", ToastKind::Info);
                    return;
                }
                let sel = self.workspace_selected.min(items.len() - 1);
                let entry = &items[sel];
                let Some(msg) = self.messages.get(entry.msg_index) else {
                    return;
                };
                let body = serde_json::to_string_pretty(&chatline_detail_json(msg))
                    .unwrap_or_else(|_| "(unrenderable event)".to_string());
                self.open_detail_view(format!("Activity — {}. {}", sel + 1, entry.kind), body);
            }
        }
    }

    /// Show `body` in the existing view panel (single tab, scrollable,
    /// Esc closes). Content is sanitized like every backend-sourced view.
    fn open_detail_view(&mut self, title: String, body: String) {
        self.view.title = sanitize_for_render(&title);
        self.view.tabs = vec![(String::new(), sanitize_for_render(&body))];
        self.view.active_tab = 0;
        self.view.scroll = 0;
        self.view.open = true;
    }

    fn workspace_next_tab(&mut self) {
        self.workspace_tab = match self.workspace_tab {
            WorkspaceTab::Activity => WorkspaceTab::Tools,
            WorkspaceTab::Tools => WorkspaceTab::Files,
            WorkspaceTab::Files => WorkspaceTab::Activity,
        };
        self.workspace_selected = 0;
        self.workspace_expanded = false;
        self.ensure_tool_catalog();
    }

    fn workspace_prev_tab(&mut self) {
        self.workspace_tab = match self.workspace_tab {
            WorkspaceTab::Activity => WorkspaceTab::Files,
            WorkspaceTab::Tools => WorkspaceTab::Activity,
            WorkspaceTab::Files => WorkspaceTab::Tools,
        };
        self.workspace_selected = 0;
        self.workspace_expanded = false;
        self.ensure_tool_catalog();
    }

    /// The catalog arrives at startup (`ui.tools.catalog`), so no fetch here.
    fn ensure_tool_catalog(&mut self) {}

    fn handle_approval_key(&mut self, key: KeyEvent) {
        let tool = self
            .approval_pending
            .as_ref()
            .map(|(t, _)| t.clone())
            .unwrap_or_default();
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                let _ = self.backend.send_approval("y", &tool);
                self.approval_pending = None;
                self.push_system(&format!("[approved {tool}]"));
                self.focus = Focus::Input;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                let _ = self.backend.send_approval("n", &tool);
                self.approval_pending = None;
                self.push_system(&format!("[denied {tool}]"));
                self.focus = Focus::Input;
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                let _ = self.backend.send_approval("a", &tool);
                self.approval_pending = None;
                self.push_system(&format!("[allow-all {tool}]"));
                self.focus = Focus::Input;
            }
            _ => {}
        }
    }

    // ── Command palette (Ctrl-P) ────────────────────────────────────

    pub fn open_palette(&mut self) {
        self.palette.open = true;
        self.palette.query.clear();
        self.palette.selected = 0;
    }

    fn close_palette(&mut self) {
        self.palette.open = false;
    }

    /// Keys while the palette is open. Mirrors opencode's DialogSelect:
    /// ↑↓ (and Ctrl-P/Ctrl-N) move, Enter dispatches, Esc/Ctrl-C cancels.
    fn handle_palette_key(&mut self, key: KeyEvent) {
        let cancel = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Esc;
        if cancel {
            self.close_palette();
            return;
        }

        match key.code {
            KeyCode::Up => self.palette_move(-1),
            KeyCode::Down => self.palette_move(1),
            KeyCode::PageUp => self.palette_move(-10),
            KeyCode::PageDown => self.palette_move(10),
            KeyCode::Home => self.palette.selected = 0,
            KeyCode::End => {
                let n = command::fuzzy_sorted(&self.palette.query).len();
                self.palette.selected = n.saturating_sub(1);
            }
            KeyCode::Enter => {
                let id = command::fuzzy_sorted(&self.palette.query)
                    .get(self.palette.selected)
                    .map(|c| c.id)
                    .map(str::to_owned);
                match id {
                    Some(id) => self.dispatch_command(&id),
                    None => self.close_palette(),
                }
            }
            KeyCode::Backspace => {
                self.palette.query.pop();
                self.palette.selected = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.palette.query.push(c);
                self.palette.selected = 0;
            }
            _ => {}
        }

        // Re-clamp selection into the (possibly shrunken) result set.
        let n = command::fuzzy_sorted(&self.palette.query).len();
        if n > 0 {
            self.palette.selected = self.palette.selected.min(n - 1);
        }
    }

    fn palette_move(&mut self, delta: i32) {
        let n = command::fuzzy_sorted(&self.palette.query).len();
        if n == 0 {
            return;
        }
        let max = (n - 1) as i32;
        let next = (self.palette.selected as i32 + delta).clamp(0, max);
        self.palette.selected = next as usize;
    }

    // ── Form pane (generic structured input) ────────────────────────

    /// Open a form pane. Any previously open form is replaced.
    pub fn open_form(&mut self, form: Form, target: FormTarget) {
        self.form = Some(FormPane { form, target });
    }

    /// Keys while a form is open: delegate to the widget, then act on
    /// the outcome (submit dispatches by target; cancel just closes).
    fn handle_form_key(&mut self, key: KeyEvent) {
        let Some(pane) = self.form.as_mut() else {
            return;
        };
        match pane.form.handle_key(key) {
            FormOutcome::Continue => {}
            FormOutcome::Cancel => self.cancel_form(),
            FormOutcome::Submit => self.submit_form(),
        }
    }

    fn cancel_form(&mut self) {
        self.form = None;
    }

    /// Dispatch a submitted form by target. Closes the pane unless the
    /// handler kept it open (e.g. validation failure).
    fn submit_form(&mut self) {
        let Some(pane) = self.form.take() else {
            return;
        };
        match pane.target.clone() {
            FormTarget::Goal => {
                let goal = pane.form.text_value("goal").trim().to_string();
                if goal.is_empty() {
                    self.goal = None;
                    self.push_system("[goal cleared]");
                    self.toast("goal cleared", ToastKind::Info);
                } else {
                    self.push_system(&format!("[goal set — {goal}]"));
                    self.goal = Some(goal);
                    self.toast("goal set", ToastKind::Ok);
                }
            }
            FormTarget::Research => {
                let question = pane.form.text_value("question").trim().to_string();
                if question.is_empty() {
                    // Validation failure: keep the pane open.
                    self.toast("enter a research question first", ToastKind::Warn);
                    self.form = Some(pane);
                    return;
                }
                // GPU-picker pattern: the agent calls
                // start_background_research and progress surfaces
                // through the existing tool-card path.
                let prompt = research_prompt(&pane.form);
                self.prefill_prompt(&prompt);
            }
        }
    }

    /// Palette `sci.research` — ask the right questions before firing
    /// the verb: question, depth, and data-source toggles.
    ///
    /// Honesty notes (verified against app/tools/agent_runs.py): the
    /// platform call is `{question, depth}` — depth is the only source
    /// control the engine enforces (0 = knowledge-graph only, 1+ = web).
    /// The Web toggle therefore maps onto depth; the other source
    /// toggles are recorded inside the question text (the tool client
    /// forwards no separate params object) and marked "(advisory)"
    /// because the engine does not act on them yet.
    pub fn open_research_form(&mut self) {
        let form = Form::new(
            "Deep research — background run",
            "launch",
            vec![
                FormField::text("question", "Question", ""),
                FormField::stepper("depth", "Depth", 1, 0, 5)
                    .with_note("0 = local-only · 1+ = web"),
                FormField::toggle("src_web", "Web", true).with_note("off forces depth 0"),
                FormField::toggle("src_kg", "Knowledge Graph", true).with_note("(advisory)"),
                FormField::toggle("src_prov", "Provenance/memory", false).with_note("(advisory)"),
                FormField::toggle("src_mesh", "Mesh/partner data", false).with_note("(advisory)"),
            ],
        );
        self.open_form(form, FormTarget::Research);
    }

    /// Palette `goal.set` — a one-field form instead of the old
    /// "type: /goal <text>" toast. Submitting empty clears the goal.
    pub fn open_goal_form(&mut self) {
        let current = self.goal.clone().unwrap_or_default();
        let form = Form::new(
            "Set goal",
            "set goal",
            vec![
                FormField::text("goal", "Standing goal", &current)
                    .with_note("sent to the agent each turn; empty clears"),
            ],
        );
        self.open_form(form, FormTarget::Goal);
    }

    // ── Knowledge pane (Search | Ingest) ─────────────────────────────

    /// Open the Knowledge pane on `tab`. The browser starts at the
    /// current working directory (the project the TUI was launched in).
    pub fn open_knowledge_pane(&mut self, tab: KnowledgeTab) {
        let start = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
        self.knowledge = KnowledgePane::opened(tab, start);
    }

    /// Keys while the Knowledge pane is open. Tab switches the mode
    /// tabs; everything else routes to the active tab (search form,
    /// file browser, or metadata form). Esc backs out one level.
    fn handle_knowledge_key(&mut self, key: KeyEvent) {
        let cancel = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Esc;

        // Tab switches modes (config-window convention). BackTab too.
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.knowledge.tab = Some(match self.knowledge.active_tab() {
                KnowledgeTab::Search => KnowledgeTab::Ingest,
                KnowledgeTab::Ingest => KnowledgeTab::Search,
            });
            return;
        }

        match self.knowledge.active_tab() {
            KnowledgeTab::Search => {
                if cancel {
                    self.knowledge.open = false;
                    return;
                }
                match self.knowledge.search_form.handle_key(key) {
                    FormOutcome::Continue => {}
                    FormOutcome::Cancel => self.knowledge.open = false,
                    FormOutcome::Submit => {
                        match knowledge::search_prompt(&self.knowledge.search_form) {
                            Some(prompt) => {
                                self.knowledge.open = false;
                                self.prefill_prompt(&prompt);
                            }
                            None => self.toast(
                                "enter a query and pick at least one scope",
                                ToastKind::Warn,
                            ),
                        }
                    }
                }
            }
            KnowledgeTab::Ingest => match self.knowledge.phase {
                IngestPhase::Browse => {
                    if cancel {
                        self.knowledge.open = false;
                        return;
                    }
                    match key.code {
                        KeyCode::Down | KeyCode::Char('j') => {
                            self.knowledge.browser.move_selection(1)
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            self.knowledge.browser.move_selection(-1)
                        }
                        KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
                            self.knowledge.browser.up()
                        }
                        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                            if let Some(path) = self.knowledge.browser.enter() {
                                self.knowledge.ingest_file = Some(path);
                                self.knowledge.phase = IngestPhase::Meta;
                            }
                        }
                        _ => {}
                    }
                }
                IngestPhase::Meta => {
                    if cancel {
                        // Back to the browser, keeping the pane open.
                        self.knowledge.phase = IngestPhase::Browse;
                        self.knowledge.ingest_file = None;
                        return;
                    }
                    match self.knowledge.meta_form.handle_key(key) {
                        FormOutcome::Continue => {}
                        FormOutcome::Cancel => {
                            self.knowledge.phase = IngestPhase::Browse;
                            self.knowledge.ingest_file = None;
                        }
                        FormOutcome::Submit => {
                            if let Some(path) = self.knowledge.ingest_file.clone() {
                                let prompt =
                                    knowledge::ingest_prompt(&path, &self.knowledge.meta_form);
                                self.knowledge.open = false;
                                self.prefill_prompt(&prompt);
                            }
                        }
                    }
                }
            },
        }
    }

    /// GPU-picker pattern: put `prompt` in the input box for review —
    /// what runs is exactly what the user sees — and focus it.
    fn prefill_prompt(&mut self, prompt: &str) {
        self.input = TextArea::default();
        self.input.insert_str(prompt);
        self.focus = Focus::Input;
        self.toast("review the prompt, then Enter", ToastKind::Info);
    }

    // ── Which-key panel (`?`) ────────────────────────────────────────

    pub fn open_which_key(&mut self) {
        self.which_key.open = true;
        self.which_key.scroll = 0;
    }

    fn close_which_key(&mut self) {
        self.which_key.open = false;
    }

    /// Keys while the which-key panel is open. vim-style: j/k/↑↓ scroll,
    /// g/G jump, `?`/q/Esc/Ctrl-C close. Unknown keys are swallowed.
    fn handle_whichkey_key(&mut self, key: KeyEvent) {
        let close = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Char('Q')
            );
        if close {
            self.close_which_key();
            return;
        }
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.which_key.scroll = self.which_key.scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.which_key.scroll = self.which_key.scroll.saturating_sub(1)
            }
            KeyCode::PageDown => self.which_key.scroll = self.which_key.scroll.saturating_add(10),
            KeyCode::PageUp => self.which_key.scroll = self.which_key.scroll.saturating_sub(10),
            KeyCode::Home | KeyCode::Char('g') => self.which_key.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => {
                self.which_key.scroll = self.whichkey_max_scroll.get()
            }
            _ => {}
        }
        let max = self.whichkey_max_scroll.get();
        self.which_key.scroll = self.which_key.scroll.min(max);
    }

    // ── Theme picker ─────────────────────────────────────────────────

    /// Active theme (Copy). Clamps the index so it is always valid.
    pub fn theme(&self) -> theme::Theme {
        theme::get(self.theme_index)
    }

    pub fn open_theme_picker(&mut self) {
        // Start the cursor on the currently active theme.
        self.theme_picker.selected = self.theme_index.min(theme::THEMES.len() - 1);
        self.theme_picker.open = true;
    }

    fn close_theme_picker(&mut self) {
        self.theme_picker.open = false;
    }

    /// Keys while the theme picker is open: j/k/↑↓ move, Enter applies,
    /// Esc/Ctrl-C cancels (without changing the active theme).
    fn handle_theme_picker_key(&mut self, key: KeyEvent) {
        let cancel = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Esc;
        if cancel {
            self.close_theme_picker();
            return;
        }
        let last = theme::THEMES.len().saturating_sub(1);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.theme_picker.selected = self
                    .theme_picker
                    .selected
                    .min(last)
                    .saturating_add(1)
                    .min(last);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.theme_picker.selected = self.theme_picker.selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                self.theme_index = self.theme_picker.selected.min(last);
                self.close_theme_picker();
                self.toast(format!("theme: {}", self.theme().name), ToastKind::Ok);
            }
            _ => {}
        }
    }

    // ── Toasts ───────────────────────────────────────────────────────

    /// Start a fresh session: clear the transcript and reset the title.
    /// (PRISM has no Home route yet, so "back" maps to this.)
    pub fn new_session(&mut self) {
        self.messages.clear();
        self.session_title = "New session".to_string();
        self.goal = None;
        self.auto_scroll = true;
        self.focus = Focus::Input;
        self.push_system("[new session]");
        self.toast("new session", ToastKind::Info);
    }

    /// Push a transient, auto-dismissing toast (capped to the last 6).
    pub fn toast(&mut self, message: impl Into<String>, kind: ToastKind) {
        self.toasts.push(toast::Toast::new(message, kind));
        let overflow = self.toasts.len().saturating_sub(6);
        if overflow > 0 {
            self.toasts.drain(..overflow);
        }
    }

    /// Drop toasts whose TTL has elapsed. Called from the render tick.
    pub fn prune_toasts(&mut self) {
        self.toasts.retain(|t| !t.is_expired());
    }

    // ── GitHub panel ────────────────────────────────────────────────

    /// Open the GitHub panel and load the Issues tab.
    pub fn open_gh(&mut self) {
        self.gh.open = true;
        self.gh.tab = GhTab::Issues;
        self.gh.query.clear();
        self.gh.selected = 0;
        self.gh_load_tab(GhTab::Issues);
    }

    fn close_gh(&mut self) {
        self.gh.open = false;
    }

    /// Request a tab's data from the backend (`/gh <tab>`), marking it loading.
    fn gh_load_tab(&mut self, tab: GhTab) {
        self.gh.tab = tab;
        self.gh.loading = true;
        self.gh.items.clear();
        self.gh.error = None;
        self.gh.selected = 0;
        self.gh.query.clear();
        let _ = self.backend.send_command(&format!("/gh {}", tab.command()));
    }

    /// Keys while the GitHub panel is open.
    fn handle_gh_key(&mut self, key: KeyEvent) {
        let cancel = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Esc;
        if cancel {
            self.close_gh();
            return;
        }
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => self.gh_load_tab(self.gh.tab.prev()),
            KeyCode::Right | KeyCode::Char('l') => self.gh_load_tab(self.gh.tab.next()),
            KeyCode::Tab => self.gh_load_tab(self.gh.tab.next()),
            KeyCode::Char('1') => self.gh_load_tab(GhTab::Issues),
            KeyCode::Char('2') => self.gh_load_tab(GhTab::Prs),
            KeyCode::Char('3') => self.gh_load_tab(GhTab::Status),
            KeyCode::Down | KeyCode::Char('j') => {
                let n = gh::filtered_rows(&self.gh).len();
                if n > 0 {
                    self.gh.selected = self.gh.selected.min(n - 1).saturating_add(1).min(n - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.gh.selected = self.gh.selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                let rows = gh::filtered_rows(&self.gh);
                if let Some(row) = rows.get(self.gh.selected.min(rows.len().saturating_sub(1)))
                    && !row.url.is_empty()
                {
                    self.push_system(&format!("[gh] {}", row.url));
                    self.toast("link posted to chat", ToastKind::Info);
                }
            }
            KeyCode::Backspace => {
                self.gh.query.pop();
                self.gh.selected = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.gh.query.push(c);
                self.gh.selected = 0;
            }
            _ => {}
        }
    }

    // ── Model picker ────────────────────────────────────────────────

    /// Open the model picker and request the catalog (`/models list`).
    pub fn open_model_picker(&mut self) {
        self.model_picker.open = true;
        self.model_picker.loading = true;
        self.model_picker.query.clear();
        self.model_picker.selected = 0;
        let _ = self.backend.send_command("/models list");
    }

    fn close_model_picker(&mut self) {
        self.model_picker.open = false;
    }

    /// Indices of models matching the query (subsequence fuzzy), in order.
    pub fn model_filtered_indices(&self) -> Vec<usize> {
        let q = self.model_picker.query.trim().to_lowercase();
        let needle: Vec<char> = q.chars().collect();
        let mut matched: Vec<(String, String, usize)> = self
            .model_picker
            .models
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                if needle.is_empty() {
                    return true;
                }
                let hay = format!(
                    "{} {} {}",
                    m.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                    m.get("label").and_then(|v| v.as_str()).unwrap_or(""),
                    m.get("provider").and_then(|v| v.as_str()).unwrap_or("")
                )
                .to_lowercase();
                let mut mi = 0;
                for c in hay.chars() {
                    if mi < needle.len() && c == needle[mi].to_ascii_lowercase() {
                        mi += 1;
                    }
                }
                mi == needle.len()
            })
            .map(|(i, m)| {
                let provider = m
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let id = m
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                (provider, id, i)
            })
            .collect();
        // Group by provider (then id) so the picker renders provider sections.
        matched.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        matched.into_iter().map(|(_, _, i)| i).collect()
    }

    fn handle_model_picker_key(&mut self, key: KeyEvent) {
        let cancel = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Esc;
        if cancel {
            self.close_model_picker();
            return;
        }
        let indices = self.model_filtered_indices();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if !indices.is_empty() {
                    self.model_picker.selected =
                        (self.model_picker.selected + 1).min(indices.len() - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.model_picker.selected = self.model_picker.selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                if let Some(&idx) = indices.get(self.model_picker.selected.min(indices.len() - 1))
                    && let Some(id) = self
                        .model_picker
                        .models
                        .get(idx)
                        .and_then(|m| m.get("id"))
                        .and_then(|v| v.as_str())
                {
                    let id = id.to_string();
                    self.close_model_picker();
                    self.toast(format!("switching to {id}…"), ToastKind::Info);
                    let _ = self.backend.send_command(&format!("/model {id}"));
                }
            }
            KeyCode::Backspace => {
                self.model_picker.query.pop();
                self.model_picker.selected = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.model_picker.query.push(c);
                self.model_picker.selected = 0;
            }
            _ => {}
        }
    }

    // ── GPU picker ──────────────────────────────────────────────────

    /// Open the GPU picker and request the live catalog (`/gpus`).
    pub fn open_gpu_picker(&mut self) {
        self.gpu_picker.open = true;
        self.gpu_picker.loading = true;
        self.gpu_picker.selected = 0;
        let _ = self.backend.send_command("/gpus");
    }

    fn handle_gpu_picker_key(&mut self, key: KeyEvent) {
        let cancel = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Esc;
        if cancel {
            self.gpu_picker.open = false;
            return;
        }
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.gpu_picker.gpus.is_empty() {
                    self.gpu_picker.selected =
                        (self.gpu_picker.selected + 1).min(self.gpu_picker.gpus.len() - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.gpu_picker.selected = self.gpu_picker.selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                if let Some(gpu) = self.gpu_picker.gpus.get(self.gpu_picker.selected) {
                    let field = |key: &str| {
                        gpu.get(key)
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string()
                    };
                    let price = gpu
                        .get("price_per_hour_usd")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let prompt = format!(
                        "Provision a compute deployment on {} ({}, {}, ${price:.2}/hr). \
                         Estimate the cost first, then start it.",
                        field("gpu_type"),
                        field("provider"),
                        field("region"),
                    );
                    self.gpu_picker.open = false;
                    // Pre-fill the prompt (sci.* palette style): what runs
                    // is exactly what the user sees in the input box.
                    self.input = TextArea::default();
                    self.input.insert_str(&prompt);
                    self.focus = Focus::Input;
                    self.toast("review the prompt, then Enter", ToastKind::Info);
                }
            }
            _ => {}
        }
    }

    // ── Account (MARC27 login/logout) ───────────────────────────────

    /// Read `~/.prism/credentials.json` for the current login status.
    pub fn read_account_status() -> AccountStatus {
        let Some(home) = std::env::var_os("HOME") else {
            return AccountStatus::default();
        };
        let path = std::path::Path::new(&home).join(".prism/credentials.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return AccountStatus::default();
        };
        let Ok(creds) = serde_json::from_str::<Value>(&text) else {
            return AccountStatus::default();
        };
        let token = creds
            .get("access_token")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if token.is_empty() {
            return AccountStatus::default();
        }
        let g = |k: &str| {
            creds
                .get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        AccountStatus {
            logged_in: true,
            user: g("user_id"),
            org: g("org_id"),
            project: g("project_id"),
        }
    }

    pub fn open_account(&mut self) {
        self.account.status = Self::read_account_status();
        self.account.open = true;
        self.account.busy = false;
    }

    fn close_account(&mut self) {
        self.account.open = false;
    }

    fn account_action(&mut self, cmd: &str, label: &str) {
        let _ = self.backend.send_command(cmd);
        self.account.busy = true;
        self.toast(format!("{label}…"), ToastKind::Info);
    }

    fn handle_account_key(&mut self, key: KeyEvent) {
        let cancel = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Esc;
        if cancel {
            self.close_account();
            return;
        }
        if self.account.busy {
            return;
        }
        match key.code {
            KeyCode::Char('l') => {
                self.account_action("/login", "login (approve in browser)");
                self.close_account();
            }
            KeyCode::Char('o') => {
                self.account_action("/logout", "logging out");
                self.account.status = AccountStatus::default();
            }
            KeyCode::Char('r') => {
                self.account.status = Self::read_account_status();
                self.toast("status refreshed", ToastKind::Info);
            }
            _ => {}
        }
    }

    // ── Session picker (list / resume) ──────────────────────────────

    pub fn open_sessions(&mut self) {
        self.session_picker.open = true;
        self.session_picker.loading = true;
        self.session_picker.query.clear();
        self.session_picker.selected = 0;
        let _ = self.backend.send_command("/sessions");
    }

    fn close_sessions(&mut self) {
        self.session_picker.open = false;
    }

    pub fn session_filtered_indices(&self) -> Vec<usize> {
        let q = self.session_picker.query.trim().to_lowercase();
        let needle: Vec<char> = q.chars().collect();
        self.session_picker
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                if needle.is_empty() {
                    return true;
                }
                let hay = format!(
                    "{} {} {}",
                    s.get("session_id").and_then(|v| v.as_str()).unwrap_or(""),
                    s.get("model").and_then(|v| v.as_str()).unwrap_or(""),
                    s.get("turn_count").and_then(|v| v.as_u64()).unwrap_or(0)
                )
                .to_lowercase();
                let mut mi = 0;
                for c in hay.chars() {
                    if mi < needle.len() && c == needle[mi] {
                        mi += 1;
                    }
                }
                mi == needle.len()
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn handle_session_picker_key(&mut self, key: KeyEvent) {
        let cancel = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Esc;
        if cancel {
            self.close_sessions();
            return;
        }
        let indices = self.session_filtered_indices();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if !indices.is_empty() {
                    self.session_picker.selected =
                        (self.session_picker.selected + 1).min(indices.len() - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.session_picker.selected = self.session_picker.selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                if let Some(&idx) = indices.get(self.session_picker.selected.min(indices.len() - 1))
                    && let Some(id) = self
                        .session_picker
                        .sessions
                        .get(idx)
                        .and_then(|s| s.get("session_id"))
                        .and_then(|v| v.as_str())
                {
                    let id = id.to_string();
                    self.close_sessions();
                    self.toast(format!("resuming {id}…"), ToastKind::Info);
                    let _ = self.backend.send_command(&format!("/resume {id}"));
                }
            }
            KeyCode::Backspace => {
                self.session_picker.query.pop();
                self.session_picker.selected = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.session_picker.query.push(c);
                self.session_picker.selected = 0;
            }
            _ => {}
        }
    }

    // ── View panel (tabbed/scrollable results) ──────────────────────

    fn close_view(&mut self) {
        self.view.open = false;
    }

    fn handle_view_key(&mut self, key: KeyEvent) {
        let cancel = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Esc;
        if cancel {
            self.close_view();
            return;
        }
        let ntabs = self.view.tabs.len().max(1);
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.view.active_tab = self.view.active_tab.checked_sub(1).unwrap_or(ntabs - 1);
                self.view.scroll = 0;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                self.view.active_tab = (self.view.active_tab + 1) % ntabs;
                self.view.scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.view.scroll = self.view.scroll.saturating_add(1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.view.scroll = self.view.scroll.saturating_sub(1);
            }
            KeyCode::PageDown => {
                self.view.scroll = self.view.scroll.saturating_add(10);
            }
            KeyCode::PageUp => {
                self.view.scroll = self.view.scroll.saturating_sub(10);
            }
            _ => {}
        }
        let max = self.view.max_scroll.get();
        self.view.scroll = self.view.scroll.min(max);
    }

    // ── Tools window (bespoke) ──────────────────────────────────────

    pub fn open_tools_window(&mut self) {
        self.tools_window.open = true;
        self.tools_window.query.clear();
        self.tools_window.selected = 0;
        // Refresh the catalog in case it changed.
        let _ = self.backend.send_command("/tools");
    }

    fn close_tools_window(&mut self) {
        self.tools_window.open = false;
    }

    /// Filtered tool indices (matching the query), sorted by name.
    pub fn tools_window_filtered(&self) -> Vec<usize> {
        let q = self.tools_window.query.trim().to_lowercase();
        let needle: Vec<char> = q.chars().collect();
        let mut out: Vec<(String, usize)> = self
            .tool_catalog
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                if needle.is_empty() {
                    return true;
                }
                let hay = format!(
                    "{} {}",
                    t.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    t.get("description").and_then(|v| v.as_str()).unwrap_or("")
                )
                .to_lowercase();
                let mut mi = 0;
                for c in hay.chars() {
                    if mi < needle.len() && c == needle[mi] {
                        mi += 1;
                    }
                }
                mi == needle.len()
            })
            .map(|(i, t)| {
                (
                    t.get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    i,
                )
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out.into_iter().map(|(_, i)| i).collect()
    }

    fn handle_tools_window_key(&mut self, key: KeyEvent) {
        let cancel = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Esc;
        if cancel {
            self.close_tools_window();
            return;
        }
        let n = self.tools_window_filtered().len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if n > 0 {
                    self.tools_window.selected = (self.tools_window.selected + 1).min(n - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.tools_window.selected = self.tools_window.selected.saturating_sub(1);
            }
            KeyCode::Backspace => {
                self.tools_window.query.pop();
                self.tools_window.selected = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.tools_window.query.push(c);
                self.tools_window.selected = 0;
            }
            _ => {}
        }
    }

    // ── Status window (bespoke, from live state) ────────────────────

    pub fn open_status_window(&mut self) {
        self.status_window.open = true;
    }
    fn close_status_window(&mut self) {
        self.status_window.open = false;
    }
    fn handle_status_window_key(&mut self, key: KeyEvent) {
        let cancel = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Esc;
        if cancel {
            self.close_status_window();
        }
    }

    // ── Config window (bespoke file viewer) ─────────────────────────

    pub fn open_config_window(&mut self) {
        let mut files: Vec<(String, String)> = Vec::new();
        for (label, path) in [
            ("prism.toml", "./prism.toml".to_string()),
            (".mcp.json", "./.mcp.json".to_string()),
            (
                "~/.prism/config.toml",
                format!(
                    "{}/.prism/config.toml",
                    std::env::var("HOME").unwrap_or_default()
                ),
            ),
            (
                "~/.prism/credentials.json",
                format!(
                    "{}/.prism/credentials.json",
                    std::env::var("HOME").unwrap_or_default()
                ),
            ),
        ] {
            match std::fs::read_to_string(&path) {
                Ok(mut content) => {
                    if label.contains("credentials") {
                        content = Self::redact_credentials(&content);
                    }
                    files.push((label.to_string(), content));
                }
                Err(_) => files.push((label.to_string(), "(not found)".to_string())),
            }
        }
        self.config_window.files = files;
        self.config_window.active = 0;
        self.config_window.scroll = 0;
        self.config_window.open = true;
    }

    /// Mask token-like values in credentials JSON before display.
    fn redact_credentials(s: &str) -> String {
        let Ok(v) = serde_json::from_str::<Value>(s) else {
            return "(unreadable credentials)".to_string();
        };
        let mut v = v;
        for key in ["access_token", "refresh_token"] {
            if let Some(t) = v.get(key).and_then(|x| x.as_str())
                && t.len() > 8
            {
                v[key] = Value::String(format!("{}…", &t[..8]));
            }
        }
        serde_json::to_string_pretty(&v).unwrap_or_else(|_| "(unreadable)".into())
    }

    fn close_config_window(&mut self) {
        self.config_window.open = false;
    }

    fn handle_config_window_key(&mut self, key: KeyEvent) {
        let nfiles = self.config_window.files.len().max(1);
        let cancel = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Esc;
        if cancel {
            self.close_config_window();
            return;
        }
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.config_window.active = self
                    .config_window
                    .active
                    .checked_sub(1)
                    .unwrap_or(nfiles - 1);
                self.config_window.scroll = 0;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                self.config_window.active = (self.config_window.active + 1) % nfiles;
                self.config_window.scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.config_window.scroll = self.config_window.scroll.saturating_add(1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.config_window.scroll = self.config_window.scroll.saturating_sub(1);
            }
            KeyCode::PageDown => {
                self.config_window.scroll = self.config_window.scroll.saturating_add(10);
            }
            KeyCode::PageUp => {
                self.config_window.scroll = self.config_window.scroll.saturating_sub(10);
            }
            _ => {}
        }
        let max = self.config_window.max_scroll.get();
        self.config_window.scroll = self.config_window.scroll.min(max);
    }

    // ── API-key window ──────────────────────────────────────────────

    pub fn open_apikey_window(&mut self) {
        // Read current key status from ~/.prism/api_keys.json + env vars.
        self.apikey_window.status = API_PROVIDERS
            .iter()
            .map(|(_, env)| {
                let has = std::env::var(env).is_ok()
                    || Self::read_api_keys()
                        .and_then(|m| m.get(env).and_then(|v| v.as_str()).map(|s| !s.is_empty()))
                        .unwrap_or(false);
                (env.to_string(), has)
            })
            .collect();
        self.apikey_window.key_input.clear();
        self.apikey_window.provider_idx = 0;
        self.apikey_window.open = true;
    }

    fn close_apikey_window(&mut self) {
        self.apikey_window.open = false;
    }

    fn read_api_keys() -> Option<serde_json::Value> {
        let home = std::env::var("HOME").ok()?;
        serde_json::from_str(&std::fs::read_to_string(format!("{home}/.prism/api_keys.json")).ok()?)
            .ok()
    }

    fn save_api_key(env_var: &str, key: &str) -> Result<(), String> {
        let home = std::env::var("HOME").map_err(|_| "HOME not set")?;
        let path = format!("{home}/.prism/api_keys.json");
        let mut map: serde_json::Map<String, serde_json::Value> = Self::read_api_keys()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        map.insert(
            env_var.to_string(),
            serde_json::Value::String(key.to_string()),
        );
        let json = serde_json::to_string_pretty(&map).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    fn handle_apikey_key(&mut self, key: KeyEvent) {
        let cancel = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Esc;
        if cancel {
            self.close_apikey_window();
            return;
        }
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                let n = API_PROVIDERS.len();
                self.apikey_window.provider_idx = self
                    .apikey_window
                    .provider_idx
                    .checked_sub(1)
                    .unwrap_or(n - 1);
                self.apikey_window.key_input.clear();
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.apikey_window.provider_idx =
                    (self.apikey_window.provider_idx + 1) % API_PROVIDERS.len();
                self.apikey_window.key_input.clear();
            }
            KeyCode::Enter => {
                let (_, env_var) = API_PROVIDERS[self.apikey_window.provider_idx];
                let key_val = self.apikey_window.key_input.trim().to_string();
                if key_val.is_empty() {
                    self.toast("enter a key first", ToastKind::Warn);
                } else {
                    match Self::save_api_key(env_var, &key_val) {
                        Ok(()) => {
                            // Update status.
                            if let Some(idx) = self
                                .apikey_window
                                .status
                                .iter()
                                .position(|(e, _)| e == env_var)
                            {
                                self.apikey_window.status[idx].1 = true;
                            }
                            self.toast(format!("saved {env_var}"), ToastKind::Ok);
                            self.apikey_window.key_input.clear();
                        }
                        Err(e) => self.toast(format!("save failed: {e}"), ToastKind::Err),
                    }
                }
            }
            KeyCode::Backspace => {
                self.apikey_window.key_input.pop();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.apikey_window.key_input.push(c);
            }
            _ => {}
        }
    }

    // ── Link picker (`o`) ───────────────────────────────────────────

    /// Collect http(s) URLs from the transcript (newest message first)
    /// and open the picker. With exactly one URL the picker goes straight
    /// to the confirm dialog — opening a browser always asks first.
    pub fn open_link_picker(&mut self) {
        let mut urls: Vec<String> = Vec::new();
        for m in self.messages.iter().rev() {
            for url in crate::markdown::extract_urls(&m.text) {
                if !urls.contains(&url) {
                    urls.push(url);
                }
            }
            if urls.len() >= 20 {
                break;
            }
        }
        urls.truncate(20);
        if urls.is_empty() {
            self.toast("no links in the transcript", ToastKind::Info);
            return;
        }
        self.link_picker.confirm = urls.len() == 1;
        self.link_picker.urls = urls;
        self.link_picker.selected = 0;
        self.link_picker.open = true;
    }

    fn handle_link_picker_key(&mut self, key: KeyEvent) {
        let cancel = (key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c'))
            || key.code == KeyCode::Esc;

        // Confirm dialog: Enter/y opens the URL, Esc/n backs out.
        if self.link_picker.confirm {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.open_selected_link();
                }
                _ if cancel || matches!(key.code, KeyCode::Char('n') | KeyCode::Char('N')) => {
                    if self.link_picker.urls.len() > 1 {
                        self.link_picker.confirm = false;
                    } else {
                        self.link_picker.open = false;
                    }
                }
                _ => {}
            }
            return;
        }

        if cancel || key.code == KeyCode::Char('q') {
            self.link_picker.open = false;
            return;
        }
        let n = self.link_picker.urls.len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if n > 0 {
                    self.link_picker.selected = (self.link_picker.selected + 1).min(n - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.link_picker.selected = self.link_picker.selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                if n > 0 {
                    self.link_picker.confirm = true;
                }
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as usize) - ('1' as usize);
                if idx < n {
                    self.link_picker.selected = idx;
                    self.link_picker.confirm = true;
                }
            }
            _ => {}
        }
    }

    /// Open `urls[selected]` in the system browser (detached) and close.
    fn open_selected_link(&mut self) {
        if let Some(url) = self
            .link_picker
            .urls
            .get(self.link_picker.selected)
            .cloned()
        {
            match open_url_detached(&url) {
                Ok(()) => self.toast(format!("opening {url}"), ToastKind::Ok),
                Err(e) => self.toast(format!("open failed: {e}"), ToastKind::Err),
            }
        }
        self.link_picker.open = false;
    }

    /// Execute a catalog command by id. Reuses existing action paths so
    /// the palette, slash-commands, and keybinds stay in sync.
    fn dispatch_command(&mut self, id: &str) {
        self.close_palette();
        match id {
            // Science entries pre-fill the prompt with a scaffold that
            // steers the agent to the right tools — the user finishes the
            // sentence and hits Enter. No hidden magic: what runs is
            // exactly what they see in the input box.
            "sci.properties" | "sci.simulate" | "sci.predict" => {
                let scaffold = match id {
                    "sci.properties" => {
                        "What are the key properties (structure, lattice constant, \
                         moduli, band gap) of "
                    }
                    "sci.simulate" => "Run a MACE/pyiron simulation for this material: ",
                    _ => "Predict material properties for this composition: ",
                };
                self.input = TextArea::default();
                self.input.insert_str(scaffold);
                self.focus = Focus::Input;
                self.toast("finish the prompt, then Enter", ToastKind::Info);
            }
            "sci.research" => self.open_research_form(),
            // One Knowledge flow; the old search/ingest verbs stay as
            // aliases that land on the right tab (muscle memory).
            "knowledge.open" | "sci.search" => self.open_knowledge_pane(KnowledgeTab::Search),
            "sci.ingest" => self.open_knowledge_pane(KnowledgeTab::Ingest),
            "sci.notebook" => {
                self.toast(
                    "notebooks run via CLI: prism notebook start",
                    ToastKind::Info,
                );
            }
            "help.show" => self.modal = Some(Modal::Help),
            "which_key.show" => self.open_which_key(),
            "theme.list" => self.open_theme_picker(),
            "gh.show" => self.open_gh(),
            "account.show" => self.open_account(),
            "sessions.show" => self.open_sessions(),
            "tools.show" => self.open_tools_window(),
            "status.show" => self.open_status_window(),
            "config.show" => self.open_config_window(),
            "apikey.show" => self.open_apikey_window(),
            "session.new" => self.new_session(),
            "links.open" => self.open_link_picker(),
            "cost.show" => self.modal = Some(Modal::Cost),
            "model.show" => self.open_model_picker(),
            "compute.gpus" => self.open_gpu_picker(),
            "mcp.show" => self.modal = Some(Modal::Tools),
            "goal.set" => self.open_goal_form(),
            "chat.clear" => {
                self.messages.clear();
                self.toast("chat cleared", ToastKind::Info);
            }
            "app.exit" => self.should_quit = true,
            "thinking.toggle" => {
                self.thinking_expanded = !self.thinking_expanded;
                self.toast(
                    if self.thinking_expanded {
                        "thinking: shown"
                    } else {
                        "thinking: hidden"
                    },
                    ToastKind::Info,
                );
            }
            "metrics.toggle" => {
                self.show_metrics = !self.show_metrics;
                self.toast(
                    if self.show_metrics {
                        "metrics: on"
                    } else {
                        "metrics: off"
                    },
                    ToastKind::Info,
                );
            }
            "cost.toggle" => {
                self.show_cost = !self.show_cost;
                self.toast(
                    if self.show_cost {
                        "cost bar: on"
                    } else {
                        "cost bar: off"
                    },
                    ToastKind::Info,
                );
            }
            "input.focus" => self.focus = Focus::Input,
            "workspace.activity" => {
                self.workspace_tab = WorkspaceTab::Activity;
                self.workspace_selected = 0;
                self.workspace_expanded = false;
                self.focus = Focus::Workspace;
            }
            "workspace.tools" => {
                self.workspace_tab = WorkspaceTab::Tools;
                self.workspace_selected = 0;
                self.workspace_expanded = false;
                self.focus = Focus::Workspace;
            }
            "workspace.files" => {
                self.workspace_tab = WorkspaceTab::Files;
                self.workspace_selected = 0;
                self.workspace_expanded = false;
                self.focus = Focus::Workspace;
            }
            other if other.starts_with("slash.") => {
                // Run any backend slash command, e.g. "slash.tools" → "/tools".
                // No chat echo — the returned `ui.view` panel is the feedback.
                let root = &other["slash.".len()..];
                let cmd = format!("/{root}");
                let _ = self.backend.send_command(&cmd);
            }
            _ => {}
        }
    }

    fn send_message(&mut self, text: &str) {
        let trimmed = text.trim();

        // Client-side commands — handled in the TUI, never sent to the backend.
        match trimmed {
            "/help" | "/keys" | "/?" => {
                self.modal = Some(Modal::Help);
                return;
            }
            "/cost" => {
                self.modal = Some(Modal::Cost);
                return;
            }
            "/model" => {
                self.modal = Some(Modal::Model);
                return;
            }
            "/mcp" | "/setup" => {
                self.modal = Some(Modal::Tools);
                return;
            }
            _ => {}
        }

        // `/goal [text]` sets (or clears) the session goal shown in the sidebar.
        if trimmed == "/goal" || trimmed.starts_with("/goal ") {
            let g = trimmed["/goal".len()..].trim();
            if g.is_empty() {
                self.goal = None;
                self.push_system("[goal cleared]");
            } else {
                self.goal = Some(g.to_string());
                self.push_system(&format!("[goal set — {g}]"));
            }
            return;
        }

        self.push_user(trimmed);

        // Derive a session title from the first real user message (opencode-style),
        // since the backend doesn't send one. Slash commands don't count.
        if self.session_title == "New session" && !trimmed.starts_with('/') {
            self.session_title = title_from_message(trimmed);
        }

        if trimmed.starts_with('/') {
            let _ = self.backend.send_command(trimmed);
        } else {
            // Inject the standing goal so it actually steers the agent. The
            // chat shows the user's clean text; the backend receives it with
            // the goal prefixed as context on every turn (survives compaction).
            let payload = match &self.goal {
                Some(goal) => format!("[Standing goal: {goal}]\n\n{trimmed}"),
                None => trimmed.to_string(),
            };
            let _ = self.backend.send_message(&payload);
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
                    "PRISM ready — {} tools available",
                    self.tool_count
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
                // Populate the session picker; open it if not already.
                self.session_picker.sessions = sessions;
                self.session_picker.loading = false;
                self.session_picker.selected = 0;
                self.session_picker.query.clear();
                if self.session_picker.sessions.is_empty() {
                    self.toast("no saved sessions", ToastKind::Info);
                    self.session_picker.open = false;
                } else if !self.session_picker.open {
                    self.session_picker.open = true;
                }
            }
            AgentMsg::GhData {
                tab,
                repo,
                items,
                error,
            } => {
                // Populate the GitHub panel. If the panel isn't open, open it so
                // the data is visible (e.g., a `/gh` slash command from the input).
                self.gh.repo = repo;
                self.gh.error = error.clone();
                self.gh.loading = false;
                self.gh.selected = 0;
                self.gh.query.clear();
                if let Some(t) = match tab.as_str() {
                    "issues" => Some(GhTab::Issues),
                    "prs" => Some(GhTab::Prs),
                    "status" => Some(GhTab::Status),
                    _ => None,
                } {
                    self.gh.tab = t;
                }
                self.gh.items = items;
                if !self.gh.open {
                    self.gh.open = true;
                }
                if let Some(err) = error {
                    self.toast(format!("gh: {err}"), ToastKind::Warn);
                }
            }
            AgentMsg::ModelList { models, current } => {
                // Populate the model picker; open it if not already (so a
                // `/models list` from the input also surfaces the picker).
                self.model_picker.models = models;
                self.model_picker.current = current;
                self.model_picker.loading = false;
                self.model_picker.selected = 0;
                self.model_picker.query.clear();
                if !self.model_picker.open {
                    self.model_picker.open = true;
                }
            }
            AgentMsg::GpuList { gpus, error } => {
                // Populate the GPU picker; open it if not already (so a
                // `/gpus` from the input also surfaces the picker).
                self.gpu_picker.gpus = gpus;
                self.gpu_picker.loading = false;
                self.gpu_picker.selected = 0;
                if !self.gpu_picker.open {
                    self.gpu_picker.open = true;
                }
                if let Some(err) = error {
                    self.toast(format!("gpus: {err}"), ToastKind::Warn);
                }
            }
            AgentMsg::ToolsCatalog { tools } => {
                // Live tool catalog for the sidebar Tools tab.
                self.tool_catalog = tools;
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
                // Sanitize tool_name and verb before formatting —
                // both come from the backend and could contain
                // control sequences.
                let clean_verb = sanitize_for_render(&verb);
                let clean_name = sanitize_for_render(&tool_name);
                // The backend sends a full humanized verb ("Searching the
                // web — …") which is displayed verbatim. Only a bare/legacy
                // "Running" verb gets the tool name appended — appending it
                // unconditionally produced lines like "Running web web".
                let text = if clean_verb.is_empty() || clean_verb == "Running" {
                    format!("Running {clean_name}")
                } else {
                    clean_verb
                };
                self.push_message(ChatLine {
                    role: Role::Tool,
                    text,
                    kind: LineKind::ToolStart {
                        tool_name: clean_name,
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
                // Sanitize tool_name and content before storing.
                let clean_name = sanitize_for_render(&tool_name);
                let clean_content = sanitize_for_render(&content);
                let success = card_type != "error";
                let elapsed = elapsed_ms.unwrap_or(0);
                if !success {
                    self.push_message(ChatLine {
                        role: Role::Tool,
                        text: format!("{}: {}", clean_name, clean_content),
                        kind: LineKind::Error(format!("{}: {}", clean_name, clean_content)),
                    });
                } else {
                    self.push_message(ChatLine {
                        role: Role::Tool,
                        text: format!("{}: {}", clean_name, clean_content),
                        kind: LineKind::ToolResult {
                            tool_name: clean_name,
                            content: clean_content,
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
                // Sanitize both before storing in approval_pending and
                // the ChatLine.
                let clean_name = sanitize_for_render(&tool_name);
                let clean_msg = sanitize_for_render(&message);
                self.approval_pending = Some((clean_name.clone(), clean_msg.clone()));
                self.focus = Focus::Approval;
                self.push_message(ChatLine {
                    role: Role::System,
                    text: format!("{}: {}", clean_name, clean_msg),
                    kind: LineKind::Approval {
                        tool_name: clean_name,
                        message: clean_msg,
                    },
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
                // A login/logout turn just finished — refresh account status.
                if self.account.busy {
                    self.account.busy = false;
                    self.account.status = Self::read_account_status();
                }
            }
            AgentMsg::View { title, tabs } => {
                // Render view results (tools/status/context/…) as a tabbed,
                // scrollable panel rather than a flat chat dump.
                let clean_title = sanitize_for_render(&title);
                let clean_tabs: Vec<(String, String)> = tabs
                    .into_iter()
                    .map(|(t, b)| (sanitize_for_render(&t), sanitize_for_render(&b)))
                    .collect();
                self.view.title = clean_title;
                self.view.tabs = if clean_tabs.is_empty() {
                    vec![("".to_string(), String::new())]
                } else {
                    clean_tabs
                };
                self.view.active_tab = 0;
                self.view.scroll = 0;
                self.view.open = true;
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
    //
    // Sanitize at TUI state ingress so render remains pure and never
    // receives raw terminal control sequences.  Every visible string
    // that enters a `ChatLine` passes through `sanitize_for_render`
    // here, at the lowest level — callers don't need to sanitize
    // again.

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
        let clean = sanitize_for_render(text);
        self.push_message(ChatLine {
            role: Role::User,
            text: clean.clone(),
            kind: LineKind::Text,
        });
    }

    pub fn push_system(&mut self, text: &str) {
        let clean = sanitize_for_render(text);
        self.push_message(ChatLine {
            role: Role::System,
            text: clean.clone(),
            kind: LineKind::Status(clean),
        });
    }

    pub fn push_error(&mut self, text: &str) {
        let clean = sanitize_for_render(text);
        self.push_message(ChatLine {
            role: Role::System,
            text: clean.clone(),
            kind: LineKind::Error(clean),
        });
    }

    pub fn append_assistant_text(&mut self, delta: &str) {
        let clean = sanitize_for_render(delta);
        if let Some(last) = self.messages.last_mut()
            && matches!(last.role, Role::Assistant)
            && matches!(last.kind, LineKind::Text)
        {
            last.text.push_str(&clean);
            return;
        }
        self.messages.push(ChatLine {
            role: Role::Assistant,
            text: clean,
            kind: LineKind::Text,
        });
    }

    /// Append thinking/reasoning tokens to a separate thinking buffer.
    /// Rendered dimmed and collapsible.
    pub fn append_thinking_text(&mut self, delta: &str) {
        let clean = sanitize_for_render(delta);
        if let Some(last) = self.messages.last_mut()
            && matches!(last.role, Role::Assistant)
            && matches!(last.kind, LineKind::Thinking)
        {
            last.text.push_str(&clean);
            return;
        }
        self.messages.push(ChatLine {
            role: Role::Assistant,
            text: clean,
            kind: LineKind::Thinking,
        });
    }
}

/// Path of the per-tool config file: `~/.prism/tools.d/<tool>.toml`.
fn tool_config_path(name: &str) -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::Path::new(&home)
        .join(".prism/tools.d")
        .join(format!("{name}.toml"))
}

/// Display a path with the home directory shortened to `~`.
pub(crate) fn tilde_path(path: &std::path::Path) -> String {
    let s = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && s.starts_with(&home) => format!("~{}", &s[home.len()..]),
        _ => s,
    }
}

/// Body of the Tools-tab detail modal, from a `ui.tools.catalog` entry.
/// Only fields the backend actually sent are shown; the per-tool config
/// file is shown with its path and contents when it exists.
fn tool_detail_body(tool: &Value) -> String {
    let text_field = |k: &str| {
        tool.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let name = {
        let n = text_field("name");
        if n.is_empty() { "?".to_string() } else { n }
    };
    let mut out = String::new();
    out.push_str(&format!("Name       {name}\n"));
    let source = text_field("source");
    if !source.is_empty() {
        out.push_str(&format!("Source     {source}\n"));
    }
    out.push_str(&format!(
        "Approval   {}\n",
        match tool.get("approval").and_then(|v| v.as_bool()) {
            Some(true) => "requires approval",
            Some(false) => "auto-approved",
            None => "unknown",
        }
    ));
    let desc = text_field("description");
    if !desc.is_empty() {
        out.push_str("\nDescription\n");
        for l in desc.lines() {
            out.push_str(&format!("  {l}\n"));
        }
    }
    for key in ["schema", "input_schema", "parameters"] {
        if let Some(schema) = tool.get(key).filter(|v| !v.is_null()) {
            out.push_str("\nSchema\n");
            out.push_str(&serde_json::to_string_pretty(schema).unwrap_or_default());
            out.push('\n');
            break;
        }
    }
    let cfg = tool_config_path(&name);
    out.push_str(&format!("\nConfig     {}\n", tilde_path(&cfg)));
    match std::fs::read_to_string(&cfg) {
        Ok(content) => {
            for l in content.lines() {
                out.push_str(&format!("  {l}\n"));
            }
            out.push_str("\n  (edit the file and restart prism to apply)\n");
        }
        Err(_) => {
            out.push_str("  (not found — create it to override this tool's settings)\n");
        }
    }
    out
}

/// Read a file for the Files-tab detail modal: text only, 200 KB cap.
fn read_file_capped(path: &str) -> String {
    const MAX_BYTES: u64 = 200 * 1024;
    match std::fs::metadata(path) {
        Err(e) => format!("(cannot read {path}: {e})"),
        Ok(md) if md.len() > MAX_BYTES => format!(
            "(file too large to preview: {} bytes — cap is 200 KB)",
            md.len()
        ),
        Ok(_) => std::fs::read_to_string(path)
            .unwrap_or_else(|e| format!("(cannot read {path}: {e} — binary files not previewed)")),
    }
}

/// The underlying event of an Activity row as JSON, for the detail modal.
fn chatline_detail_json(m: &ChatLine) -> Value {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "tool",
    };
    let mut v = serde_json::json!({ "role": role, "text": m.text });
    match &m.kind {
        LineKind::Text => v["event"] = "text".into(),
        LineKind::Thinking => v["event"] = "thinking".into(),
        LineKind::Status(_) => v["event"] = "status".into(),
        LineKind::Error(e) => {
            v["event"] = "error".into();
            v["error"] = e.clone().into();
        }
        LineKind::ToolStart {
            tool_name,
            elapsed_ms,
        } => {
            v["event"] = "tool_start".into();
            v["tool_name"] = tool_name.clone().into();
            if let Some(ms) = elapsed_ms {
                v["elapsed_ms"] = (*ms).into();
            }
        }
        LineKind::ToolResult {
            tool_name,
            content,
            elapsed_ms,
            success,
        } => {
            v["event"] = "tool_result".into();
            v["tool_name"] = tool_name.clone().into();
            v["content"] = content.clone().into();
            v["elapsed_ms"] = (*elapsed_ms).into();
            v["success"] = (*success).into();
        }
        LineKind::Approval { tool_name, message } => {
            v["event"] = "approval".into();
            v["tool_name"] = tool_name.clone().into();
            v["message"] = message.clone().into();
        }
        LineKind::View { title, body } => {
            v["event"] = "view".into();
            v["title"] = title.clone().into();
            v["body"] = body.clone().into();
        }
    }
    v
}

/// Open a URL with the platform opener (`open` on macOS, `xdg-open` on
/// Linux), fully detached with null stdio — the UI never blocks on it.
/// Only http(s) URLs reach this (see [`crate::markdown::extract_urls`])
/// and the URL is passed as a single argument, never through a shell.
fn open_url_detached(url: &str) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(url);
        c
    } else if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Compose the exact chat instruction the research form launches.
///
/// The engine contract is `{question, depth}` (app/tools/agent_runs.py),
/// so the Web toggle maps onto depth (off → 0) and the remaining source
/// preferences ride inside the question text — that string IS recorded
/// server-side with the run; there is no separate params object on this
/// path, and unenforced sources are labeled advisory rather than
/// pretending server-side filtering exists.
fn research_prompt(form: &crate::form::Form) -> String {
    let question = form.text_value("question").trim().to_string();
    let web = form.toggle_value("src_web");
    let depth = if web {
        form.stepper_value("depth").max(1)
    } else {
        0
    };
    let mut sources: Vec<&str> = Vec::new();
    if form.toggle_value("src_kg") {
        sources.push("knowledge graph");
    }
    if web {
        sources.push("web");
    }
    let mut advisory: Vec<&str> = Vec::new();
    if form.toggle_value("src_prov") {
        advisory.push("provenance/memory");
    }
    if form.toggle_value("src_mesh") {
        advisory.push("mesh/partner data");
    }
    let mut source_note = String::new();
    if !sources.is_empty() {
        source_note.push_str(&format!(" [data sources: {}", sources.join(", ")));
        if !advisory.is_empty() {
            source_note.push_str(&format!("; advisory: {}", advisory.join(", ")));
        }
        source_note.push(']');
    } else if !advisory.is_empty() {
        source_note.push_str(&format!(" [advisory sources: {}]", advisory.join(", ")));
    }
    format!(
        "Launch deep background research with start_background_research \
         (depth {depth}): \"{question}{source_note}\". Report the run_id, \
         keep helping me meanwhile, and check with check_background_research \
         when I ask."
    )
}

/// Derive a short session title from the first user message (opencode-style):
/// first line, trimmed, capped to ~48 chars.
fn title_from_message(msg: &str) -> String {
    let first = msg.lines().next().unwrap_or(msg).trim();
    let chars: Vec<char> = first.chars().take(48).collect();
    let mut s: String = chars.into_iter().collect();
    if first.chars().count() > 48 {
        s.push('…');
    }
    if s.is_empty() {
        "New session".to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::FakeScenario;

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn fresh() -> App {
        App::new(BackendHandle::fake(FakeScenario::BasicChat))
    }

    #[test]
    fn ctrl_p_opens_the_palette() {
        let mut app = fresh();
        assert!(!app.palette.open);
        app.handle_key(ctrl('p'));
        assert!(app.palette.open, "Ctrl-P must open the command palette");
    }

    #[test]
    fn ctrl_c_inside_palette_cancels_without_quitting() {
        let mut app = fresh();
        app.open_palette();
        app.handle_key(ctrl('c'));
        assert!(!app.palette.open, "Ctrl-C must close the palette");
        assert!(
            !app.should_quit,
            "Ctrl-C inside the palette must NOT quit the app"
        );
    }

    #[test]
    fn ctrl_c_outside_palette_quits() {
        let mut app = fresh();
        app.handle_key(ctrl('c'));
        assert!(app.should_quit, "Ctrl-C outside the palette must quit");
    }

    #[test]
    fn palette_cannot_open_during_approval() {
        let mut app = fresh();
        app.approval_pending = Some(("compute_submit".into(), "Allow?".into()));
        app.handle_key(ctrl('p'));
        assert!(
            !app.palette.open,
            "palette must not open while an approval is pending"
        );
    }

    #[test]
    fn palette_enter_dispatches_first_command() {
        let mut app = fresh();
        app.open_palette();
        // Filter to "help" so dispatch is order-independent → help.show.
        for c in "help".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.modal, Some(Modal::Help));
        assert!(!app.palette.open, "dispatch must close the palette");
    }

    #[test]
    fn palette_can_open_gh_panel() {
        let mut app = fresh();
        app.open_palette();
        for c in "github".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
        assert!(app.gh.open, "the GitHub command must open the panel");
        assert!(app.gh.loading, "opening must request data from the backend");
    }

    #[test]
    fn palette_typing_then_enter_dispatches_match() {
        let mut app = fresh();
        app.open_palette();
        for c in "quit".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        // "quit" filters to app.exit at the top; Enter runs it.
        app.handle_key(key(KeyCode::Enter));
        assert!(app.should_quit, "selecting 'Quit' must exit");
    }

    #[test]
    fn palette_esc_closes_without_dispatch() {
        let mut app = fresh();
        app.open_palette();
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.palette.open);
        assert_eq!(app.modal, None, "Esc must not dispatch a command");
    }

    fn qmark() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT)
    }

    #[test]
    fn qmark_opens_which_key_from_chat_focus() {
        let mut app = fresh();
        app.focus = Focus::Chat;
        app.handle_key(qmark());
        assert!(
            app.which_key.open,
            "? must open the which-key panel from chat focus"
        );
    }

    #[test]
    fn qmark_does_not_open_from_input_focus() {
        // In input focus `?` must type, not open the panel.
        let mut app = fresh();
        app.focus = Focus::Input;
        app.handle_key(qmark());
        assert!(!app.which_key.open, "? must not steal the key while typing");
    }

    #[test]
    fn ctrl_c_inside_which_key_cancels_without_quitting() {
        let mut app = fresh();
        app.open_which_key();
        app.handle_key(ctrl('c'));
        assert!(!app.which_key.open, "Ctrl-C must close the which-key panel");
        assert!(!app.should_quit, "Ctrl-C inside the panel must NOT quit");
    }

    #[test]
    fn which_key_jk_scrolls_and_clamps() {
        let mut app = fresh();
        app.open_which_key();
        // max_scroll is set by the renderer; simulate a tall panel.
        app.whichkey_max_scroll.set(5);
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.which_key.scroll, 3);
        // Overscroll clamps to max.
        for _ in 0..10 {
            app.handle_key(key(KeyCode::Char('j')));
        }
        assert_eq!(app.which_key.scroll, 5, "scroll must clamp at max");
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.which_key.scroll, 4);
    }

    #[test]
    fn palette_can_open_which_key() {
        let mut app = fresh();
        app.open_palette();
        app.palette.query = "keyb".into();
        app.handle_key(key(KeyCode::Enter));
        assert!(
            app.which_key.open,
            "the 'Keybindings' command must open the panel"
        );
    }

    #[test]
    fn palette_dispatch_theme_opens_picker() {
        let mut app = fresh();
        app.open_palette();
        app.palette.query = "theme".into();
        app.handle_key(key(KeyCode::Enter));
        assert!(
            app.theme_picker.open,
            "theme.list must open the theme picker"
        );
        assert!(!app.palette.open, "palette must close after dispatch");
    }

    #[test]
    fn theme_picker_enter_applies_and_closes() {
        let mut app = fresh();
        app.open_theme_picker();
        // THEMES = [opencode, prism, midnight, forest, ...]. Down 3 → forest.
        app.handle_theme_picker_key(key(KeyCode::Down));
        app.handle_theme_picker_key(key(KeyCode::Down));
        app.handle_theme_picker_key(key(KeyCode::Down));
        app.handle_theme_picker_key(key(KeyCode::Enter));
        assert!(!app.theme_picker.open, "Enter must close the picker");
        assert_eq!(app.theme_index, 3, "Enter must apply the selected theme");
        assert_eq!(app.theme().name, "forest");
    }

    #[test]
    fn toasts_cap_and_prune() {
        let mut app = fresh();
        for i in 0..20 {
            app.toast(format!("t{i}"), ToastKind::Info);
        }
        assert!(
            app.toasts.len() <= 6,
            "toasts must cap at 6, got {}",
            app.toasts.len()
        );

        // A zero-TTL toast is expired immediately and gets pruned.
        app.toasts.push(toast::Toast {
            message: "expire".into(),
            kind: ToastKind::Warn,
            created_at: std::time::Instant::now(),
            ttl: std::time::Duration::ZERO,
        });
        let before = app.toasts.len();
        app.prune_toasts();
        assert_eq!(app.toasts.len(), before - 1, "expired toast must be pruned");
    }

    #[test]
    fn toggle_dispatch_emits_toast() {
        let mut app = fresh();
        app.dispatch_command("metrics.toggle");
        assert_eq!(app.toasts.len(), 1);
        assert!(app.toasts[0].message.contains("metrics"));
    }

    // ── Form pane ────────────────────────────────────────────────────

    #[test]
    fn goal_set_dispatch_opens_form() {
        let mut app = fresh();
        app.dispatch_command("goal.set");
        assert!(app.form.is_some(), "goal.set must open the goal form");
        let pane = app.form.as_ref().unwrap();
        assert_eq!(pane.target, FormTarget::Goal);
        assert_eq!(pane.form.title, "Set goal");
    }

    #[test]
    fn goal_form_submit_sets_goal() {
        let mut app = fresh();
        app.open_goal_form();
        for c in "beat Vegard's law".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
        assert!(app.form.is_none(), "submit must close the form");
        assert_eq!(app.goal.as_deref(), Some("beat Vegard's law"));
    }

    #[test]
    fn goal_form_empty_submit_clears_goal() {
        let mut app = fresh();
        app.goal = Some("old goal".into());
        app.open_goal_form();
        // Wipe the pre-filled value, then submit empty.
        for _ in 0.."old goal".len() {
            app.handle_key(key(KeyCode::Backspace));
        }
        app.handle_key(key(KeyCode::Enter));
        assert!(app.form.is_none());
        assert_eq!(app.goal, None, "empty submit must clear the goal");
    }

    #[test]
    fn form_esc_cancels_without_side_effects() {
        let mut app = fresh();
        app.goal = Some("keep me".into());
        app.open_goal_form();
        for c in "scratch".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Esc));
        assert!(app.form.is_none(), "Esc must close the form");
        assert_eq!(
            app.goal.as_deref(),
            Some("keep me"),
            "cancel must not apply"
        );
        assert!(!app.should_quit, "Esc in a form must not quit");
    }

    #[test]
    fn ctrl_c_inside_form_cancels_without_quitting() {
        let mut app = fresh();
        app.open_goal_form();
        app.handle_key(ctrl('c'));
        assert!(app.form.is_none(), "Ctrl-C must close the form");
        assert!(!app.should_quit, "Ctrl-C inside a form must NOT quit");
    }

    // ── Deep research pane ───────────────────────────────────────────

    #[test]
    fn research_dispatch_opens_form_not_scaffold() {
        let mut app = fresh();
        app.dispatch_command("sci.research");
        let pane = app.form.as_ref().expect("sci.research must open a form");
        assert_eq!(pane.target, FormTarget::Research);
        let names: Vec<&str> = pane.form.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "question", "depth", "src_web", "src_kg", "src_prov", "src_mesh"
            ]
        );
        assert!(
            app.input.lines().join("").is_empty(),
            "opening the pane must not pre-fill the input"
        );
    }

    #[test]
    fn research_submit_requires_question() {
        let mut app = fresh();
        app.open_research_form();
        app.handle_key(key(KeyCode::Enter));
        assert!(app.form.is_some(), "empty question must keep the pane open");
        assert!(
            app.toasts.iter().any(|t| t.message.contains("question")),
            "must explain what's missing"
        );
    }

    #[test]
    fn research_submit_prefills_exact_instruction() {
        let mut app = fresh();
        app.open_research_form();
        for c in "NiTi shape memory".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        // depth 1 → 2.
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Right));
        app.handle_key(key(KeyCode::Enter));
        assert!(app.form.is_none(), "submit must close the pane");
        assert_eq!(app.focus, Focus::Input);
        let prompt = app.input.lines().join("\n");
        assert!(
            prompt.contains("start_background_research"),
            "must route through the existing background-research tool: {prompt}"
        );
        assert!(
            prompt.contains("depth 2"),
            "depth must be honored: {prompt}"
        );
        assert!(prompt.contains("NiTi shape memory"));
        assert!(
            prompt.contains("knowledge graph, web"),
            "default sources must be recorded in the question: {prompt}"
        );
    }

    #[test]
    fn research_web_off_forces_depth_zero() {
        let mut app = fresh();
        app.open_research_form();
        for c in "local only".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        // Move to src_web (question → depth → src_web) and toggle off.
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Char(' ')));
        app.handle_key(key(KeyCode::Enter));
        let prompt = app.input.lines().join("\n");
        assert!(
            prompt.contains("depth 0"),
            "web off must force the local-only depth: {prompt}"
        );
        assert!(!prompt.contains("web]"), "web must not be listed: {prompt}");
    }

    // ── Knowledge pane ───────────────────────────────────────────────

    #[test]
    fn knowledge_open_and_aliases_land_on_right_tab() {
        let mut app = fresh();
        app.dispatch_command("knowledge.open");
        assert!(app.knowledge.open);
        assert_eq!(app.knowledge.active_tab(), KnowledgeTab::Search);

        let mut app = fresh();
        app.dispatch_command("sci.search");
        assert!(app.knowledge.open, "sci.search must alias the pane");
        assert_eq!(app.knowledge.active_tab(), KnowledgeTab::Search);

        let mut app = fresh();
        app.dispatch_command("sci.ingest");
        assert!(app.knowledge.open, "sci.ingest must alias the pane");
        assert_eq!(app.knowledge.active_tab(), KnowledgeTab::Ingest);
    }

    #[test]
    fn knowledge_tab_key_switches_modes() {
        let mut app = fresh();
        app.open_knowledge_pane(KnowledgeTab::Search);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.knowledge.active_tab(), KnowledgeTab::Ingest);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.knowledge.active_tab(), KnowledgeTab::Search);
    }

    #[test]
    fn knowledge_search_submit_prefills_scoped_prompt() {
        let mut app = fresh();
        app.open_knowledge_pane(KnowledgeTab::Search);
        for c in "TiAl creep".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        // Turn Literature off: query → literature, Space toggles.
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Char(' ')));
        app.handle_key(key(KeyCode::Enter));
        assert!(!app.knowledge.open, "submit must close the pane");
        assert_eq!(
            app.input.lines().join("\n"),
            "Search the knowledge graph for TiAl creep"
        );
        assert_eq!(app.focus, Focus::Input);
    }

    #[test]
    fn knowledge_search_requires_query_and_scope() {
        let mut app = fresh();
        app.open_knowledge_pane(KnowledgeTab::Search);
        app.handle_key(key(KeyCode::Enter));
        assert!(app.knowledge.open, "empty query must keep the pane open");
        assert!(app.toasts.iter().any(|t| t.message.contains("query")));
    }

    #[test]
    fn knowledge_ingest_meta_submit_prefills_ingest_prompt() {
        let mut app = fresh();
        app.open_knowledge_pane(KnowledgeTab::Ingest);
        // Simulate a picked file (browser navigation is covered by
        // knowledge.rs unit tests on a real temp dir).
        app.knowledge.ingest_file = Some(std::path::PathBuf::from("/data/niti.pdf"));
        app.knowledge.phase = IngestPhase::Meta;
        for c in "NiTi review".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
        assert!(!app.knowledge.open);
        assert_eq!(
            app.input.lines().join("\n"),
            "Ingest this file into the knowledge graph: /data/niti.pdf (title: NiTi review)"
        );
    }

    #[test]
    fn knowledge_meta_esc_backs_out_to_browser_not_close() {
        let mut app = fresh();
        app.open_knowledge_pane(KnowledgeTab::Ingest);
        app.knowledge.ingest_file = Some(std::path::PathBuf::from("/data/x.pdf"));
        app.knowledge.phase = IngestPhase::Meta;
        app.handle_key(key(KeyCode::Esc));
        assert!(app.knowledge.open, "Esc from metadata must not close");
        assert_eq!(app.knowledge.phase, IngestPhase::Browse);
        assert_eq!(app.knowledge.ingest_file, None);
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.knowledge.open, "Esc from browser closes the pane");
    }

    #[test]
    fn research_advisory_sources_are_labeled() {
        let mut app = fresh();
        app.open_research_form();
        for c in "q".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        // question → depth → src_web → src_kg → src_prov: toggle on.
        for _ in 0..4 {
            app.handle_key(key(KeyCode::Down));
        }
        app.handle_key(key(KeyCode::Char(' ')));
        app.handle_key(key(KeyCode::Enter));
        let prompt = app.input.lines().join("\n");
        assert!(
            prompt.contains("advisory: provenance/memory"),
            "unenforced sources must be labeled advisory: {prompt}"
        );
    }
}
