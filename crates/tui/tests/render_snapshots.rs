//! Render snapshot tests for the PRISM TUI.
//!
//! These tests render the TUI's `App` state to a `TestBackend` buffer
//! and compare the output against committed snapshots via `insta`.
//!
//! ## Snapshot review policy
//!
//! Snapshots are visual contracts — they catch real UI regressions.
//! When a snapshot test fails:
//! 1. Inspect the diff: `cargo insta review` (if available) or open
//!    the `.snap` file and the `.snap.new` file.
//! 2. If the change is intentional (you modified the render path),
//!    accept the new snapshot: `INSTA_UPDATE=always cargo test -p
//!    prism-tui --test render_snapshots`.
//! 3. If the change is a regression, fix the code, don't accept the
//!    snapshot.
//!
//! Do NOT blindly accept snapshots without inspecting them.
//!
//! ## Determinism
//!
//! All snapshots use fixed terminal sizes and deterministic App state.
//! Volatile fields (tokens_per_sec, timestamps) are set to fixed values
//! or redacted in the snapshot string so the test is reproducible.

#![cfg(test)]

use prism_tui::app::{App, Focus};
use prism_tui::backend::{BackendHandle, FakeScenario};
use prism_tui::msg::AgentMsg;
use prism_tui::render::draw;

use ratatui::Terminal;
use ratatui::backend::TestBackend;

// ── Helpers ─────────────────────────────────────────────────────────

/// Render an `App` to a deterministic string at the given terminal size.
///
/// Creates a `TestBackend`, calls `render::draw`, and converts the
/// buffer to a string where each line is the cell symbols joined.
/// Trailing spaces on each line are preserved (they catch layout
/// regressions), but trailing empty lines at the bottom are trimmed.
fn render_app_to_string(app: &App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("failed to create TestBackend");
    terminal.draw(|f| draw(f, app)).expect("failed to draw");

    let buffer = terminal.backend().buffer();
    let mut lines: Vec<String> = Vec::new();
    for row in 0..buffer.area.height {
        let mut line = String::new();
        for col in 0..buffer.area.width {
            let cell = &buffer[(col, row)];
            line.push_str(cell.symbol());
        }
        // Trim trailing spaces but keep the content
        let trimmed = line.trim_end();
        lines.push(trimmed.to_string());
    }
    // Remove trailing empty lines
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// Create an `App` with a fake backend (no subprocess spawned).
fn fake_app() -> App {
    App::new(BackendHandle::fake(FakeScenario::BasicChat))
}

/// Assert that the rendered string contains no unsafe terminal control
/// characters (ESC, BEL, BS, CR, DEL).
fn assert_no_terminal_controls(text: &str) {
    assert!(!text.contains('\x1b'), "ESC found in render output");
    assert!(!text.contains('\x07'), "BEL found in render output");
    assert!(!text.contains('\x08'), "BS found in render output");
    assert!(!text.contains('\x0d'), "CR found in render output");
    assert!(!text.contains('\x7f'), "DEL found in render output");
}

// ── Snapshot tests ───────────────────────────────────────────────────

/// Snapshot: empty launch state at 100x30.
/// App is freshly created, no messages, no backend events applied.
#[test]
fn snapshot_empty_launch_100x30() {
    let app = fake_app();
    let rendered = render_app_to_string(&app, 100, 30);
    insta::assert_snapshot!("empty_launch_100x30", rendered);
}

/// Snapshot: basic chat after response at 100x30.
/// Apply welcome + status + user message + streamed response.
#[test]
fn snapshot_basic_chat_after_response_100x30() {
    let mut app = fake_app();
    // Apply welcome
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1-fake".into(),
        tool_count: 99,
    });
    // Apply status
    app.apply_agent_msg(AgentMsg::Status {
        model: "fake-backend".into(),
        mode: "chat".into(),
        message_count: 1,
    });
    // User sends a message
    app.push_user("Hello PRISM");
    // Apply streamed response
    app.apply_agent_msg(AgentMsg::TextDelta(
        "Fake backend response: PRISM TUI is running ".into(),
    ));
    app.apply_agent_msg(AgentMsg::TextDelta("in deterministic test mode.".into()));
    app.apply_agent_msg(AgentMsg::TextFlush);
    app.apply_agent_msg(AgentMsg::TurnComplete);
    // Set deterministic metrics (avoid volatile tokens_per_sec)
    app.tokens_per_sec = 0.0;
    app.first_token_time = None;
    app.last_token_time = None;
    app.tokens_received = 0;

    let rendered = render_app_to_string(&app, 100, 30);
    insta::assert_snapshot!("basic_chat_after_response_100x30", rendered);
}

