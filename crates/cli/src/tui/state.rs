#![allow(dead_code)]

use std::path::PathBuf;

use crate::tui::protocol::{UiCard, UiCost, UiPrompt, UiStatus, UiToolStart, UiView};

#[derive(Debug, Clone)]
pub enum ChatElement {
    UserMessage(String),
    Text(String),
    ToolStart(UiToolStart),
    Card(UiCard),
    Cost(UiCost),
}

/// Which UI zone has keyboard focus
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusZone {
    Input,   // Text input bar (default)
    Chat,    // Chat canvas (scrollable)
    Sidebar, // Sidebar panel (scrollable)
}

/// The main content area workspace
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workspace {
    Chat,
    Explorer,    // Knowledge graph browser
    Models,      // Model selection + config
    Compute,     // GPU jobs, deployments
    Mesh,        // Nodes, peers, federation
    Workflows,   // Workflow list + runner
    Marketplace, // Browse/install resources
    Data,        // Datasets, corpora, ingest
    Settings,    // Config, permissions, billing
}

/// Activity bar items — like VS Code's left icon strip
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Chat,
    Explorer,
    Models,
    Compute,
    Mesh,
    Workflows,
    Marketplace,
    Data,
    Settings,
}

impl Activity {
    pub fn all() -> &'static [Activity] {
        &[
            Self::Chat,
            Self::Explorer,
            Self::Models,
            Self::Compute,
            Self::Mesh,
            Self::Workflows,
            Self::Marketplace,
            Self::Data,
            Self::Settings,
        ]
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Chat => "\u{25cf}",        // ●  (filled = active by default)
            Self::Explorer => "\u{2737}",    // ✷  knowledge graph
            Self::Models => "\u{2636}",      // ☶  LLM models
            Self::Compute => "\u{2699}",     // ⚙  GPU/compute
            Self::Mesh => "\u{2630}",        // ☰  mesh/network
            Self::Workflows => "\u{25b7}",   // ▷  play/workflow
            Self::Marketplace => "\u{229e}", // ⊞  grid/store
            Self::Data => "\u{2261}",        // ≡  data/list
            Self::Settings => "\u{2638}",    // ☸  settings
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Explorer => "Explorer",
            Self::Models => "Models",
            Self::Compute => "Compute",
            Self::Mesh => "Mesh",
            Self::Workflows => "Workflows",
            Self::Marketplace => "Marketplace",
            Self::Data => "Data",
            Self::Settings => "Settings",
        }
    }

    pub fn shortcut(&self) -> &'static str {
        match self {
            Self::Chat => "1",
            Self::Explorer => "2",
            Self::Models => "3",
            Self::Compute => "4",
            Self::Mesh => "5",
            Self::Workflows => "6",
            Self::Marketplace => "7",
            Self::Data => "8",
            Self::Settings => "9",
        }
    }

    pub fn to_workspace(self) -> Workspace {
        match self {
            Self::Chat => Workspace::Chat,
            Self::Explorer => Workspace::Explorer,
            Self::Models => Workspace::Models,
            Self::Compute => Workspace::Compute,
            Self::Mesh => Workspace::Mesh,
            Self::Workflows => Workspace::Workflows,
            Self::Marketplace => Workspace::Marketplace,
            Self::Data => Workspace::Data,
            Self::Settings => Workspace::Settings,
        }
    }
}

#[derive(Debug)]
pub struct App {
    // Chat state
    pub chat_history: Vec<ChatElement>,
    pub streaming_text: String,

    // Layout — VS Code style
    pub activity_bar_idx: usize, // selected activity (0-8)
    pub sidebar_visible: bool,   // toggle panel open/closed
    pub workspace: Workspace,    // what the main content shows

    // Backend state
    pub status: Option<UiStatus>,
    pub total_cost: f64,

    // View panel (opens INSIDE the sidebar panel, not the canvas)
    pub active_view: Option<UiView>,
    pub view_tab_index: usize,
    pub view_scroll: u16,

    // Approval prompt (modal overlay — blocks everything)
    pub active_prompt: Option<UiPrompt>,

    // Auth state
    pub auth_error: bool, // true when 401 detected
    pub login_device_code: Option<String>,
    pub login_url: Option<String>,

    // Input
    pub input_buffer: String,
    pub input_cursor: usize, // byte position in input_buffer
    pub input_history: Vec<String>,
    pub input_history_idx: Option<usize>,

    // Command palette (autocomplete)
    pub palette_visible: bool,
    pub palette_selected: usize,

    // Focus & navigation
    pub focus: FocusZone,
    pub chat_scroll: u16,
    pub sidebar_scroll: u16,

    // Background loading flags
    pub loading_models: bool,

    // Model picker
    pub model_picker_visible: bool,
    pub model_picker_search: String,
    pub model_picker_selected: usize,
    pub model_picker_provider_idx: usize, // 0 = all, 1+ = specific provider
    pub cached_models: Vec<super::components::model_picker::ModelInfo>,
    pub cached_providers: Vec<String>,

    // Misc
    pub project_root: PathBuf,
    pub should_quit: bool,

    // Cached workspace data
    pub tool_count: usize,
    pub model_count: Option<usize>,
    pub gpu_count: Option<usize>,
    pub node_count: Option<usize>,
    pub peer_count: Option<usize>,
    pub marketplace_count: Option<usize>,
    pub workflow_names: Vec<String>,
    pub corpus_count: Option<usize>,
    pub entity_count: Option<String>,
}

impl App {
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            chat_history: Vec::new(),
            streaming_text: String::new(),
            activity_bar_idx: 0,
            sidebar_visible: true,
            workspace: Workspace::Chat,
            status: None,
            total_cost: 0.0,
            active_view: None,
            view_tab_index: 0,
            view_scroll: 0,
            active_prompt: None,
            auth_error: false,
            login_device_code: None,
            login_url: None,
            input_buffer: String::new(),
            input_cursor: 0,
            input_history: Vec::new(),
            input_history_idx: None,
            palette_visible: false,
            palette_selected: 0,
            focus: FocusZone::Input,
            chat_scroll: 0,
            sidebar_scroll: 0,
            loading_models: false,
            model_picker_visible: false,
            model_picker_search: String::new(),
            model_picker_selected: 0,
            model_picker_provider_idx: 0,
            cached_models: Vec::new(),
            cached_providers: Vec::new(),
            project_root,
            should_quit: false,
            tool_count: 0,
            model_count: None,
            gpu_count: None,
            node_count: None,
            peer_count: None,
            marketplace_count: None,
            workflow_names: Vec::new(),
            corpus_count: None,
            entity_count: None,
        }
    }

    pub fn current_activity(&self) -> Activity {
        Activity::all()[self.activity_bar_idx]
    }

    pub fn select_activity(&mut self, idx: usize) {
        let activities = Activity::all();
        if idx < activities.len() {
            self.activity_bar_idx = idx;
            self.workspace = activities[idx].to_workspace();
            self.active_view = None;
            self.view_tab_index = 0;
            self.view_scroll = 0;
        }
    }

    pub fn activity_up(&mut self) {
        if self.activity_bar_idx > 0 {
            self.select_activity(self.activity_bar_idx - 1);
        }
    }

    pub fn activity_down(&mut self) {
        let max = Activity::all().len() - 1;
        if self.activity_bar_idx < max {
            self.select_activity(self.activity_bar_idx + 1);
        }
    }
}
