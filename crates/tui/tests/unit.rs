//! Tests for the TUI crate.
//!
//! These are pure unit tests for the TEA Model + Msg parsing layers:
//! - `parse_notification` turns JSON-RPC notifications into `AgentMsg`.
//! - `App::apply_agent_msg` mutates app state in response.
//! - `App::handle_key` mutates state in response to key events.
//!
//! No real `prism backend` is spawned.  We construct an `App` with a
//! dummy `BackendHandle` backed by `cat` (echoes stdin to stdout,
//! harmless) so `send_message`/`send_approval` don't crash.  The state
//! transitions under test don't depend on backend responses.

#![cfg(test)]

use serde_json::json;

use prism_tui::app::{App, Focus, LineKind, Role};
use prism_tui::backend::BackendHandle;
use prism_tui::msg::{parse_notification, AgentMsg};

/// Build an `App` backed by a `cat` subprocess so the stdin writes in
/// `send_message`/`send_approval` don't crash.  The `cat` process is
/// killed when the `App` (and thus `BackendHandle`) is dropped.
fn test_app() -> App {
    let mut child = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn `cat` for test backend");
    let stdin = child.stdin.take().expect("no stdin on cat");
    let stdout = child.stdout.take().expect("no stdout on cat");
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    // Drain stdout so `cat` doesn't block when its pipe fills up.
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 1024];
        let mut stdout = stdout;
        loop {
            match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        drop(tx);
    });
    let handle = BackendHandle::from_parts(child, stdin, rx, 1);
    App::new(handle)
}

// ── parse_notification ─────────────────────────────────────────────