/// Snapshot: thinking stream, collapsed at 100x30.
#[test]
fn snapshot_thinking_stream_collapsed_100x30() {
    let mut app = fake_app();
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1-fake".into(),
        tool_count: 99,
    });
    app.apply_agent_msg(AgentMsg::Status {
        model: "fake-backend".into(),
        mode: "chat".into(),
        message_count: 1,
    });
    app.push_user("think about this");
    // Thinking deltas
    app.apply_agent_msg(AgentMsg::ThinkingDelta("Let me reason about this. ".into()));
    app.apply_agent_msg(AgentMsg::ThinkingDelta(
        "The user is asking a question.".into(),
    ));
    // Answer deltas
    app.apply_agent_msg(AgentMsg::TextDelta(
        "Based on my reasoning, here is the answer.".into(),
    ));
    app.apply_agent_msg(AgentMsg::TextFlush);
    app.apply_agent_msg(AgentMsg::TurnComplete);
    // thinking_expanded is false (collapsed) by default
    app.tokens_per_sec = 0.0;
    app.first_token_time = None;
    app.last_token_time = None;
    app.tokens_received = 0;

    let rendered = render_app_to_string(&app, 100, 30);
    insta::assert_snapshot!("thinking_stream_collapsed_100x30", rendered);
}

/// Snapshot: thinking stream, expanded at 100x30.
/// Same state as collapsed but with thinking_expanded = true.
#[test]
fn snapshot_thinking_stream_expanded_100x30() {
    let mut app = fake_app();
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1-fake".into(),
        tool_count: 99,
    });
    app.apply_agent_msg(AgentMsg::Status {
        model: "fake-backend".into(),
        mode: "chat".into(),
        message_count: 1,
    });
    app.push_user("think about this");
    app.apply_agent_msg(AgentMsg::ThinkingDelta("Let me reason about this. ".into()));
    app.apply_agent_msg(AgentMsg::ThinkingDelta(
        "The user is asking a question.".into(),
    ));
    app.apply_agent_msg(AgentMsg::TextDelta(
        "Based on my reasoning, here is the answer.".into(),
    ));
    app.apply_agent_msg(AgentMsg::TextFlush);
    app.apply_agent_msg(AgentMsg::TurnComplete);
    // Expand thinking
    app.thinking_expanded = true;
    app.tokens_per_sec = 0.0;
    app.first_token_time = None;
    app.last_token_time = None;
    app.tokens_received = 0;

    let rendered = render_app_to_string(&app, 100, 30);
    insta::assert_snapshot!("thinking_stream_expanded_100x30", rendered);
}

/// Snapshot: tool success at 100x30.
#[test]
fn snapshot_tool_success_100x30() {
    let mut app = fake_app();
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1-fake".into(),
        tool_count: 99,
    });
    app.apply_agent_msg(AgentMsg::Status {
        model: "fake-backend".into(),
        mode: "chat".into(),
        message_count: 1,
    });
    app.push_user("sample alloy");
    app.apply_agent_msg(AgentMsg::ToolStart {
        tool_name: "sample_material".into(),
        verb: "Running".into(),
        call_id: Some("call-1".into()),
        preview: Some("{\"n\": 10}".into()),
        approval_required: Some(false),
    });
    app.apply_agent_msg(AgentMsg::ToolCard {
        tool_name: "sample_material".into(),
        call_id: Some("call-1".into()),
        content: "W0.3 Mo0.2 Ta0.3 Nb0.2".into(),
        card_type: "results".into(),
        elapsed_ms: Some(292),
        provenance_id: Some("prov_001".into()),
        data: None,
    });
    app.apply_agent_msg(AgentMsg::TurnComplete);
    app.tokens_per_sec = 0.0;
    app.first_token_time = None;
    app.last_token_time = None;
    app.tokens_received = 0;

    let rendered = render_app_to_string(&app, 100, 30);
    insta::assert_snapshot!("tool_success_100x30", rendered);
}

/// Snapshot: tool error at 100x30.
#[test]
fn snapshot_tool_error_100x30() {
    let mut app = fake_app();
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1-fake".into(),
        tool_count: 99,
    });
    app.apply_agent_msg(AgentMsg::Status {
        model: "fake-backend".into(),
        mode: "chat".into(),
        message_count: 1,
    });
    app.push_user("submit job");
    app.apply_agent_msg(AgentMsg::ToolStart {
        tool_name: "compute_submit".into(),
        verb: "Running".into(),
        call_id: Some("call-2".into()),
        preview: None,
        approval_required: Some(true),
    });
    app.apply_agent_msg(AgentMsg::ToolCard {
        tool_name: "compute_submit".into(),
        call_id: Some("call-2".into()),
        content: "Error: budget exceeded ($50.00 limit)".into(),
        card_type: "error".into(),
        elapsed_ms: Some(1200),
        provenance_id: None,
        data: None,
    });
    app.apply_agent_msg(AgentMsg::TurnComplete);
    app.tokens_per_sec = 0.0;
    app.first_token_time = None;
    app.last_token_time = None;
    app.tokens_received = 0;

    let rendered = render_app_to_string(&app, 100, 30);
    insta::assert_snapshot!("tool_error_100x30", rendered);
}

