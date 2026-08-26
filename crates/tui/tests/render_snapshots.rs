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
use prism_tui::artifact::{ArtifactPromotion, ArtifactStoreState, WorkspaceArtifact};
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
///
/// Snapshots capture post-launch UI states, so this dismisses the Mission
/// Control home (the launch overlay). The launch home itself is captured by
/// `snapshot_home_launch_100x30`.
fn fake_app() -> App {
    let mut app = App::new(BackendHandle::fake(FakeScenario::BasicChat));
    app.home.open = false;
    app
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

/// Snapshot: the Mission Control home — the actual launch screen (chat demoted).
#[test]
fn snapshot_home_launch_100x30() {
    let app = App::new(BackendHandle::fake(FakeScenario::BasicChat));
    assert!(app.home.open, "home must be open on launch");
    let rendered = render_app_to_string(&app, 100, 30);
    insta::assert_snapshot!("home_launch_100x30", rendered);
}

#[test]
fn first_screen_has_no_debug_text_stale_state_or_panel_overlap() {
    let mut app = App::new(BackendHandle::fake(FakeScenario::BasicChat));
    app.handle_backend_message(&serde_json::json!({
        "method": "ui.welcome",
        "params": {"version": "1.0.0", "tool_count": 164}
    }));
    let tools = (0..164)
        .map(|index| {
            serde_json::json!({
                "name": format!("tool-{index}"),
                "approval": index < 55
            })
        })
        .collect::<Vec<_>>();
    app.handle_backend_message(&serde_json::json!({
        "method": "ui.tools.catalog",
        "params": {"tools": tools}
    }));
    app.handle_backend_message(&serde_json::json!({
        "method": "ui.status",
        "params": {
            "model": "qwen2.5:3b",
            "session_mode": "chat",
            "message_count": 0
        }
    }));

    let rendered = render_app_to_string(&app, 150, 42);
    assert!(
        rendered.lines().all(|line| !line.contains("tui-dbg")),
        "debug output leaked into rendered lines:\n{rendered}"
    );
    assert!(!rendered.contains("loading tool catalog…"), "{rendered}");
    // The catalog HAS loaded (above), and the home view says so without
    // reciting the inventory.
    assert!(rendered.contains("tools ready"), "{rendered}");
    // Regression guard for a deliberate product decision: the tool COUNT is
    // not advertised on any ambient surface. A headline "164 tools" invites
    // being asked about all 164, and the number is implementation detail
    // rather than a capability anyone can act on. It stays one keypress away
    // in the tools pane (`t`) and in the usage stats — surfaces reached by
    // asking. Nothing is hidden; it is simply not shouted.
    assert!(
        !rendered.contains("164 tools"),
        "the tool count must not appear on the first screen:\n{rendered}"
    );
    assert!(
        !rendered.contains("need approval"),
        "the approval split must not appear on the first screen:\n{rendered}"
    );
    assert!(!rendered.contains("model: —"), "{rendered}");
    assert_eq!(
        rendered.matches("qwen2.5:3b").count(),
        3,
        "header, home, and footer must show the same model:\n{rendered}"
    );
    assert!(rendered.contains("session cost  $0.0000"), "{rendered}");
    assert!(
        rendered.contains("credits  not reported (unauthed / not fetched)"),
        "{rendered}"
    );
    assert!(rendered.contains("Ctrl-C quit"), "{rendered}");
    assert!(!rendered.contains("C rl-C quit"), "{rendered}");

    for (width, height) in [(150, 42), (100, 30), (200, 60), (40, 12)] {
        let rendered = render_app_to_string(&app, width, height);
        let lines = rendered.lines().collect::<Vec<_>>();
        // Mirror of the layout rule in render::draw — the sidebar is hidden
        // below 100 columns, and where it is shown a 1-column gap separates
        // it from the content boxes.
        let sidebar_width = if width >= 100 {
            (width / 3).clamp(24, 42)
        } else {
            0
        };
        let prompt_right = if sidebar_width > 0 {
            usize::from(width - sidebar_width - 2)
        } else {
            usize::from(width - 1)
        };
        let prompt_top = usize::from(height - 6);
        let prompt_bottom = usize::from(height - 2);
        assert_eq!(
            lines[prompt_top].chars().nth(prompt_right),
            Some('┐'),
            "home panel overwrote the Prompt top-right corner at {width}x{height}:\n{rendered}"
        );
        assert_eq!(
            lines[prompt_bottom].chars().nth(prompt_right),
            Some('┘'),
            "home panel overwrote the Prompt bottom-right corner at {width}x{height}:\n{rendered}"
        );
    }
}

#[test]
fn backend_message_ingress_has_no_direct_terminal_debug_writes() {
    let source = include_str!("../src/app.rs");
    assert!(
        !source.contains("eprintln!"),
        "App must not write behind Ratatui"
    );
    assert!(!source.contains("tui-dbg"), "debug marker must not ship");
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
        session_id: None,
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
        session_id: None,
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
        session_id: None,
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
        session_id: None,
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
        data: Some(serde_json::json!({"evidence_class": "screening"})),
    });
    app.apply_agent_msg(AgentMsg::TurnComplete);
    app.tokens_per_sec = 0.0;
    app.first_token_time = None;
    app.last_token_time = None;
    app.tokens_received = 0;

    let rendered = render_app_to_string(&app, 100, 30);
    assert!(rendered.contains("[YELLOW screening]"), "{rendered}");
    insta::assert_snapshot!("tool_success_100x30", rendered);
}

