//! App state — the Model in TEA.

use crate::backend::BackendHandle;
use crate::command;
use crate::gh::{self, GhPanel, GhTab};
use crate::keymap;
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
    /// Enter/Space expand the selected item, i/Esc jump back to input.
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
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.workspace_expanded = !self.workspace_expanded;
            }
            KeyCode::Char('?') => self.open_which_key(),
            KeyCode::Char('i') | KeyCode::Esc => self.focus = Focus::Input,
            _ => {}
        }
    }

    fn workspace_next_tab(&mut self) {
        self.workspace_tab = match self.workspace_tab {
            WorkspaceTab::Activity => WorkspaceTab::Tools,
            WorkspaceTab::Tools => WorkspaceTab::Files,
            WorkspaceTab::Files => WorkspaceTab::Activity,
        };
        self.workspace_selected = 0;
        self.workspace_expanded = false;
    }

    fn workspace_prev_tab(&mut self) {
        self.workspace_tab = match self.workspace_tab {
            WorkspaceTab::Activity => WorkspaceTab::Files,
            WorkspaceTab::Tools => WorkspaceTab::Activity,
            WorkspaceTab::Files => WorkspaceTab::Tools,
        };
        self.workspace_selected = 0;
        self.workspace_expanded = false;
    }

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

    /// Execute a catalog command by id. Reuses existing action paths so
    /// the palette, slash-commands, and keybinds stay in sync.
    fn dispatch_command(&mut self, id: &str) {
        self.close_palette();
        match id {
            "help.show" => self.modal = Some(Modal::Help),
            "which_key.show" => self.open_which_key(),
            "theme.list" => self.open_theme_picker(),
            "gh.show" => self.open_gh(),
            "session.new" => self.new_session(),
            "cost.show" => self.modal = Some(Modal::Cost),
            "model.show" => self.modal = Some(Modal::Model),
            "mcp.show" => self.modal = Some(Modal::Tools),
            "goal.set" => {
                self.focus = Focus::Input;
                self.toast("type: /goal <text>", ToastKind::Info);
            }
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
                // Current behavior: display as a system line listing
                // the session count.  The full JSON objects are retained
                // in the variant for future rendering.
                if sessions.is_empty() {
                    self.push_system("[no previous sessions]");
                } else {
                    self.push_system(&format!("[{} previous session(s)]", sessions.len()));
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
                self.push_message(ChatLine {
                    role: Role::Tool,
                    text: format!("{} {}", clean_verb, clean_name),
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
            }
            AgentMsg::View { title, tabs } => {
                // Sanitize each visible field from the backend.
                let clean_title = sanitize_for_render(&title);
                for (tab_title, body) in tabs {
                    let clean_tab = sanitize_for_render(&tab_title);
                    let clean_body = sanitize_for_render(&body);
                    self.push_message(ChatLine {
                        role: Role::System,
                        text: format!("[{} > {}]\n{}", clean_title, clean_tab, clean_body),
                        kind: LineKind::View {
                            title: clean_title.clone(),
                            body: clean_body,
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
}