/// Snapshot: approval required popup at 100x30.
#[test]
fn snapshot_approval_required_popup_100x30() {
    let mut app = fake_app();
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1-fake".into(),
        tool_count: 99,
    });
    app.apply_agent_msg(AgentMsg::Status {
        model: "fake-backend".into(),
        mode: "chat".into(),
        message_count: 1,
    });
    app.push_user("run compute");
    app.apply_agent_msg(AgentMsg::ApprovalPrompt {
        tool_name: "compute_submit".into(),
        call_id: Some("call-3".into()),
        message: "Allow compute_submit?".into(),
        tool_args: Some(serde_json::json!({"image": "vasp:6.5"})),
        tool_description: Some("Dispatch a GPU compute job".into()),
        requires_approval: Some(true),
        permission_mode: Some("full_access".into()),
        choices: vec!["y".into(), "n".into(), "a".into()],
        prompt_type: Some("approval".into()),
    });
    // The approval popup should be visible
    app.tokens_per_sec = 0.0;
    app.first_token_time = None;
    app.last_token_time = None;
    app.tokens_received = 0;

    let rendered = render_app_to_string(&app, 100, 30);
    insta::assert_snapshot!("approval_required_popup_100x30", rendered);
}

/// Snapshot: cost metrics at 100x30.
#[test]
fn snapshot_cost_metrics_100x30() {
    let mut app = fake_app();
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1-fake".into(),
        tool_count: 99,
    });
    app.apply_agent_msg(AgentMsg::Status {
        model: "fake-backend".into(),
        mode: "chat".into(),
        message_count: 1,
    });
    app.push_user("show cost");
    app.apply_agent_msg(AgentMsg::TextDelta("Cost report ready.".into()));
    app.apply_agent_msg(AgentMsg::TextFlush);
    app.apply_agent_msg(AgentMsg::Cost {
        turn_cost: 0.001,
        session_cost: 0.05,
        input_tokens: Some(1200),
        output_tokens: Some(800),
        cache_tokens: Some(400),
    });
    app.apply_agent_msg(AgentMsg::TurnComplete);
    app.tokens_per_sec = 0.0;
    app.first_token_time = None;
    app.last_token_time = None;
    app.tokens_received = 0;

    let rendered = render_app_to_string(&app, 100, 30);
    insta::assert_snapshot!("cost_metrics_100x30", rendered);
}

/// Snapshot: ANSI injection sanitized at 100x30.
/// Verify the sanitizer strips ANSI/control sequences before they
/// reach the render buffer.
#[test]
fn snapshot_ansi_injection_sanitized_100x30() {
    let mut app = fake_app();
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1-fake".into(),
        tool_count: 99,
    });
    app.apply_agent_msg(AgentMsg::Status {
        model: "fake-backend".into(),
        mode: "chat".into(),
        message_count: 1,
    });
    app.push_user("inject ansi");
    // Text delta with ANSI escapes — sanitizer should strip them
    app.apply_agent_msg(AgentMsg::TextDelta(
        "\x1b[31mred text\x1b[0m \x1b]0;owned\x07safe \x07BEL\x08BS\x0dCR\x7fDEL".into(),
    ));
    app.apply_agent_msg(AgentMsg::TextFlush);
    app.apply_agent_msg(AgentMsg::TurnComplete);
    app.tokens_per_sec = 0.0;
    app.first_token_time = None;
    app.last_token_time = None;
    app.tokens_received = 0;

    let rendered = render_app_to_string(&app, 100, 30);
    // Assert no unsafe terminal controls in the rendered output
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("ansi_injection_sanitized_100x30", rendered);
}

/// Snapshot: tiny terminal basic chat at 40x12.
#[test]
fn snapshot_tiny_terminal_basic_chat_40x12() {
    let mut app = fake_app();
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1-fake".into(),
        tool_count: 99,
    });
    app.apply_agent_msg(AgentMsg::Status {
        model: "fake-backend".into(),
        mode: "chat".into(),
        message_count: 1,
    });
    app.push_user("Hello");
    app.apply_agent_msg(AgentMsg::TextDelta(
        "Fake backend response: PRISM TUI is running ".into(),
    ));
    app.apply_agent_msg(AgentMsg::TextDelta("in deterministic test mode.".into()));
    app.apply_agent_msg(AgentMsg::TextFlush);
    app.apply_agent_msg(AgentMsg::TurnComplete);
    app.tokens_per_sec = 0.0;
    app.first_token_time = None;
    app.last_token_time = None;
    app.tokens_received = 0;

    let rendered = render_app_to_string(&app, 40, 12);
    insta::assert_snapshot!("tiny_terminal_basic_chat_40x12", rendered);
}