/// Snapshot: tool error at 100x30.
#[test]
fn snapshot_tool_error_100x30() {
    let mut app = fake_app();
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1-fake".into(),
        tool_count: 99,
        session_id: None,
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
        session_id: None,
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

/// Snapshot: `notebook_exec` approval popup shows the FULL cell code — the
/// kernel is shared with the human, so consent must be informed (a 60-char
/// first-line preview could hide `print(api_key)` on line two).
#[test]
fn snapshot_notebook_exec_approval_code_popup_100x30() {
    let mut app = fake_app();
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1-fake".into(),
        tool_count: 99,
        session_id: None,
    });
    app.apply_agent_msg(AgentMsg::Status {
        model: "fake-backend".into(),
        mode: "chat".into(),
        message_count: 1,
    });
    app.push_user("analyze the data");
    app.apply_agent_msg(AgentMsg::ApprovalPrompt {
        tool_name: "notebook_exec".into(),
        call_id: Some("call-7".into()),
        message: "Allow notebook_exec?".into(),
        tool_args: Some(serde_json::json!({
            "code": "import os\nsecrets = {k: v for k, v in os.environ.items()}\nprint(secrets)",
            "reset": false,
        })),
        tool_description: Some("Execute Python in the shared notebook kernel".into()),
        requires_approval: Some(true),
        permission_mode: Some("full_access".into()),
        choices: vec!["y".into(), "n".into(), "a".into()],
        prompt_type: Some("approval".into()),
    });
    app.tokens_per_sec = 0.0;
    app.first_token_time = None;
    app.last_token_time = None;
    app.tokens_received = 0;

    let rendered = render_app_to_string(&app, 100, 30);
    // Every line of the cell must be visible, not just a truncated head.
    assert!(rendered.contains("import os"));
    assert!(rendered.contains("secrets = {k: v for k, v in os.environ.items()}"));
    assert!(rendered.contains("print(secrets)"));
    insta::assert_snapshot!("notebook_exec_approval_code_popup_100x30", rendered);
}

/// Snapshot: cost metrics at 100x30.
#[test]
fn snapshot_cost_metrics_100x30() {
    let mut app = fake_app();
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1-fake".into(),
        tool_count: 99,
        session_id: None,
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
        session_id: None,
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
        session_id: None,
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

/// The workspace tab strip must never WRAP, at any terminal width.
///
/// Adding a fourth tab pushed the full strip (`[Activity] Tools Files
/// Objects`, 31 columns) past the sidebar width on a small terminal. The
/// paragraph wrapped, "Objects" landed on its own line, and it silently cost a
/// row of panel content — every entry shifted down. That regression shipped
/// inside a 41-file bulk snapshot update and a reviewer caught it, not the
/// suite: a snapshot records whatever it is given, so it cannot object to a
/// layout getting worse. This asserts the invariant directly.
///
/// With six tabs the ladder is three-letter labels, then two-letter initials
/// (the full words can never fit the 42-column sidebar ceiling).
///
/// Below 100 columns the sidebar is hidden outright, so there is no strip to
/// wrap; the invariant that matters there is absence, asserted first.
#[test]
fn workspace_tab_strip_never_wraps_at_any_width() {
    // Below 100 columns the sidebar is hidden entirely — degrade, don't clip.
    for (w, h) in [(40, 12), (60, 20), (99, 30)] {
        let app = fake_app();
        let rendered = render_app_to_string(&app, w, h);
        assert!(
            !["[Activity]", "[Act]", "[Ac]"]
                .iter()
                .any(|s| rendered.contains(s)),
            "sidebar must be hidden below 100 columns, but a tab strip rendered at {w}x{h}"
        );
    }
    for (w, h) in [(100, 30), (200, 60)] {
        let app = fake_app();
        let rendered = render_app_to_string(&app, w, h);
        let strip = rendered
            .lines()
            .find(|l| l.contains("[Activity]") || l.contains("[Act]") || l.contains("[Ac]"))
            .unwrap_or_else(|| panic!("no workspace tab strip rendered at {w}x{h}"));
        // All six tabs must sit on that ONE line. If the strip wrapped, the
        // trailing tab is on the next line and this fails.
        for label in [
            ["Activity", "Act", "Ac"],
            ["Tools", "Too", "To"],
            ["Files", "Fil", "Fi"],
            ["Objects", "Obj", "Ob"],
            ["Structures", "Str", "St"],
            ["Artifacts", "Art", "Ar"],
        ] {
            assert!(
                label.iter().any(|variant| strip.contains(variant)),
                "tab `{}` missing from the strip at {w}x{h} — it wrapped \
                 onto another line and stole a row of panel content.\n\
                 strip: {strip:?}",
                label[0]
            );
        }
    }
}

/// The acceptance criterion for the whole Objects tab: **a running object must
/// never render as complete.**
///
/// `object_update_running_never_renders_as_complete` in tests/unit.rs carries
/// that name but asserts only model state — it never renders anything, and it
/// cannot, because the render harness (`render_app_to_string`) lives here. A
/// test named after a render invariant that never renders is how the invariant
/// goes unguarded. This checks the pixels.
///
/// The hard case is a job at FULL progress that has not reported completion —
/// 10000/10000 and still `running`. That is exactly when a reader (or a
/// rounding bug) is most tempted to call it done, and exactly when the user
/// would stop waiting for a result that has not arrived.
#[test]
fn a_running_object_never_renders_as_done() {
    use prism_tui::app::WorkspaceTab;
    let mut app = app_with_welcome();
    app.workspace_tab = WorkspaceTab::Objects;
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "sim-full".into(),
        kind: "simulation".into(),
        label: "MD NPT 300K".into(),
        status: "running".into(),
        progress_current: Some(10_000),
        progress_total: Some(10_000),
        detail: None,
    });

    let rendered = render_app_to_string(&app, 100, 30);
    let row = rendered
        .lines()
        .find(|l| l.contains("MD NPT 300K"))
        .expect("the running object must be rendered at all");

    assert!(
        !row.contains("done"),
        "a running object rendered as done — the user stops waiting for a \
         result that has not arrived.\nrow: {row:?}"
    );
    assert!(
        row.contains("100%") || row.contains("running"),
        "a running object must render its progress or the word running.\nrow: {row:?}"
    );
}