#[test]
fn parse_welcome() {
    let msg = json!({
        "jsonrpc": "2.0",
        "method": "ui.welcome",
        "params": {"version": "2.7.1", "tool_count": 42}
    });
    let parsed = parse_notification(&msg);
    match parsed {
        AgentMsg::Welcome { version, tool_count } => {
            assert_eq!(version, "2.7.1");
            assert_eq!(tool_count, 42);
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
}

#[test]
fn parse_status() {
    let msg = json!({
        "method": "ui.status",
        "params": {"model": "gemma-4-12b", "session_mode": "agent", "message_count": 5}
    });
    let parsed = parse_notification(&msg);
    match parsed {
        AgentMsg::Status { model, mode, message_count } => {
            assert_eq!(model, "gemma-4-12b");
            assert_eq!(mode, "agent");
            assert_eq!(message_count, 5);
        }
        other => panic!("expected Status, got {other:?}"),
    }
}

#[test]
fn parse_text_delta() {
    let msg = json!({"method": "ui.text.delta", "params": {"text": "Hello"}});
    assert!(matches!(parse_notification(&msg), AgentMsg::TextDelta(t) if t == "Hello"));
}

#[test]
fn parse_thinking_delta() {
    let msg = json!({"method": "ui.thinking.delta", "params": {"text": "hmm"}});
    assert!(matches!(parse_notification(&msg), AgentMsg::ThinkingDelta(t) if t == "hmm"));
}

#[test]
fn parse_text_flush() {
    let msg = json!({"method": "ui.text.flush"});
    assert!(matches!(parse_notification(&msg), AgentMsg::TextFlush));
}

#[test]
fn parse_tool_start() {
    let msg = json!({
        "method": "ui.tool.start",
        "params": {"tool_name": "alloy_sample", "verb": "Running", "call_id": "c1"}
    });
    match parse_notification(&msg) {
        AgentMsg::ToolStart { tool_name, verb, call_id } => {
            assert_eq!(tool_name, "alloy_sample");
            assert_eq!(verb, "Running");
            assert_eq!(call_id, "c1");
        }
        other => panic!("expected ToolStart, got {other:?}"),
    }
}

#[test]
fn parse_tool_card() {
    let msg = json!({
        "method": "ui.card",
        "params": {
            "tool_name": "gfn_evaluate",
            "content": "Fe: 0.3, Ni: 0.3",
            "card_type": "results",
            "elapsed_ms": 292,
        }
    });
    match parse_notification(&msg) {
        AgentMsg::ToolCard { tool_name, content, card_type, elapsed_ms } => {
            assert_eq!(tool_name, "gfn_evaluate");
            assert_eq!(content, "Fe: 0.3, Ni: 0.3");
            assert_eq!(card_type, "results");
            assert_eq!(elapsed_ms, 292);
        }
        other => panic!("expected ToolCard, got {other:?}"),
    }
}

#[test]
fn parse_approval_prompt() {
    let msg = json!({
        "method": "ui.prompt",
        "params": {"tool_name": "bash", "message": "Run `ls`?"}
    });
    match parse_notification(&msg) {
        AgentMsg::ApprovalPrompt { tool_name, message } => {
            assert_eq!(tool_name, "bash");
            assert_eq!(message, "Run `ls`?");
        }
        other => panic!("expected ApprovalPrompt, got {other:?}"),
    }
}

#[test]
fn parse_cost() {
    let msg = json!({
        "method": "ui.cost",
        "params": {"turn_cost": 0.002, "session_cost": 0.05}
    });
    match parse_notification(&msg) {
        AgentMsg::Cost { turn_cost, session_cost } => {
            assert!((turn_cost - 0.002).abs() < 1e-9);
            assert!((session_cost - 0.05).abs() < 1e-9);
        }
        other => panic!("expected Cost, got {other:?}"),
    }
}

#[test]
fn parse_turn_complete() {
    let msg = json!({"method": "ui.turn.complete"});
    assert!(matches!(parse_notification(&msg), AgentMsg::TurnComplete));
}

#[test]
fn parse_view() {
    let msg = json!({
        "method": "ui.view",
        "params": {
            "title": "Search Results",
            "tabs": [
                {"title": "MP", "body": "5 hits"},
                {"title": "OPTIMADE", "body": "12 hits"},
                {"title": "empty", "body": ""},
            ]
        }
    });
    match parse_notification(&msg) {
        AgentMsg::View { title, tabs } => {
            assert_eq!(title, "Search Results");
            assert_eq!(tabs.len(), 2); // empty body tab is filtered
            assert_eq!(tabs[0].0, "MP");
            assert_eq!(tabs[0].1, "5 hits");
        }
        other => panic!("expected View, got {other:?}"),
    }
}

#[test]
fn parse_error() {
    let msg = json!({"error": "something broke"});
    match parse_notification(&msg) {
        AgentMsg::Error(s) => assert!(s.contains("something broke")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn parse_unknown_method() {
    let msg = json!({"method": "ui.mystery", "params": {}});
    assert!(matches!(parse_notification(&msg), AgentMsg::Unknown(_)));
}

#[test]
fn parse_missing_fields_default_gracefully() {
    let msg = json!({"method": "ui.welcome", "params": {}});
    match parse_notification(&msg) {
        AgentMsg::Welcome { version, tool_count } => {
            assert_eq!(version, "?");
            assert_eq!(tool_count, 0);
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
}

// ── apply_agent_msg state transitions ──────────────────────────────

#[test]
fn welcome_sets_version_and_tool_count() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::Welcome {
        version: "2.7.1".into(),
        tool_count: 99,
    });
    assert_eq!(app.prism_version, "2.7.1");
    assert_eq!(app.tool_count, 99);
    // Should push a system message
    assert!(!app.messages.is_empty());
    assert!(app.messages.last().unwrap().text.contains("99 tools"));
}

#[test]
fn status_updates_model_and_mode() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::Status {
        model: "gemma-4-12b".into(),
        mode: "agent".into(),
        message_count: 7,
    });
    assert_eq!(app.model, "gemma-4-12b");
    assert_eq!(app.session_mode, "agent");
    assert_eq!(app.message_count, 7);
}

#[test]
fn text_delta_appends_to_assistant_message() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::TextDelta("Hello".into()));
    app.apply_agent_msg(AgentMsg::TextDelta(" world".into()));
    assert_eq!(app.messages.len(), 1);
    let last = app.messages.last().unwrap();
    assert!(matches!(last.role, Role::Assistant));
    assert_eq!(last.text, "Hello world");
    assert!(matches!(last.kind, LineKind::Text));
}

#[test]
fn text_delta_starts_new_message_after_non_text() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::TextDelta("first".into()));
    app.apply_agent_msg(AgentMsg::ToolStart {
        tool_name: "t".into(),
        verb: "Running".into(),
        call_id: "c".into(),
    });
    app.apply_agent_msg(AgentMsg::TextDelta("second".into()));
    // Should have 3 messages: text, tool-start, text
    assert_eq!(app.messages.len(), 3);
    assert_eq!(app.messages[2].text, "second");
}