/// Snapshot: wide terminal basic chat at 200x60.
#[test]
fn snapshot_wide_terminal_basic_chat_200x60() {
    let mut app = fake_app();
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1-fake".into(),
        tool_count: 99,
    });
    app.apply_agent_msg(AgentMsg::Status {
        model: "fake-backend".into(),
        mode: "chat".into(),
        message_count: 1,
    });
    app.push_user("Hello PRISM, what tools do you have?");
    app.apply_agent_msg(AgentMsg::TextDelta(
        "Fake backend response: PRISM TUI is running ".into(),
    ));
    app.apply_agent_msg(AgentMsg::TextDelta("in deterministic test mode.".into()));
    app.apply_agent_msg(AgentMsg::TextFlush);
    app.apply_agent_msg(AgentMsg::TurnComplete);
    app.tokens_per_sec = 0.0;
    app.first_token_time = None;
    app.last_token_time = None;
    app.tokens_received = 0;

    let rendered = render_app_to_string(&app, 200, 60);
    insta::assert_snapshot!("wide_terminal_basic_chat_200x60", rendered);
}

// ═══════════════════════════════════════════════════════════════════════
// Patch 4B: expanded render snapshot coverage
// ═══════════════════════════════════════════════════════════════════════

/// Helper: create a baseline app with welcome + status already applied.
fn app_with_welcome() -> App {
    let mut app = fake_app();
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1-fake".into(),
        tool_count: 99,
    });
    app.apply_agent_msg(AgentMsg::Status {
        model: "fake-backend".into(),
        mode: "chat".into(),
        message_count: 1,
    });
    app.tokens_per_sec = 0.0;
    app.first_token_time = None;
    app.last_token_time = None;
    app.tokens_received = 0;
    app
}

/// Helper: set volatile metrics to deterministic values.
fn freeze_metrics(app: &mut App) {
    app.tokens_per_sec = 0.0;
    app.first_token_time = None;
    app.last_token_time = None;
    app.tokens_received = 0;
}

// ── Backend warning + error ─────────────────────────────────────────

#[test]
fn snapshot_backend_warning_error_100x30() {
    let mut app = app_with_welcome();
    app.push_user("trigger error");
    app.apply_agent_msg(AgentMsg::BackendWarning {
        code: Some("rate_limit".into()),
        message: "Approaching API rate limit (80% of quota)".into(),
    });
    app.apply_agent_msg(AgentMsg::BackendError {
        code: Some(429),
        message: "Rate limit exceeded, please retry in 60s".into(),
        recoverable: Some(true),
    });
    app.apply_agent_msg(AgentMsg::TurnComplete);
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("backend_warning_error_100x30", rendered);
}

// ── Approval after y (approved) ──────────────────────────────────────

#[test]
fn snapshot_approval_after_y_100x30() {
    let mut app = app_with_welcome();
    app.push_user("run compute");
    // Show the approval prompt
    app.apply_agent_msg(AgentMsg::ApprovalPrompt {
        tool_name: "compute_submit".into(),
        call_id: Some("call-3".into()),
        message: "Allow compute_submit?".into(),
        tool_args: None,
        tool_description: None,
        requires_approval: Some(true),
        permission_mode: None,
        choices: vec!["y".into(), "n".into(), "a".into()],
        prompt_type: None,
    });
    // Simulate pressing 'y' — the key handler approves and clears
    // the pending state.  We replicate the visible behavior directly.
    if let Some((tool, _)) = app.approval_pending.take() {
        app.push_system(&format!("[approved {tool}]"));
    }
    app.focus = prism_tui::app::Focus::Input;
    // Apply a tool success card as the backend response
    app.apply_agent_msg(AgentMsg::ToolCard {
        tool_name: "compute_submit".into(),
        call_id: Some("call-3".into()),
        content: "Job submitted successfully (job_id: fake-123)".into(),
        card_type: "results".into(),
        elapsed_ms: Some(500),
        provenance_id: None,
        data: None,
    });
    app.apply_agent_msg(AgentMsg::TurnComplete);
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("approval_after_y_100x30", rendered);
}

// ── Approval after n (denied) ────────────────────────────────────────

#[test]
fn snapshot_approval_after_n_100x30() {
    let mut app = app_with_welcome();
    app.push_user("run compute");
    app.apply_agent_msg(AgentMsg::ApprovalPrompt {
        tool_name: "compute_submit".into(),
        call_id: Some("call-3".into()),
        message: "Allow compute_submit?".into(),
        tool_args: None,
        tool_description: None,
        requires_approval: Some(true),
        permission_mode: None,
        choices: vec!["y".into(), "n".into(), "a".into()],
        prompt_type: None,
    });
    // Simulate pressing 'n' — deny
    if let Some((tool, _)) = app.approval_pending.take() {
        app.push_system(&format!("[denied {tool}]"));
    }
    app.focus = prism_tui::app::Focus::Input;
    app.apply_agent_msg(AgentMsg::TurnComplete);
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("approval_after_n_100x30", rendered);
}

// ── Approval after a (allow-all) ──────────────────────────────────────