/// Snapshot: wide terminal basic chat at 200x60.
#[test]
fn snapshot_wide_terminal_basic_chat_200x60() {
    let mut app = fake_app();
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1-fake".into(),
        tool_count: 99,
        session_id: None,
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
        session_id: None,
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
        rpc_id: None,
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
        data: Some(serde_json::json!({
            "evidence_class": "reference_validated"
        })),
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
        data: Some(serde_json::json!({
            "evidence_class": "reference_validated"
        })),
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
        notice: None,
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

/// Snapshot: Nodes view populated from a fake ui.nodes.list.
/// Covers online (GPU + CPU) / provisioning / offline states and a sparse
/// profile — all time-independent so the snapshot stays deterministic.
#[test]
fn snapshot_node_picker_100x30() {
    let mut app = app_with_welcome();
    app.open_node_picker();
    app.apply_agent_msg(prism_tui::msg::AgentMsg::NodeList {
        nodes: vec![
            serde_json::json!({"name":"lab-hpc-01","status":"online","visibility":"org","last_seen_at":"2026-04-06T02:00:00Z","profile":{"cpu_cores":64,"ram_gb":256,"gpus":["A100-80GB","A100-80GB"],"labels":{"arch":"x86_64"}}}),
            serde_json::json!({"name":"studio-mac","status":"online","visibility":"private","last_seen_at":"2026-04-06T02:00:00Z","profile":{"cpu_cores":12,"ram_gb":24,"labels":{"arch":"aarch64"}}}),
            serde_json::json!({"name":"edge-box-eu","status":"provisioning","visibility":"private","profile":{"cpu_cores":8,"ram_gb":16}}),
            serde_json::json!({"name":"old-worker","status":"offline","visibility":"private","profile":{}}),
        ],
        error: None,
    });
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("node_picker_100x30", rendered);
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
        notice: None,
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
        data: Some(serde_json::json!({"evidence_class": "screening"})),
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

/// Snapshot: the Objects tab with a mix of running, completed, and failed
/// domain objects at 100x30.
#[test]
fn snapshot_workspace_objects_tab_100x30() {
    use prism_tui::app::WorkspaceTab;
    let mut app = app_with_welcome();
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "obj-1".into(),
        kind: "structure".into(),
        label: "W-BCC a=3.14A".into(),
        status: "completed".into(),
        progress_current: None,
        progress_total: None,
        detail: Some("E=-8.42 eV/atom".into()),
    });
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "obj-2".into(),
        kind: "simulation".into(),
        label: "MD NPT 300K 10000 steps".into(),
        status: "running".into(),
        progress_current: Some(5000),
        progress_total: Some(10000),
        detail: None,
    });
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "obj-3".into(),
        kind: "alloy".into(),
        label: "CrMnFeCoNi HEA".into(),
        status: "completed".into(),
        progress_current: None,
        progress_total: None,
        detail: Some("5 candidates".into()),
    });
    // Tag the alloy for the agent.
    app.objects[2].tagged = true;
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "obj-4".into(),
        kind: "simulation".into(),
        label: "MD NVT 500K".into(),
        status: "failed".into(),
        progress_current: None,
        progress_total: None,
        detail: Some("divergence at step 2341".into()),
    });
    freeze_metrics(&mut app);
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Objects;
    app.workspace_selected = 1; // the running sim

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("workspace_objects_tab_100x30", rendered);
}

/// Snapshot: the Objects tab when empty — must say so plainly.
#[test]
fn snapshot_workspace_objects_empty_100x30() {
    use prism_tui::app::WorkspaceTab;
    let mut app = app_with_welcome();
    freeze_metrics(&mut app);
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Objects;

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("workspace_objects_empty_100x30", rendered);
}

/// Snapshot: artifact metadata keeps KG promotion visible at a glance while
/// also showing tool, summary, record count, size, and age.
#[test]
fn snapshot_workspace_artifacts_entries_100x30() {
    use prism_tui::app::WorkspaceTab;
    let mut app = app_with_welcome();
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Artifacts;
    app.workspace_selected = 0;
    app.artifact_store = ArtifactStoreState::Ready(vec![
        WorkspaceArtifact {
            id: "art_promoted".into(),
            tool: "materials_search".into(),
            summary: "24 refractory alloy candidates with complete property rows".into(),
            record_count: Some(24),
            bytes_size: 18_432,
            created_at: "2026-08-11T11:58:00+00:00".into(),
            age: "2m ago".into(),
            promotion: ArtifactPromotion::Promoted,
            session_id: "session-artifacts".into(),
        },
        WorkspaceArtifact {
            id: "art_local".into(),
            tool: "phase_diagram".into(),
            summary: "Calculated binary phase boundaries for inspection".into(),
            record_count: None,
            bytes_size: 2_048,
            created_at: "2026-08-11T11:45:00+00:00".into(),
            age: "15m ago".into(),
            promotion: ArtifactPromotion::NotPromoted,
            session_id: "session-artifacts".into(),
        },
    ]);

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("workspace_artifacts_entries_100x30", rendered);
}

/// Snapshot: a healthy store with no rows must not resemble a failed store.
#[test]
fn snapshot_workspace_artifacts_empty_100x30() {
    use prism_tui::app::WorkspaceTab;
    let mut app = app_with_welcome();
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Artifacts;
    app.artifact_store = ArtifactStoreState::Ready(Vec::new());

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("workspace_artifacts_empty_100x30", rendered);
}

/// Snapshot: a store-open failure is explicit and visually distinct from an
/// empty, healthy session.
#[test]
fn snapshot_workspace_artifacts_unavailable_100x30() {
    use prism_tui::app::WorkspaceTab;
    let mut app = app_with_welcome();
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Artifacts;
    app.artifact_store =
        ArtifactStoreState::Unavailable("database could not be opened: permission denied".into());

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("workspace_artifacts_unavailable_100x30", rendered);
}

// ── Workspace Structures tab (the materials plane) ────────────────

/// One structures row in the wire shape `structure_io.py`'s meta defines.
fn structure_row(
    cache_key: &str,
    formula: &str,
    n_atoms: u64,
    composition: serde_json::Value,
    source: &str,
) -> serde_json::Value {
    serde_json::json!({
        "cache_key": cache_key,
        "cache_ref": format!("cache://{cache_key}/structure.cif"),
        "tool": "structure_import",
        "formula": formula,
        "n_atoms": n_atoms,
        "composition": composition,
        "source": source,
        "created_at": "2026-08-11T09:14:00+00:00",
    })
}

/// Snapshot: structures read formula-first, with atom count, composition,
/// source (verbatim — a user import is not a database lookup), and the
/// cache:// reference.
#[test]
fn snapshot_workspace_structures_entries_100x30() {
    use prism_tui::app::WorkspaceTab;
    use prism_tui::structures::StructuresStoreState;

    let mut app = app_with_welcome();
    app.session_id = Some("session-structures".into());
    app.apply_agent_msg(AgentMsg::StructuresListed {
        session_id: "session-structures".into(),
        structures: vec![
            structure_row(
                "0f7a1c2e9b4d4a6f",
                "TiAl",
                2,
                serde_json::json!({"Al": 1, "Ti": 1}),
                "user_import",
            ),
            structure_row(
                "1a2b3c4d5e6f7081",
                "MgB2",
                3,
                serde_json::json!({"B": 2, "Mg": 1}),
                "materials_project",
            ),
            structure_row(
                "9e8d7c6b5a493827",
                "W2",
                2,
                serde_json::json!({"W": 2}),
                "mace_relaxation",
            ),
        ],
    });
    assert!(matches!(
        &app.structure_store,
        StructuresStoreState::Ready(rows) if rows.len() == 3
    ));
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Structures;
    app.workspace_selected = 0;

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("workspace_structures_entries_100x30", rendered);
}