#[test]
fn thinking_delta_appends_separately() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ThinkingDelta("reasoning".into()));
    assert_eq!(app.messages.len(), 1);
    let last = app.messages.last().unwrap();
    assert!(matches!(last.role, Role::Assistant));
    assert!(matches!(last.kind, LineKind::Thinking));
    assert_eq!(last.text, "reasoning");
}

#[test]
fn text_delta_tracks_streaming_metrics() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::TextDelta("a".into()));
    assert_eq!(app.tokens_received, 1);
    assert!(app.first_token_time.is_some());
    app.apply_agent_msg(AgentMsg::TextDelta("b".into()));
    assert_eq!(app.tokens_received, 2);
}

#[test]
fn text_flush_clears_waiting_state() {
    let mut app = test_app();
    app.is_waiting = true;
    app.status_text = "Thinking…".into();
    app.apply_agent_msg(AgentMsg::TextFlush);
    assert!(!app.is_waiting);
    assert_eq!(app.status_text, "Ready");
}

#[test]
fn tool_start_pushes_tool_message() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolStart {
        tool_name: "alloy_sample".into(),
        verb: "Running".into(),
        call_id: "c1".into(),
    });
    let last = app.messages.last().unwrap();
    assert!(matches!(last.role, Role::Tool));
    assert!(last.text.contains("alloy_sample"));
    assert!(matches!(last.kind, LineKind::ToolStart { .. }));
}

#[test]
fn tool_card_success_pushes_result() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        tool_name: "gfn_evaluate".into(),
        content: "density=7.8".into(),
        card_type: "results".into(),
        elapsed_ms: 150,
    });
    let last = app.messages.last().unwrap();
    assert!(matches!(last.kind, LineKind::ToolResult { success: true, .. }));
}

#[test]
fn tool_card_error_pushes_error_line() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        tool_name: "bash".into(),
        content: "exit 1".into(),
        card_type: "error".into(),
        elapsed_ms: 50,
    });
    let last = app.messages.last().unwrap();
    assert!(matches!(last.kind, LineKind::Error(_)));
}

#[test]
fn approval_prompt_sets_pending_and_focus() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ApprovalPrompt {
        tool_name: "bash".into(),
        message: "rm -rf?".into(),
    });
    assert!(app.approval_pending.is_some());
    assert_eq!(app.approval_pending.as_ref().unwrap().0, "bash");
    assert!(matches!(app.focus, Focus::Approval));
}

#[test]
fn cost_updates_totals() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::Cost {
        turn_cost: 0.01,
        session_cost: 0.50,
    });
    assert!((app.turn_cost - 0.01).abs() < 1e-9);
    assert!((app.session_cost - 0.50).abs() < 1e-9);
}

#[test]
fn turn_complete_resets_waiting() {
    let mut app = test_app();
    app.is_waiting = true;
    app.apply_agent_msg(AgentMsg::TurnComplete);
    assert!(!app.is_waiting);
    assert_eq!(app.status_text, "Ready");
}