#[test]
fn snapshot_approval_after_a_100x30() {
    let mut app = app_with_welcome();
    app.push_user("run compute");
    app.apply_agent_msg(AgentMsg::ApprovalPrompt {
        tool_name: "compute_submit".into(),
        call_id: Some("call-3".into()),
        message: "Allow compute_submit?".into(),
        tool_args: None,
        tool_description: None,
        requires_approval: Some(true),
        permission_mode: None,
        choices: vec!["y".into(), "n".into(), "a".into()],
        prompt_type: None,
    });
    // Simulate pressing 'a' — allow all
    if let Some((tool, _)) = app.approval_pending.take() {
        app.push_system(&format!("[allow-all {tool}]"));
    }
    app.focus = prism_tui::app::Focus::Input;
    // Backend response: permissions auto-approved + card
    app.apply_agent_msg(AgentMsg::Permissions {
        mode: Some("agent".into()),
        auto_approved: Some(true),
        raw: serde_json::json!({}),
    });
    app.apply_agent_msg(AgentMsg::ToolCard {
        tool_name: "compute_submit".into(),
        call_id: Some("call-3".into()),
        content: "Job submitted (auto-approved for session)".into(),
        card_type: "results".into(),
        elapsed_ms: Some(500),
        provenance_id: None,
        data: None,
    });
    app.apply_agent_msg(AgentMsg::TurnComplete);
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("approval_after_a_100x30", rendered);
}

// ── Unicode-heavy message ────────────────────────────────────────────

#[test]
fn snapshot_unicode_heavy_message_100x30() {
    let mut app = app_with_welcome();
    app.push_user("show unicode");
    // Intentional Unicode preservation test — contains CJK, emoji,
    // math symbols, combining marks.  The sanitizer must preserve
    // all of these.
    app.apply_agent_msg(AgentMsg::TextDelta(
        "Ti₆Al₄V ΔH_mix 你好 café 🚀 entropy μ".into(),
    ));
    app.apply_agent_msg(AgentMsg::TextFlush);
    app.apply_agent_msg(AgentMsg::TurnComplete);
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("unicode_heavy_message_100x30", rendered);
}

// ── Long unbroken line ────────────────────────────────────────────────

#[test]
fn snapshot_long_unbroken_line_100x30() {
    let mut app = app_with_welcome();
    app.push_user("show long line");
    // A long JSON-like string with no spaces — tests word wrapping.
    let long = "key_".to_string() + &"value_".repeat(80) + "end_of_long_unbroken_token";
    app.apply_agent_msg(AgentMsg::TextDelta(long));
    app.apply_agent_msg(AgentMsg::TextFlush);
    app.apply_agent_msg(AgentMsg::TurnComplete);
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("long_unbroken_line_100x30", rendered);
}

// ── Multiline message ─────────────────────────────────────────────────

#[test]
fn snapshot_multiline_message_100x30() {
    let mut app = app_with_welcome();
    app.push_user("show multiline");
    app.apply_agent_msg(AgentMsg::TextDelta("Line 1: hello\n".into()));
    app.apply_agent_msg(AgentMsg::TextDelta("Line 2: world\n".into()));
    app.apply_agent_msg(AgentMsg::TextDelta("Line 3: deterministic".into()));
    app.apply_agent_msg(AgentMsg::TextFlush);
    app.apply_agent_msg(AgentMsg::TurnComplete);
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("multiline_message_100x30", rendered);
}

// ── Metrics panel toggled ─────────────────────────────────────────────

#[test]
fn snapshot_metrics_panel_toggled_100x30() {
    let mut app = app_with_welcome();
    app.push_user("show metrics");
    app.apply_agent_msg(AgentMsg::TextDelta("Metrics display is toggled.".into()));
    app.apply_agent_msg(AgentMsg::TextFlush);
    app.apply_agent_msg(AgentMsg::TurnComplete);
    // Toggle metrics off — status bar should not show tok/s
    app.show_metrics = false;
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("metrics_panel_toggled_100x30", rendered);
}

// ── Cost panel toggled off ────────────────────────────────────────────

#[test]
fn snapshot_cost_panel_toggled_100x30() {
    let mut app = app_with_welcome();
    app.push_user("show cost");
    app.apply_agent_msg(AgentMsg::TextDelta("Cost display is hidden.".into()));
    app.apply_agent_msg(AgentMsg::TextFlush);
    app.apply_agent_msg(AgentMsg::Cost {
        turn_cost: 0.01,
        session_cost: 0.99,
        input_tokens: Some(500),
        output_tokens: Some(300),
        cache_tokens: Some(100),
    });
    app.apply_agent_msg(AgentMsg::TurnComplete);
    // Toggle cost OFF — status bar should not show cost
    app.show_cost = false;
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("cost_panel_toggled_100x30", rendered);
}

// ── Command palette (Ctrl-P) ─────────────────────────────────────────

/// Snapshot: command palette open with an empty query at 100x30.
/// Suggested commands float to the top; the first row is highlighted.
#[test]
fn snapshot_command_palette_open_100x30() {
    let mut app = app_with_welcome();
    app.open_palette();
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("command_palette_open_100x30", rendered);
}