/// Snapshot: a healthy cache with no structures must not resemble a failed
/// cache — it says "no structures yet" and why they would appear.
#[test]
fn snapshot_workspace_structures_empty_100x30() {
    use prism_tui::app::WorkspaceTab;

    let mut app = app_with_welcome();
    app.session_id = Some("session-structures".into());
    app.apply_agent_msg(AgentMsg::StructuresListed {
        session_id: "session-structures".into(),
        structures: vec![],
    });
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Structures;

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("workspace_structures_empty_100x30", rendered);
}

/// Snapshot: a cache that could not be queried is explicit and visually
/// distinct from an empty, healthy session.
#[test]
fn snapshot_workspace_structures_unavailable_100x30() {
    use prism_tui::app::WorkspaceTab;

    let mut app = app_with_welcome();
    app.session_id = Some("session-structures".into());
    app.apply_agent_msg(AgentMsg::StructureStoreUnavailable {
        message: "structure cache directory could not be opened: permission denied".into(),
    });
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Structures;

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("workspace_structures_unavailable_100x30", rendered);
}

/// Snapshot: selecting a structure opens the scrollable CIF detail — the
/// actual text, with the meta PRISM has (full cache:// ref included).
#[test]
fn snapshot_workspace_structures_selected_cif_100x30() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use prism_tui::app::WorkspaceTab;

    let mut app = app_with_welcome();
    app.session_id = Some("session-structures".into());
    app.apply_agent_msg(AgentMsg::StructuresListed {
        session_id: "session-structures".into(),
        structures: vec![structure_row(
            "0f7a1c2e9b4d4a6f",
            "TiAl",
            2,
            serde_json::json!({"Al": 1, "Ti": 1}),
            "user_import",
        )],
    });
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Structures;

    // Real interaction path: Enter requests the CIF, the fetch reply
    // fills the existing view panel.
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.view.open);
    app.apply_agent_msg(AgentMsg::StructureFetched {
        session_id: "session-structures".into(),
        cache_key: "0f7a1c2e9b4d4a6f".into(),
        cif: "\
data_TiAl
_chemical_formula_sum \"Al1 Ti1\"
_chemical_name_common \"TiAl gamma\"
_cell_length_a 4.005
_cell_length_b 4.005
_cell_length_c 4.171
_cell_angle_alpha 90.0
_cell_angle_beta 90.0
_cell_angle_gamma 90.0
_symmetry_space_group_name_H-M \"P 4/m m m\"
loop_
_atom_site_label
_atom_site_type_symbol
_atom_site_fract_x
_atom_site_fract_y
_atom_site_fract_z
Ti1 Ti 0.00000 0.00000 0.00000
Al1 Al 0.50000 0.50000 0.50000
"
        .into(),
        truncated: false,
    });

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("workspace_structures_selected_cif_100x30", rendered);
}

/// Snapshot: missing meta fields render as unknown — a formula PRISM did
/// not compute is never invented.
#[test]
fn snapshot_workspace_structures_missing_meta_unknown_100x30() {
    use prism_tui::app::WorkspaceTab;

    let mut app = app_with_welcome();
    app.session_id = Some("session-structures".into());
    app.apply_agent_msg(AgentMsg::StructuresListed {
        session_id: "session-structures".into(),
        // Only identity — the cache holds this entry, but no meta writer
        // recorded formula, atom count, composition, or source.
        structures: vec![serde_json::json!({
            "cache_key": "beef0000deadbeef",
            "cache_ref": "cache://beef0000deadbeef/structure.cif",
        })],
    });
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Structures;

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    assert!(
        rendered.contains("unknown"),
        "missing text fields must read as unknown:\n{rendered}"
    );
    assert!(
        rendered.contains("? atoms"),
        "a missing atom count must not become a number:\n{rendered}"
    );
    insta::assert_snapshot!("workspace_structures_missing_meta_unknown_100x30", rendered);
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

// ── Overlay geometry ────────────────────────────────────────────────

/// Render to raw rows, one string per terminal line, **without** trimming
/// trailing spaces — column indices must line up with buffer cells for a
/// cell-exact comparison.
fn render_rows(app: &App, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("failed to create TestBackend");
    terminal.draw(|f| draw(f, app)).expect("failed to draw");
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|row| {
            (0..buffer.area.width)
                .map(|col| buffer[(col, row)].symbol())
                .collect::<String>()
        })
        .collect()
}