#[test]
fn view_pushes_tab_messages() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::View {
        title: "Results".into(),
        tabs: vec![("MP".into(), "5 hits".into()), ("OQMD".into(), "3 hits".into())],
    });
    assert_eq!(app.messages.len(), 2);
    assert!(app.messages[0].text.contains("MP"));
    assert!(app.messages[1].text.contains("OQMD"));
}

#[test]
fn error_pushes_error_message() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::Error("bad thing".into()));
    let last = app.messages.last().unwrap();
    assert!(matches!(last.kind, LineKind::Error(_)));
    assert!(last.text.contains("bad thing"));
}

#[test]
fn unknown_msg_is_noop() {
    let mut app = test_app();
    let before = app.messages.len();
    app.apply_agent_msg(AgentMsg::Unknown(json!({})));
    assert_eq!(app.messages.len(), before);
}

// ── handle_key state transitions ───────────────────────────────────

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

#[test]
fn ctrl_c_quits() {
    let mut app = test_app();
    app.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.should_quit);
}

#[test]
fn ctrl_l_clears_chat() {
    let mut app = test_app();
    app.push_user("hello");
    app.push_user("world");
    assert_eq!(app.messages.len(), 2);
    app.handle_key(key(KeyCode::Char('l'), KeyModifiers::CONTROL));
    assert_eq!(app.messages.len(), 1); // the "[chat cleared]" system message
    assert!(app.messages[0].text.contains("chat cleared"));
}