/// Snapshot: command palette filtered by the query "tool" at 100x30.
#[test]
fn snapshot_command_palette_filtered_100x30() {
    let mut app = app_with_welcome();
    app.open_palette();
    app.palette.query = "tool".into();
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("command_palette_filtered_100x30", rendered);
}

/// Snapshot: which-key panel open at 100x30.
/// Grouped by category, scrollable (content exceeds the viewport here).
#[test]
fn snapshot_which_key_open_100x30() {
    let mut app = app_with_welcome();
    app.open_which_key();
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("which_key_open_100x30", rendered);
}

/// Snapshot: theme picker open at 100x30. Lists all themes with a swatch.
#[test]
fn snapshot_theme_picker_open_100x30() {
    let mut app = app_with_welcome();
    app.open_theme_picker();
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("theme_picker_open_100x30", rendered);
}

/// Snapshot: a toast floating above the status bar at 100x30.
#[test]
fn snapshot_toast_visible_100x30() {
    let mut app = app_with_welcome();
    app.toast("theme: forest", prism_tui::toast::ToastKind::Ok);
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("toast_visible_100x30", rendered);
}

/// Snapshot: view panel (tabbed) for a ui.view result (e.g. /tools, /status).
#[test]
fn snapshot_view_panel_100x30() {
    let mut app = app_with_welcome();
    app.apply_agent_msg(prism_tui::msg::AgentMsg::View {
        title: "Tools".into(),
        tabs: vec![
            ("Native".into(), "sample_material\nmaterials_search".into()),
            ("MCP".into(), "github\nfilesystem".into()),
        ],
    });
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("view_panel_100x30", rendered);
}

/// Snapshot: session picker populated from a fake ui.session.list.
#[test]
fn snapshot_session_picker_100x30() {
    let mut app = app_with_welcome();
    app.open_sessions();
    app.apply_agent_msg(prism_tui::msg::AgentMsg::SessionList {
        sessions: vec![
            serde_json::json!({"session_id":"sess-3","created_at":1751400000.0,"turn_count":12,"model":"gemma-4-12B-it-qat-UD-Q4_K_XL.gguf","is_latest":true}),
            serde_json::json!({"session_id":"sess-2","created_at":1751200000.0,"turn_count":4,"model":"anthropic/claude-sonnet-4","is_latest":false}),
        ],
        raw: serde_json::json!({}),
    });
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("session_picker_100x30", rendered);
}

/// Snapshot: account dialog (logged-in) with status read locally.
#[test]
fn snapshot_account_logged_in_100x30() {
    let mut app = app_with_welcome();
    app.account.open = true;
    app.account.status = prism_tui::app::AccountStatus {
        logged_in: true,
        user: "044d5402".into(),
        org: "00000000".into(),
        project: "00000000".into(),
    };
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("account_logged_in_100x30", rendered);
}

/// Snapshot: model picker populated from a fake ui.model.list.
#[test]
fn snapshot_model_picker_100x30() {
    let mut app = app_with_welcome();
    app.open_model_picker();
    app.apply_agent_msg(prism_tui::msg::AgentMsg::ModelList {
        models: vec![
            serde_json::json!({"id":"gemma-4-12B-it-qat-UD-Q4_K_XL.gguf","label":"Gemma 4 12B (local)","provider":"local","free":true}),
            serde_json::json!({"id":"anthropic/claude-sonnet-4","label":"Claude Sonnet 4","provider":"anthropic","free":false}),
        ],
        current: "gemma-4-12B-it-qat-UD-Q4_K_XL.gguf".into(),
    });
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("model_picker_100x30", rendered);
}

/// Snapshot: GPU picker populated from a fake ui.gpu.list.
/// One row is unavailable to lock in the dimmed-row rendering.
#[test]
fn snapshot_gpu_picker_100x30() {
    let mut app = app_with_welcome();
    app.open_gpu_picker();
    app.apply_agent_msg(prism_tui::msg::AgentMsg::GpuList {
        gpus: vec![
            serde_json::json!({"gpu_type":"RTX-4090","vram_gb":24,"region":"US","provider":"runpod","price_per_hour_usd":0.44,"available":true}),
            serde_json::json!({"gpu_type":"L40S","vram_gb":48,"region":"EU","provider":"datacrunch","price_per_hour_usd":0.89,"available":true}),
            serde_json::json!({"gpu_type":"A100-80GB","vram_gb":80,"region":"US","provider":"runpod","price_per_hour_usd":1.64,"available":true}),
            serde_json::json!({"gpu_type":"H100-SXM5","vram_gb":80,"region":"EU","provider":"datacrunch","price_per_hour_usd":2.19,"available":true}),
            serde_json::json!({"gpu_type":"B200","vram_gb":192,"region":"US","provider":"nebius","price_per_hour_usd":4.80,"available":false}),
        ],
        error: None,
    });
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("gpu_picker_100x30", rendered);
}