/// **An overlay may only repaint the transcript.** Every other widget on the
/// frame — the header bar, the bordered Prompt box, the footer and the
/// Workspace sidebar — is drawn *before* the overlay and owns its cells.
///
/// The Tools pane centred itself on the whole frame and opened with `Clear`,
/// so at 100x30 it wiped the prompt box down to the fragments `┌ Prompt` and
/// `│ Type a` on the left edge and cut the sidebar's border out of every row
/// it covered. That is the screen the owner photographed.
///
/// The invariant is stated as a diff: with the same `App` state, opening an
/// overlay must change **only** cells inside the transcript region. Anything
/// else means the overlay claimed area another widget owns.
#[test]
fn an_overlay_repaints_only_the_transcript_never_the_prompt_or_sidebar() {
    type Open = fn(&mut App);
    let overlays: [(&str, Open); 8] = [
        ("tools", |a| a.tools_window.open = true),
        ("status", |a| a.status_window.open = true),
        ("config", |a| a.config_window.open = true),
        ("model picker", |a| a.model_picker.open = true),
        ("theme picker", |a| a.theme_picker.open = true),
        ("which-key", |a| a.which_key.open = true),
        ("knowledge", |a| a.knowledge.open = true),
        ("command palette", |a| a.open_palette()),
    ];

    // The first three are realistic terminals with the sidebar shown (it
    // needs 100 columns), so the sidebar column is live and can be sliced.
    // The last two are below that threshold: no sidebar, but the prompt box
    // and footer still own their rows and the transcript is only five rows
    // tall — the size at which an overlay is most tempted to spill.
    for (width, height) in [(100, 30), (120, 40), (150, 42), (60, 20), (40, 12)] {
        let mut base_app = app_with_welcome();
        base_app.home.open = false;
        freeze_metrics(&mut base_app);
        let base = render_rows(&base_app, width, height);

        // Mirror of the layout rule in render::frame_layout: header row,
        // transcript, 5-row prompt box, footer row; the sidebar takes a third
        // of the width (clamped) and only above 100 columns.
        let transcript_rows = 1..usize::from(height) - 6;
        let sidebar_x = if width >= 100 {
            usize::from(width - (width / 3).clamp(24, 42))
        } else {
            usize::from(width)
        };

        for (name, open) in overlays {
            let mut app = app_with_welcome();
            app.home.open = false;
            freeze_metrics(&mut app);
            open(&mut app);
            let rows = render_rows(&app, width, height);

            for (y, (before, after)) in base.iter().zip(rows.iter()).enumerate() {
                if !transcript_rows.contains(&y) {
                    assert_eq!(
                        before, after,
                        "the {name} overlay repainted row {y} at {width}x{height} — that row \
                         belongs to the header, the Prompt box or the footer.\n\
                         without overlay: {before:?}\n   with overlay: {after:?}"
                    );
                    continue;
                }
                // Inside the transcript rows the overlay owns the content
                // column, but the sidebar still owns its own columns — that
                // is where the divider and the panel body live.
                let cut = |line: &str| -> String { line.chars().skip(sidebar_x).collect() };
                assert_eq!(
                    cut(before),
                    cut(after),
                    "the {name} overlay reached into the Workspace sidebar on row {y} \
                     at {width}x{height}.\n\
                     without overlay: {before:?}\n   with overlay: {after:?}"
                );
            }
        }
    }
}

/// Snapshot: the Tools pane open at 100x30 — the exact screen the owner
/// photographed. The picture that matters is the frame *around* the pane:
/// a whole `┌ Prompt ─…─┐` box, an unbroken sidebar divider on every row,
/// and the tab strip still on one line.
#[test]
fn snapshot_tools_pane_open_100x30() {
    let mut app = app_with_welcome();
    app.handle_backend_message(&serde_json::json!({
        "method": "ui.tools.catalog",
        "params": {"tools": [
            {"name": "execute_bash", "description": "Run a shell command", "approval": true},
            {"name": "execute_python", "description": "Run Python in the kernel", "approval": true},
            {"name": "materials_search", "description": "Search OPTIMADE providers", "approval": false},
            {"name": "plot", "description": "Render a chart", "approval": false},
        ]}
    }));
    freeze_metrics(&mut app);
    app.tools_window.open = true;

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("tools_pane_open_100x30", rendered);
}

/// A long reply must not push the reader's own turn off the top.
///
/// Auto-follow pinned the viewport to the last row, so any answer taller than
/// the transcript hid the prompt that caused it and the chat read as if it
/// contained only PRISM's half of the conversation. `push_user` was never at
/// fault — the message was above the fold.
///
/// Asserted on CONTENT rather than as a snapshot: the property is "the user's
/// turn is on screen", and a snapshot would also fail for an unrelated pixel
/// and re-accepting it would quietly retire the guarantee.
#[test]
fn a_long_reply_does_not_scroll_the_users_own_turn_off_screen() {
    let mut app = app_with_welcome();
    app.push_user("what is the solidus of Ti-6Al-4V");
    // Comfortably taller than a 30-row viewport.
    let long_reply: String = (1..=120)
        .map(|i| format!("line {i} of a long answer\n"))
        .collect();
    app.apply_agent_msg(AgentMsg::TextDelta(long_reply));
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert!(
        rendered.contains("❯ You"),
        "the user's own turn must stay visible under a long reply; got:\n{rendered}"
    );
    assert!(
        rendered.contains("what is the solidus"),
        "the user's TEXT must be visible, not just the header; got:\n{rendered}"
    );
}

/// Scrolling is the reader taking over: once they move, the anchor releases
/// and the view stops jumping back to their last turn on every redraw.
#[test]
fn scrolling_releases_the_user_turn_anchor() {
    let mut app = app_with_welcome();
    app.push_user("anchor me");
    assert!(
        app.anchor_user_turn.get(),
        "submitting a turn must arm the anchor"
    );
    // `j` only scrolls when the TRANSCRIPT has focus; with the prompt focused
    // it is just a character. The first version of this test missed that and
    // failed, which is itself the behaviour worth pinning.
    app.focus = prism_tui::app::Focus::Chat;
    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('j'),
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(
        !app.anchor_user_turn.get(),
        "a manual scroll must release the anchor rather than fighting the reader"
    );
}

/// The notebook DRAWS its newest figure instead of only naming the file.
///
/// `render.rs` used to emit `[plot saved: /path/cell-1-0.png]` and stop there.
/// The figure existed and the reader could not see it, so nothing in the pane
/// said whether the plot was right, empty, or upside down.
///
/// Driven with a path that does not exist, because that is the case with a
/// visible, assertable result on a `TestBackend`: pixels cannot be asserted in
/// a test, but the honest failure line can, and reaching it proves the draw
/// path ran rather than being skipped.
#[test]
fn the_notebook_draws_its_newest_figure_and_names_what_it_cannot_draw() {
    let mut app = app_with_welcome();
    let cell = prism_tui::notebook::NotebookCell::from_value(&serde_json::json!({
        "execution_count": 1,
        "origin": "agent",
        "code": "plt.plot(x, y)",
        "image_paths": ["/nonexistent/cell-1-0.png"],
        "success": true,
    }));
    app.notebook
        .apply_state(false, "kernel: idle".into(), vec![cell]);
    app.notebook.open = true;
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 40);
    assert!(
        rendered.contains("figure missing on disk"),
        "an undrawable figure must say so where the picture would be, not leave \
         a blank rectangle; got:\n{rendered}"
    );
    assert!(
        rendered.contains("cell-1-0.png"),
        "the failure must name the file so it can be chased; got:\n{rendered}"
    );
}

