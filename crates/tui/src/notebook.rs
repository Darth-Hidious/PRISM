//! Notebook pane — the in-app Python notebook surface.
//!
//! PRISM's notebook is a real, persistent kernel shared between the human (this
//! pane) and the agent (`notebook_exec` tool): the same variables, the same
//! cell log. This module holds the pane's state + pure helpers (cell parsing,
//! run-command composition); key handling lives in `app.rs` and rendering in
//! `render.rs`, matching the `knowledge.rs` / `gh.rs` convention.
//!
//! The kernel itself runs in the backend process (`crates/agent/src/notebook.rs`);
//! this pane drives it entirely through slash commands (`/notebook open|run|
//! reset`) and renders the `ui.notebook.state` / `ui.notebook.cell` events it
//! sends back. No exit to a shell, no browser — the whole loop stays in the TUI.

use ratatui_textarea::TextArea;
use serde_json::Value;

/// One executed cell, mirrored from the backend's shared cell log.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NotebookCell {
    pub execution_count: u64,
    /// `"user"` (this pane) or `"agent"` (the tool) — shown so the human can
    /// see which cells the agent ran.
    pub origin: String,
    pub code: String,
    pub stdout: String,
    pub stderr: String,
    pub result: Option<String>,
    /// Saved PNG paths (terminals can't inline images; we show the path).
    pub image_paths: Vec<String>,
    pub error: Option<String>,
    pub success: bool,
}

impl NotebookCell {
    /// Parse one cell from a backend JSON payload, defaulting missing fields.
    pub fn from_value(value: &Value) -> Self {
        Self {
            execution_count: value
                .get("execution_count")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            origin: value
                .get("origin")
                .and_then(Value::as_str)
                .unwrap_or("user")
                .to_string(),
            code: value
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            stdout: value
                .get("stdout")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            stderr: value
                .get("stderr")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            result: value
                .get("result")
                .and_then(Value::as_str)
                .map(str::to_string),
            image_paths: value
                .get("image_paths")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            error: value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string),
            success: value
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }
    }
}

/// Notebook pane state.
#[derive(Debug, Default)]
pub struct NotebookPane {
    pub open: bool,
    /// Multi-line code editor for the next cell.
    pub input: TextArea<'static>,
    pub cells: Vec<NotebookCell>,
    /// Kernel status line for the header (e.g. "jupyter · Python 3.12.4").
    pub kernel_status: String,
    pub running: bool,
    pub scroll: u16,
}

impl NotebookPane {
    /// Open a fresh pane (the code editor starts empty).
    pub fn opened() -> Self {
        let mut input = TextArea::default();
        input.set_placeholder_text("Type Python — Ctrl-R runs the cell, Esc closes");
        Self {
            open: true,
            input,
            cells: Vec::new(),
            kernel_status: "kernel: starting…".to_string(),
            running: false,
            scroll: 0,
        }
    }

    /// The code currently in the editor (joined lines).
    pub fn code(&self) -> String {
        self.input.lines().join("\n")
    }

    /// Clear the editor after a successful run.
    pub fn clear_input(&mut self) {
        self.input = TextArea::default();
        self.input
            .set_placeholder_text("Type Python — Ctrl-R runs the cell, Esc closes");
    }

    /// Apply a `ui.notebook.state` payload (full refresh).
    pub fn apply_state(&mut self, running: bool, header: String, cells: Vec<NotebookCell>) {
        self.running = running;
        self.kernel_status = header;
        self.cells = cells;
    }

    /// Apply a `ui.notebook.cell` payload (append or replace by count).
    pub fn apply_cell(&mut self, cell: NotebookCell, header: Option<String>) {
        self.running = false;
        if let Some(header) = header {
            self.kernel_status = header;
        }
        // Replace a same-numbered cell if it already exists (re-render), else
        // append. Execution counts are monotonic, so this is usually an append.
        if let Some(slot) = self
            .cells
            .iter_mut()
            .find(|existing| existing.execution_count == cell.execution_count)
        {
            *slot = cell;
        } else {
            self.cells.push(cell);
        }
    }
}

/// Compose the kernel status header from a backend payload's fields.
pub fn kernel_header(running: bool, backend: Option<&str>, python: Option<&str>) -> String {
    match (running, backend) {
        (true, Some(backend)) => {
            format!("kernel: {backend} · Python {}", python.unwrap_or("?"))
        }
        (true, None) => "kernel: running".to_string(),
        (false, _) => "kernel: idle (starts on first run)".to_string(),
    }
}

/// Build the `/notebook run --code <code>` slash command for the current
/// editor contents, or `None` when the editor is empty. The code is passed as
/// a single shlex-quoted token so newlines and quotes survive the round trip
/// to the backend (which shlex-splits it back).
pub fn run_command(code: &str) -> Option<String> {
    let trimmed = code.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(format!("/notebook run --code {}", shlex_quote(code)))
}

/// Minimal POSIX single-quote escaping (matches the backend's shlex parsing).
fn shlex_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || "._-/=".contains(c))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_command_is_none_when_empty() {
        assert!(run_command("   \n  ").is_none());
    }

    #[test]
    fn run_command_quotes_multiline_code_for_round_trip() {
        let code = "import os\nprint('it works')";
        let cmd = run_command(code).expect("non-empty code yields a command");
        assert!(cmd.starts_with("/notebook run --code "));
        // The whole code block is one shlex token: shlex-splitting the tail
        // must recover the exact source, newline and quotes intact.
        let tail = cmd.strip_prefix("/notebook run --code ").unwrap();
        let split = shlex::split(tail).expect("valid shlex");
        assert_eq!(split, vec![code.to_string()]);
    }

    #[test]
    fn cell_parses_from_backend_payload() {
        let value = serde_json::json!({
            "execution_count": 3,
            "origin": "agent",
            "code": "x = 1",
            "stdout": "hi\n",
            "stderr": "",
            "result": "42",
            "image_paths": ["/tmp/cell-3-0.png"],
            "error": null,
            "success": true,
        });
        let cell = NotebookCell::from_value(&value);
        assert_eq!(cell.execution_count, 3);
        assert_eq!(cell.origin, "agent");
        assert_eq!(cell.result.as_deref(), Some("42"));
        assert_eq!(cell.image_paths, vec!["/tmp/cell-3-0.png".to_string()]);
        assert!(cell.success);
    }

    #[test]
    fn apply_cell_appends_then_replaces_by_count() {
        let mut pane = NotebookPane::opened();
        let cell = |count: u64, result: &str| NotebookCell {
            execution_count: count,
            result: Some(result.to_string()),
            success: true,
            ..Default::default()
        };
        pane.apply_cell(cell(1, "a"), None);
        pane.apply_cell(cell(2, "b"), None);
        assert_eq!(pane.cells.len(), 2);
        // Same count replaces in place rather than duplicating.
        pane.apply_cell(cell(2, "b2"), None);
        assert_eq!(pane.cells.len(), 2);
        assert_eq!(pane.cells[1].result.as_deref(), Some("b2"));
        assert!(!pane.running, "a delivered cell clears the running flag");
    }

    #[test]
    fn kernel_header_reflects_state() {
        assert_eq!(
            kernel_header(true, Some("jupyter"), Some("3.12.4")),
            "kernel: jupyter · Python 3.12.4"
        );
        assert!(kernel_header(false, Some("jupyter"), Some("3.12.4")).contains("idle"));
    }
}