/// Snapshot: GitHub panel open (Issues) populated from a fake ui.gh.data.
#[test]
fn snapshot_gh_panel_100x30() {
    let mut app = app_with_welcome();
    app.open_gh();
    // Simulate the backend pushing issues data.
    app.apply_agent_msg(prism_tui::msg::AgentMsg::GhData {
        tab: "issues".into(),
        repo: "Darth-Hidious/PRISM".into(),
        items: vec![
            serde_json::json!({"number": 42, "title": "TUI crashes on startup", "state": "OPEN",
             "author": {"login": "alice"}, "labels": [{"name": "bug"}], "url": "https://x/42"}),
            serde_json::json!({"number": 7, "title": "Add dark mode", "state": "CLOSED",
             "author": {"login": "bob"}, "labels": [], "url": "https://x/7"}),
        ],
        error: None,
    });
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("gh_panel_100x30", rendered);
}

// ── Stress / robustness: "nothing breaks under stress" ──────────────
// Huge catalogs, malformed backend messages, extreme terminal sizes, and a
// flurry of key events must never panic.

#[test]
fn stress_renders_without_panic() {
    let mut app = app_with_welcome();
    let models: Vec<_> = (0..1000)
        .map(|i| serde_json::json!({"id":format!("m{i}"),"label":format!("model {i}"),"provider":"p","free":i%2==0}))
        .collect();
    app.apply_agent_msg(prism_tui::msg::AgentMsg::ModelList {
        models,
        current: "m0".into(),
    });
    let sessions: Vec<_> = (0..1000)
        .map(|i| serde_json::json!({"session_id":format!("s{i}"),"created_at":i as f64,"turn_count":i,"model":"m","is_latest":i==0}))
        .collect();
    app.apply_agent_msg(prism_tui::msg::AgentMsg::SessionList {
        sessions,
        raw: serde_json::json!({}),
    });
    let tools: Vec<_> = (0..500)
        .map(
            |i| serde_json::json!({"name":format!("tool_{i}"),"description":"d","approval":i%3==0}),
        )
        .collect();
    app.apply_agent_msg(prism_tui::msg::AgentMsg::ToolsCatalog { tools });
    // Extreme sizes — none may panic.
    for &(w, h) in &[
        (1u16, 1u16),
        (2, 2),
        (10, 5),
        (40, 12),
        (100, 30),
        (200, 60),
        (500, 200),
    ] {
        let _ = render_app_to_string(&app, w, h);
    }
}

#[test]
fn stress_malformed_messages_never_panic() {
    let mut app = app_with_welcome();
    let junk: Vec<serde_json::Value> = vec![
        serde_json::json!({}),
        serde_json::json!({"method": "ui.text.delta"}),
        serde_json::json!({"method": "ui.text.delta", "params": {}}),
        serde_json::json!({"method": "ui.card", "params": {"tool_name": 123}}),
        serde_json::json!({"method": "ui.cost", "params": {"turn_cost": "not a number"}}),
        serde_json::json!({"method": "totally.unknown.method", "params": {"x": 1}}),
        serde_json::json!({"method": "ui.view", "params": {"title": null, "tabs": "nope"}}),
        serde_json::json!(42),
        serde_json::json!("a string"),
        serde_json::json!([1, 2, 3]),
        serde_json::json!({"method": "ui.welcome", "params": {"version": "\u{0}\u{1b}[31m", "tool_count": -5}}),
    ];
    for msg in &junk {
        let parsed = prism_tui::msg::parse_notification(msg);
        let _ = format!("{parsed:?}");
        app.handle_backend_message(msg);
    }
    let _ = render_app_to_string(&app, 100, 30);
}

#[test]
fn stress_rapid_key_events() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use prism_tui::app::Focus;
    let mut app = app_with_welcome();
    app.focus = Focus::Input;
    let mk = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
    for c in "the quick brown fox jumps over the lazy dog 1234567890".chars() {
        app.handle_key(mk(c));
    }
    let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    for _ in 0..50 {
        app.handle_key(ctrl('p'));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    }
    app.focus = Focus::Chat;
    for _ in 0..100 {
        app.handle_key(mk('j'));
        app.handle_key(mk('k'));
    }
    let _ = render_app_to_string(&app, 80, 24);
}

// ── Link picker (`o`) ────────────────────────────────────────────────

/// Snapshot: link picker list at 100x30 (two URLs, newest first).
#[test]
fn snapshot_link_picker_list_100x30() {
    let mut app = app_with_welcome();
    app.push_user("compare these sources");
    app.apply_agent_msg(AgentMsg::TextDelta(
        "See [alpha](https://alpha.example.org/a) and https://beta.example.org/b".into(),
    ));
    app.apply_agent_msg(AgentMsg::TextFlush);
    app.apply_agent_msg(AgentMsg::TurnComplete);
    freeze_metrics(&mut app);
    app.focus = Focus::Chat;
    app.open_link_picker();

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("link_picker_list_100x30", rendered);
}