/// A table a TOOL produced must render as a table, exactly like one PRISM
/// wrote itself.
///
/// `markdown_lines` was reachable from a single place — the assistant-prose
/// branch of `draw_chat`. Every tool result took a different path and arrived
/// as flat text, so identical bytes rendered as a bordered, column-aligned
/// table when PRISM said them and as raw `|` pipes when a tool did. The
/// quality of the display depended on who was speaking, which is not a
/// distinction a reader cares about.
#[test]
fn a_table_from_a_tool_renders_as_a_table_not_as_pipes() {
    let mut app = app_with_welcome();
    app.apply_agent_msg(AgentMsg::ToolCard {
        tool_name: "compare_materials".into(),
        content: "comparison complete\n\n| alloy | density |\n|---|---|\n| Ti64 | 4.43 |\n| NbMoTaW | 13.7 |".into(),
        card_type: "results".into(),
        elapsed_ms: Some(12),
        call_id: None,
        provenance_id: None,
        data: None,
    });
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 30);
    assert!(
        rendered.contains('┌') && rendered.contains('│'),
        "a tool's table must be drawn with real borders; got:\n{rendered}"
    );
    assert!(
        !rendered.contains("|---|"),
        "the raw markdown separator row must not reach the screen; got:\n{rendered}"
    );
    assert!(
        rendered.contains("Ti64") && rendered.contains("4.43"),
        "the table's actual data must survive rendering; got:\n{rendered}"
    );
}

/// A figure from ANY tool — not just the notebook — reaches the transcript.
///
/// The engine has always sent these: `ui.card`'s `data.images` carries
/// `{path, shown}` per figure. The TUI read `data` only to choose an evidence
/// colour and discarded the rest, so every plot from `visualization`, the ML
/// parity plots, the correlation heatmaps and the dataset figures was
/// invisible — not for want of information, but because nobody read it.
///
/// Driven with a path that does not exist, since pixels cannot be asserted on
/// a `TestBackend` but the honest failure can, and reaching it proves the draw
/// path ran at all.
#[test]
fn a_figure_from_any_tool_reaches_the_transcript() {
    let mut app = app_with_welcome();
    app.apply_agent_msg(AgentMsg::ToolCard {
        tool_name: "plot".into(),
        content: "wrote the parity plot".into(),
        card_type: "results".into(),
        elapsed_ms: Some(30),
        call_id: None,
        provenance_id: None,
        data: Some(serde_json::json!({
            "images": [{ "path": "/nonexistent/parity.png", "shown": false }]
        })),
    });
    freeze_metrics(&mut app);

    let rendered = render_app_to_string(&app, 100, 40);
    assert!(
        rendered.contains("figure missing on disk"),
        "a tool's figure must be drawn, and an undrawable one must say so \
         rather than leaving blank rows; got:\n{rendered}"
    );
    assert!(
        rendered.contains("parity.png"),
        "the failure must name the file so it can be chased; got:\n{rendered}"
    );
}