#[test]
fn ctrl_t_toggles_thinking_expansion() {
    let mut app = test_app();
    let initial = app.thinking_expanded;
    app.handle_key(key(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert_eq!(app.thinking_expanded, !initial);
    app.handle_key(key(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert_eq!(app.thinking_expanded, initial);
}

#[test]
fn ctrl_m_toggles_metrics() {
    let mut app = test_app();
    let initial = app.show_metrics;
    app.handle_key(key(KeyCode::Char('m'), KeyModifiers::CONTROL));
    assert_eq!(app.show_metrics, !initial);
}

#[test]
fn ctrl_4_toggles_cost() {
    let mut app = test_app();
    let initial = app.show_cost;
    app.handle_key(key(KeyCode::Char('4'), KeyModifiers::CONTROL));
    assert_eq!(app.show_cost, !initial);
}

#[test]
fn tab_cycles_focus() {
    let mut app = test_app();
    assert!(matches!(app.focus, Focus::Input));
    app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
    assert!(matches!(app.focus, Focus::Chat));
    app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
    assert!(matches!(app.focus, Focus::Input));
}

#[test]
fn enter_submits_message_when_in_input_focus() {
    let mut app = test_app();
    app.input.insert_str("hello world");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.is_waiting);
    assert!(app.is_thinking);
    assert_eq!(app.status_text, "Thinking…");
    // User message should be pushed
    assert!(app.messages.iter().any(|m| m.text == "hello world" && matches!(m.role, Role::User)));
    // Input should be cleared
    assert!(app.input.lines().is_empty() || app.input.lines().join("").is_empty());
}

#[test]
fn enter_does_not_submit_empty_input() {
    let mut app = test_app();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(!app.is_waiting);
}

#[test]
fn slash_command_routes_to_send_command() {
    let mut app = test_app();
    app.input.insert_str("/tools");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    // Should be waiting and have a user message with the command
    assert!(app.is_waiting);
    assert!(app.messages.iter().any(|m| m.text == "/tools"));
}

#[test]
fn esc_blurs_from_input_to_chat() {
    let mut app = test_app();
    assert!(matches!(app.focus, Focus::Input));
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.focus, Focus::Chat));
}

#[test]
fn chat_keys_scroll() {
    let mut app = test_app();
    app.focus = Focus::Chat;
    app.auto_scroll = true;
    app.handle_key(key(KeyCode::Up, KeyModifiers::NONE));
    assert!(!app.auto_scroll);
}

#[test]
fn approval_y_approves() {
    let mut app = test_app();
    app.approval_pending = Some(("bash".into(), "rm?".into()));
    app.focus = Focus::Approval;
    app.handle_key(key(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(app.approval_pending.is_none());
    assert!(matches!(app.focus, Focus::Input));
    assert!(app.messages.iter().any(|m| m.text.contains("approved bash")));
}

#[test]
fn approval_n_denies() {
    let mut app = test_app();
    app.approval_pending = Some(("bash".into(), "rm?".into()));
    app.focus = Focus::Approval;
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(app.approval_pending.is_none());
    assert!(app.messages.iter().any(|m| m.text.contains("denied bash")));
}

#[test]
fn approval_a_allows_all() {
    let mut app = test_app();
    app.approval_pending = Some(("bash".into(), "rm?".into()));
    app.focus = Focus::Approval;
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE));
    assert!(app.approval_pending.is_none());
    assert!(app.messages.iter().any(|m| m.text.contains("allow-all bash")));
}

// ── message helpers ────────────────────────────────────────────────

#[test]
fn push_user_adds_user_message() {
    let mut app = test_app();
    app.push_user("hi");
    assert_eq!(app.messages.len(), 1);
    assert!(matches!(app.messages[0].role, Role::User));
    assert_eq!(app.messages[0].text, "hi");
}

#[test]
fn push_system_adds_system_message() {
    let mut app = test_app();
    app.push_system("booted");
    assert_eq!(app.messages.len(), 1);
    assert!(matches!(app.messages[0].role, Role::System));
}

#[test]
fn push_error_adds_error_line() {
    let mut app = test_app();
    app.push_error("oops");
    assert!(matches!(app.messages[0].kind, LineKind::Error(_)));
}

#[test]
fn trim_messages_enforces_sliding_window() {
    let mut app = test_app();
    app.max_messages = 3;
    app.push_user("a");
    app.push_user("b");
    app.push_user("c");
    app.push_user("d");
    assert_eq!(app.messages.len(), 3);
    // Oldest should be dropped
    assert_eq!(app.messages[0].text, "b");
    assert_eq!(app.messages[2].text, "d");
}

#[test]
fn append_assistant_text_merges_consecutive_deltas() {
    let mut app = test_app();
    app.append_assistant_text("foo");
    app.append_assistant_text("bar");
    assert_eq!(app.messages.len(), 1);
    assert_eq!(app.messages[0].text, "foobar");
}

#[test]
fn append_thinking_text_merges_consecutive_deltas() {
    let mut app = test_app();
    app.append_thinking_text("step1");
    app.append_thinking_text("step2");
    assert_eq!(app.messages.len(), 1);
    assert!(matches!(app.messages[0].kind, LineKind::Thinking));
    assert_eq!(app.messages[0].text, "step1step2");
}

// ── handle_backend_message (integration of parse + apply) ──────────

#[test]
fn handle_backend_message_welcome() {
    let mut app = test_app();
    let msg = json!({
        "method": "ui.welcome",
        "params": {"version": "1.0.0", "tool_count": 5}
    });
    app.handle_backend_message(&msg);
    assert_eq!(app.prism_version, "1.0.0");
    assert_eq!(app.tool_count, 5);
}

#[test]
fn handle_backend_message_text_delta() {
    let mut app = test_app();
    let msg = json!({"method": "ui.text.delta", "params": {"text": "hi"}});
    app.handle_backend_message(&msg);
    assert_eq!(app.messages.len(), 1);
    assert_eq!(app.messages[0].text, "hi");
}

// ── regression: tool_card error vs success boundary ────────────────

#[test]
fn tool_card_empty_card_type_is_success() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        tool_name: "t".into(),
        content: "ok".into(),
        card_type: "".into(), // empty != "error" → success
        elapsed_ms: 10,
    });
    assert!(matches!(app.messages.last().unwrap().kind, LineKind::ToolResult { .. }));
}