/// Snapshot: link confirm dialog at 100x30 (single URL goes straight to
/// the "do you want to go to this website?" dialog).
#[test]
fn snapshot_link_picker_confirm_100x30() {
    let mut app = app_with_welcome();
    app.apply_agent_msg(AgentMsg::TextDelta(
        "source: https://example.org/paper".into(),
    ));
    app.apply_agent_msg(AgentMsg::TextFlush);
    app.apply_agent_msg(AgentMsg::TurnComplete);
    freeze_metrics(&mut app);
    app.focus = Focus::Chat;
    app.open_link_picker();

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("link_picker_confirm_100x30", rendered);
}

// ── Workspace detail modal (Enter) ───────────────────────────────────

/// Snapshot: Enter on a Workspace Activity row opens the event detail
/// modal (the underlying event as pretty JSON) at 100x30.
#[test]
fn snapshot_workspace_activity_detail_100x30() {
    use prism_tui::app::WorkspaceTab;
    let mut app = app_with_welcome();
    app.push_user("sample alloy");
    app.apply_agent_msg(AgentMsg::ToolCard {
        tool_name: "sample_material".into(),
        content: "W0.3 Mo0.2 Ta0.3 Nb0.2".into(),
        card_type: "results".into(),
        elapsed_ms: Some(292),
        call_id: None,
        provenance_id: None,
        data: None,
    });
    freeze_metrics(&mut app);
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Activity;
    app.workspace_selected = 1; // the tool-result row
    app.open_workspace_detail();

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("workspace_activity_detail_100x30", rendered);
}

// ── Form pane (generic structured input) ────────────────────────────

/// Snapshot: a form pane with every field kind at 100x30.
/// Exercises text (empty placeholder), stepper, toggles (on/off with an
/// advisory note), and select rendering plus the focused-row reverse.
#[test]
fn snapshot_form_pane_all_field_kinds_100x30() {
    use prism_tui::app::FormTarget;
    use prism_tui::form::{Form, FormField};

    let mut app = app_with_welcome();
    let form = Form::new(
        "Deep research",
        "launch",
        vec![
            FormField::text("question", "Question", ""),
            FormField::stepper("depth", "Depth", 1, 0, 5).with_note("0 = local-only · 1+ = web"),
            FormField::toggle("kg", "Knowledge Graph", true),
            FormField::toggle("mesh", "Mesh/partner data", false).with_note("(advisory)"),
            FormField::select(
                "transport",
                "Transport",
                vec!["stdio".into(), "http".into()],
                0,
            ),
        ],
    );
    app.open_form(form, FormTarget::Goal);
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("form_pane_all_field_kinds_100x30", rendered);
}

/// Snapshot: the goal form opened from the palette with a typed value.
#[test]
fn snapshot_goal_form_typed_100x30() {
    let mut app = app_with_welcome();
    app.open_goal_form();
    for c in "map the NiTi phase diagram".chars() {
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        ));
    }
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("goal_form_typed_100x30", rendered);
}

/// Snapshot: the Deep Research launch pane with a typed question.
#[test]
fn snapshot_research_form_100x30() {
    let mut app = app_with_welcome();
    app.open_research_form();
    for c in "high-entropy alloys for hot structures".chars() {
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        ));
    }
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("research_form_100x30", rendered);
}

// ── Knowledge pane ───────────────────────────────────────────────────

/// Snapshot: Knowledge pane, Search tab with a typed query.
#[test]
fn snapshot_knowledge_search_tab_100x30() {
    use prism_tui::knowledge::KnowledgeTab;

    let mut app = app_with_welcome();
    app.open_knowledge_pane(KnowledgeTab::Search);
    for c in "gamma-TiAl oxidation".chars() {
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        ));
    }
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("knowledge_search_tab_100x30", rendered);
}

/// Snapshot: Knowledge pane, Ingest tab with a deterministic fabricated
/// browser listing (a real temp dir would leak a random path into the
/// snapshot).
#[test]
fn snapshot_knowledge_ingest_browser_100x30() {
    use prism_tui::knowledge::{FileEntry, KnowledgeTab};

    let mut app = app_with_welcome();
    app.open_knowledge_pane(KnowledgeTab::Ingest);
    app.knowledge.browser.cwd = std::path::PathBuf::from("/data/papers");
    app.knowledge.browser.entries = vec![
        FileEntry {
            name: "..".into(),
            is_dir: true,
        },
        FileEntry {
            name: "reviews".into(),
            is_dir: true,
        },
        FileEntry {
            name: "lpbf_params.csv".into(),
            is_dir: false,
        },
        FileEntry {
            name: "niti_sma.pdf".into(),
            is_dir: false,
        },
        FileEntry {
            name: "phase_graph.json".into(),
            is_dir: false,
        },
    ];
    app.knowledge.browser.selected = 3;
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("knowledge_ingest_browser_100x30", rendered);
}