/// Jump-to-bottom and jump-to-top must release the user-turn anchor.
///
/// The anchor outranks `auto_scroll` in `draw_chat`, so `G` setting
/// `auto_scroll = true` while the anchor was still armed set a flag the
/// renderer then ignored — pressing `G` did nothing at all. Caught by driving
/// the real binary; the original anchor tests only exercised `j`/`k`, so the
/// whole Home/End pair slipped through.
#[test]
fn jump_to_top_and_bottom_release_the_user_turn_anchor() {
    for key in [
        crossterm::event::KeyCode::Char('G'),
        crossterm::event::KeyCode::End,
        crossterm::event::KeyCode::Char('g'),
        crossterm::event::KeyCode::Home,
    ] {
        let mut app = app_with_welcome();
        app.push_user("anchor me");
        assert!(app.anchor_user_turn.get(), "submitting must arm the anchor");
        app.focus = prism_tui::app::Focus::Chat;
        app.handle_key(crossterm::event::KeyEvent::new(
            key,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(
            !app.anchor_user_turn.get(),
            "{key:?} is explicit navigation and must release the anchor, or it \
             sets a flag the renderer ignores"
        );
    }
}

// ── Honest empty states ───────────────────────────────────────────

/// Collect the Workspace sidebar text out of a rendered frame.
///
/// The sidebar is the column right of the last `│` on each row. Joining the
/// rows with a space undoes the pane's word wrap, so an assertion can name a
/// phrase without having to know where it breaks at this width.
fn sidebar_text(rendered: &str) -> String {
    rendered
        .lines()
        .filter_map(|line| line.rsplit('│').next())
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Render the Workspace sidebar for one tab of an already-prepared `App`.
fn sidebar_for(app: &mut App, tab: prism_tui::app::WorkspaceTab) -> String {
    app.focus = Focus::Workspace;
    app.workspace_tab = tab;
    let rendered = render_app_to_string(app, 100, 30);
    assert_no_terminal_controls(&rendered);
    sidebar_text(&rendered)
}

/// Objects, Structures and Artifacts all go blank, for three DIFFERENT
/// reasons: no `ui.object.update` has arrived and nothing backfills the tab
/// from history, the shared structure cache holds nothing, and the artifact
/// list is session-scoped so rows stored under another session are not
/// listed. Rendering all three as "nothing here yet" hands the reader one
/// screen for three problems with three different owners, so each empty pane
/// must name its own cause and no two may read alike.
#[test]
fn empty_workspace_panes_name_their_own_cause() {
    use prism_tui::app::WorkspaceTab;

    // 1. Objects: the notification feed has produced nothing.
    let mut objects_app = app_with_welcome();
    let objects = sidebar_for(&mut objects_app, WorkspaceTab::Objects);
    assert!(
        objects.contains("No object updates received"),
        "the Objects pane must report that the feed sent nothing, not promise \
         that working will fill it: {objects}"
    );

    // 2. Structures refused by the backend: a missing handler, not a broken
    //    cache. This is what the owner sees today.
    let mut refused_app = app_with_welcome();
    refused_app.session_id = Some("session-structures".into());
    refused_app.apply_agent_msg(AgentMsg::StructureStoreUnavailable {
        message: "Method not found: workspace.structures.list".into(),
    });
    let refused = sidebar_for(&mut refused_app, WorkspaceTab::Structures);
    assert!(
        refused.contains("Structures not connected"),
        "a refused method must read as not connected, not as a cache fault: {refused}"
    );

    // 3. Structures cache fault: a real failure of a connected feature must
    //    stay distinct from case 2.
    let mut broken_app = app_with_welcome();
    broken_app.session_id = Some("session-structures".into());
    broken_app.apply_agent_msg(AgentMsg::StructureStoreUnavailable {
        message: "structure cache directory could not be opened: permission denied".into(),
    });
    let broken = sidebar_for(&mut broken_app, WorkspaceTab::Structures);
    assert!(
        broken.contains("Structure cache unavailable") && !broken.contains("not connected"),
        "a store fault must not be reported as a wiring gap: {broken}"
    );

    // 4. Structures answered, with nothing in it.
    let mut empty_structures_app = app_with_welcome();
    empty_structures_app.session_id = Some("session-structures".into());
    empty_structures_app.apply_agent_msg(AgentMsg::StructuresListed {
        session_id: "session-structures".into(),
        structures: vec![],
    });
    let empty_structures = sidebar_for(&mut empty_structures_app, WorkspaceTab::Structures);
    assert!(
        empty_structures.contains("No structures in the cache"),
        "an answered-but-empty list must name what it is empty over — the \
         shared cache, which is where the rows come from; structure rows carry \
         no session, so naming one claims a scope the data does not have: \
         {empty_structures}"
    );

    // 5. Artifacts answered, with nothing in it. ~/.prism/artifacts.db holds
    //    rows under session `default`; a new session lists none of them, so
    //    the pane must say the list is session-scoped rather than imply the
    //    store is empty.
    let mut empty_artifacts_app = app_with_welcome();
    empty_artifacts_app.artifact_store = ArtifactStoreState::Ready(Vec::new());
    let empty_artifacts = sidebar_for(&mut empty_artifacts_app, WorkspaceTab::Artifacts);
    assert!(
        empty_artifacts.contains("No artifacts in this session"),
        "an empty session list must name the scope it is empty over: {empty_artifacts}"
    );

    // No two of the five may collapse into the same screen.
    let panes = [
        ("objects", &objects),
        ("structures refused", &refused),
        ("structures broken", &broken),
        ("structures empty", &empty_structures),
        ("artifacts empty", &empty_artifacts),
    ];
    for (i, (left_name, left)) in panes.iter().enumerate() {
        for (right_name, right) in panes.iter().skip(i + 1) {
            assert_ne!(
                left, right,
                "`{left_name}` and `{right_name}` render the same pane for different causes"
            );
        }
    }
}

/// The same rule on the Artifacts tab: a backend that does not implement
/// `workspace.artifacts.list` must not be reported as a broken store. The
/// agent answers any unknown method with -32601 (`protocol.rs`), so this is
/// one regression away at all times.
#[test]
fn artifacts_refused_by_the_backend_reads_as_not_connected() {
    use prism_tui::app::WorkspaceTab;

    let mut refused = app_with_welcome();
    refused.apply_agent_msg(AgentMsg::ArtifactStoreUnavailable {
        message: "Method not found: workspace.artifacts.list".into(),
    });
    let refused = sidebar_for(&mut refused, WorkspaceTab::Artifacts);
    assert!(
        refused.contains("Artifacts not connected"),
        "a refused method must read as not connected: {refused}"
    );

    let mut broken = app_with_welcome();
    broken.apply_agent_msg(AgentMsg::ArtifactStoreUnavailable {
        message: "database could not be opened: permission denied".into(),
    });
    let broken = sidebar_for(&mut broken, WorkspaceTab::Artifacts);
    assert!(
        broken.contains("Artifact store unavailable") && !broken.contains("not connected"),
        "a store fault must not be reported as a wiring gap: {broken}"
    );
}

/// Snapshot: the Structures tab as the owner sees it today. The agent has no
/// `workspace.structures.list` arm, so the request comes back -32601. That is
/// a wiring gap, and the pane must not read like a cache that failed to open.
#[test]
fn snapshot_workspace_structures_not_connected_100x30() {
    use prism_tui::app::WorkspaceTab;

    let mut app = app_with_welcome();
    app.session_id = Some("session-structures".into());
    app.apply_agent_msg(AgentMsg::StructureStoreUnavailable {
        message: "Method not found: workspace.structures.list".into(),
    });
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Structures;

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("workspace_structures_not_connected_100x30", rendered);
}

/// Snapshot: the same rule on the Artifacts tab.
#[test]
fn snapshot_workspace_artifacts_not_connected_100x30() {
    use prism_tui::app::WorkspaceTab;

    let mut app = app_with_welcome();
    app.apply_agent_msg(AgentMsg::ArtifactStoreUnavailable {
        message: "Method not found: workspace.artifacts.list".into(),
    });
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Artifacts;

    let rendered = render_app_to_string(&app, 100, 30);
    assert_no_terminal_controls(&rendered);
    insta::assert_snapshot!("workspace_artifacts_not_connected_100x30", rendered);
}

/// Taking over from the anchor continues from the row on screen.
///
/// `draw_chat` places the view at the anchor, but `scroll_offset` still held
/// whatever the reader last typed, and leaving auto-follow used to seed from
/// `view_max_scroll`. So the first `k` after any turn abandoned the anchored
/// position and jumped to the bottom of the transcript — the reader pressed
/// "up" and the view went down. Only a real render catches this: the two
/// numbers are only ever compared through what was drawn.
#[test]
fn the_first_scroll_after_a_turn_resumes_from_what_is_on_screen() {
    let mut app = app_with_welcome();
    // A transcript long enough that the anchor and the bottom differ a lot.
    for i in 0..200 {
        app.push_user(&format!("question {i}"));
        app.apply_agent_msg(AgentMsg::TextDelta(format!("answer {i}\n")));
        app.apply_agent_msg(AgentMsg::TextFlush);
    }
    app.push_user("the turn I want to keep in view");
    // A reply longer than the viewport — the case the anchor exists for, and
    // the only one where the anchored row and the bottom differ.
    for i in 0..60 {
        app.apply_agent_msg(AgentMsg::TextDelta(format!("reply line {i}\n")));
    }
    app.apply_agent_msg(AgentMsg::TextFlush);
    let rendered = render_app_to_string(&app, 100, 30);
    assert!(
        rendered.contains("the turn I want to keep in view"),
        "the anchored turn must be on screen before the handoff is meaningful; \
         got:\n{rendered}"
    );
    let drawn = app.view_scroll.get();
    let bottom = app.view_max_scroll.get();
    assert!(
        drawn < bottom,
        "the anchor must place the view above the bottom for this to test \
         anything (drawn {drawn}, bottom {bottom})"
    );

    app.focus = prism_tui::app::Focus::Chat;
    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('k'),
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(
        app.scroll_offset,
        drawn.saturating_sub(1),
        "one press of `k` must move ONE row up from what was drawn ({drawn}), \
         not jump to the bottom ({bottom})"
    );
}

/// Wrapping is additive, which is what lets `draw_chat` measure figure and
/// anchor positions in one accumulating pass instead of re-wrapping the whole
/// transcript once per mark.
///
/// If a ratatui upgrade ever makes a chunk measure differently from its place
/// in the whole, every inline figure lands on the wrong row. This asserts the
/// property directly rather than leaving it as an assumption in a comment.
#[test]
fn wrapping_is_additive() {
    use ratatui::text::Line;
    use ratatui::widgets::{Paragraph, Wrap};

    let lines: Vec<Line<'static>> = (0..120)
        .map(|i| match i % 4 {
            0 => Line::raw(""),
            1 => Line::raw("short"),
            2 => Line::raw(
                "a considerably longer line that is certain to wrap at every \
                 width this test uses, several times over at the narrow end",
            ),
            _ => Line::raw("    indented body text that also wraps at narrow widths"),
        })
        .collect();

    let count = |chunk: &[Line<'static>], width: u16| {
        Paragraph::new(chunk.to_vec())
            .wrap(Wrap { trim: false })
            .line_count(width)
    };

    for width in [10u16, 20, 37, 80, 100] {
        let whole = count(&lines, width);
        for split in [1usize, 7, 60, 119] {
            let head = count(&lines[..split], width);
            let tail = count(&lines[split..], width);
            assert_eq!(
                head + tail,
                whole,
                "wrapping must be additive at width {width}, split {split}: \
                 {head} + {tail} != {whole}"
            );
        }
    }
}

/// Drawing never asks the terminal what it can draw.
///
/// The graphics query writes an escape sequence and waits up to two seconds
/// for the reply on stdin. Run from inside the render closure — where it first
/// was — it stalls every draw and races the crossterm event reader for the
/// answer: the kitty reply starts `\x1b_G`, which crossterm turns into Alt+`_`
/// and then plain `G`, `i`, `=` and digits, and those land in the prompt and
/// the transcript key map. Detection belongs in `run()`, before the terminal
/// is set up. The query count is the only observable difference, because a
/// failed query and the halfblocks floor produce the same working picker.
#[test]
fn rendering_never_queries_the_terminal_for_graphics() {
    use prism_tui::image_view::ImageView;

    let before = ImageView::detections();
    let mut app = app_with_welcome();
    app.apply_agent_msg(AgentMsg::TextDelta(
        "[plot saved: /nonexistent/figure.png]\n".into(),
    ));
    app.apply_agent_msg(AgentMsg::TextFlush);
    let _ = render_app_to_string(&app, 100, 30);
    // Touch the accessor directly too — the path a future renderer would take.
    let _ = app.image_view().protocol();
    assert_eq!(
        ImageView::detections(),
        before,
        "drawing must not query the terminal: detection belongs in run(), \
         before raw mode and the event reader exist"
    );
}

/// The renderer fills the hit map, so a click means something.
///
/// The map, the lookup and the mouse handlers all existed and were all tested —
/// but nothing ever put a region IN, so `at()` always answered None and every
/// click and every pointer move was discarded. The two tests that covered it
/// seeded the map themselves, so they passed against a dead feature. This one
/// draws a real frame and then asks what is under a cell.
#[test]
fn a_real_frame_records_what_it_drew() {
    use prism_tui::app::WorkspaceTab;
    use prism_tui::hit_map::HitTarget;

    let mut app = app_with_welcome();
    app.push_user("what is the solidus of Ti-6Al-4V");
    app.apply_agent_msg(AgentMsg::TextDelta("about 1878 K\n".into()));
    app.apply_agent_msg(AgentMsg::TextFlush);
    app.apply_agent_msg(AgentMsg::ToolStart {
        tool_name: "materials_search".into(),
        verb: "Running".into(),
        call_id: Some("call-1".into()),
        preview: None,
        approval_required: Some(false),
    });
    app.workspace_tab = WorkspaceTab::Tools;
    let _ = render_app_to_string(&app, 100, 30);

    let map = app.hit_map.borrow();
    assert!(
        !map.is_empty(),
        "a drawn frame recorded nothing — the map is filled by the renderer, \
         and a map that stays empty makes every click a no-op"
    );

    // A tab label must answer for its own cells.
    let tabs: Vec<&HitTarget> = (0..100u16)
        .filter_map(|col| {
            map.at(col, 0)
                .into_iter()
                .chain((0..30u16).filter_map(|row| map.at(col, row)))
                .next()
        })
        .collect();
    assert!(
        tabs.iter().any(|t| matches!(t, HitTarget::WorkspaceTab(_))),
        "no tab label claimed any cell, so clicking the strip cannot switch tabs"
    );
    assert!(
        tabs.iter()
            .any(|t| matches!(t, HitTarget::WorkspaceRow { .. })),
        "no workspace row claimed any cell, so a listed tool cannot be clicked"
    );
    assert!(
        tabs.iter()
            .any(|t| matches!(t, HitTarget::TranscriptMessage { .. })),
        "no transcript message claimed any cell, so pointing at a reply cannot \
         say which reply it is"
    );
}

/// Clicking a tab label switches to THAT tab, not to whatever is nearby.
///
/// The strip shortens labels and elides whole tabs as the sidebar narrows, so
/// the columns are only knowable while the strip is built. A drawn frame is the
/// only honest way to check they line up.
#[test]
fn clicking_a_tab_label_switches_to_that_tab() {
    use prism_tui::app::WorkspaceTab;
    use prism_tui::hit_map::HitTarget;

    let mut app = app_with_welcome();
    app.workspace_tab = WorkspaceTab::Activity;
    let _ = render_app_to_string(&app, 100, 30);

    // Find the cell the Structures label occupies, then click it.
    let target = {
        let map = app.hit_map.borrow();
        let mut found = None;
        'outer: for row in 0..30u16 {
            for col in 0..100u16 {
                if let Some(HitTarget::WorkspaceTab(WorkspaceTab::Structures)) = map.at(col, row) {
                    found = Some((col, row));
                    break 'outer;
                }
            }
        }
        found.expect("the Structures label must claim cells in a 100-column layout")
    };

    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: target.0,
        row: target.1,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
    assert_eq!(
        app.workspace_tab,
        WorkspaceTab::Structures,
        "clicking the Structures label must open Structures"
    );
}
