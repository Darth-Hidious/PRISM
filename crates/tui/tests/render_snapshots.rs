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
        tool_name: "alloy_sample".into(),
        verb: "Running".into(),
        call_id: Some("call-1".into()),
        preview: Some("{\"n\": 10}".into()),
        approval_required: Some(false),
    });
    app.apply_agent_msg(AgentMsg::ToolCard {
        tool_name: "alloy_sample".into(),
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
