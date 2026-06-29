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
use prism_tui::msg::{AgentMsg, parse_notification};

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
        AgentMsg::Welcome {
            version,
            tool_count,
        } => {
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
        AgentMsg::Status {
            model,
            mode,
            message_count,
        } => {
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
        AgentMsg::ToolStart {
            tool_name,
            verb,
            call_id,
            preview,
            approval_required,
            ..
        } => {
            assert_eq!(tool_name, "alloy_sample");
            assert_eq!(verb, "Running");
            assert_eq!(call_id.as_deref(), Some("c1"));
            assert!(preview.is_none());
            assert!(approval_required.is_none());
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
        AgentMsg::ToolCard {
            tool_name,
            content,
            card_type,
            elapsed_ms,
            call_id,
            provenance_id,
            data,
            ..
        } => {
            assert_eq!(tool_name, "gfn_evaluate");
            assert_eq!(content, "Fe: 0.3, Ni: 0.3");
            assert_eq!(card_type, "results");
            assert_eq!(elapsed_ms, Some(292));
            assert!(call_id.is_none());
            assert!(provenance_id.is_none());
            assert!(data.is_none());
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
        AgentMsg::ApprovalPrompt {
            tool_name,
            message,
            call_id,
            choices,
            ..
        } => {
            assert_eq!(tool_name, "bash");
            assert_eq!(message, "Run `ls`?");
            assert!(call_id.is_none());
            assert!(choices.is_empty());
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
        AgentMsg::Cost {
            turn_cost,
            session_cost,
            input_tokens,
            output_tokens,
            cache_tokens,
        } => {
            assert!((turn_cost - 0.002).abs() < 1e-9);
            assert!((session_cost - 0.05).abs() < 1e-9);
            assert!(input_tokens.is_none());
            assert!(output_tokens.is_none());
            assert!(cache_tokens.is_none());
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
        AgentMsg::Welcome {
            version,
            tool_count,
        } => {
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
        call_id: Some("c".into()),
        preview: None,
        approval_required: None,
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
        call_id: Some("c1".into()),
        preview: None,
        approval_required: None,
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
        elapsed_ms: Some(150),
        call_id: None,
        provenance_id: None,
        data: None,
    });
    let last = app.messages.last().unwrap();
    assert!(matches!(
        last.kind,
        LineKind::ToolResult { success: true, .. }
    ));
}

#[test]
fn tool_card_error_pushes_error_line() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        tool_name: "bash".into(),
        content: "exit 1".into(),
        card_type: "error".into(),
        elapsed_ms: Some(50),
        call_id: None,
        provenance_id: None,
        data: None,
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
        call_id: None,
        tool_args: None,
        tool_description: None,
        requires_approval: None,
        permission_mode: None,
        choices: vec![],
        prompt_type: None,
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
        input_tokens: None,
        output_tokens: None,
        cache_tokens: None,
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
        tabs: vec![
            ("MP".into(), "5 hits".into()),
            ("OQMD".into(), "3 hits".into()),
        ],
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
    assert!(
        app.messages
            .iter()
            .any(|m| m.text == "hello world" && matches!(m.role, Role::User))
    );
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
    assert!(
        app.messages
            .iter()
            .any(|m| m.text.contains("approved bash"))
    );
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
    assert!(
        app.messages
            .iter()
            .any(|m| m.text.contains("allow-all bash"))
    );
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
        elapsed_ms: Some(10),
        call_id: None,
        provenance_id: None,
        data: None,
    });
    assert!(matches!(
        app.messages.last().unwrap().kind,
        LineKind::ToolResult { .. }
    ));
}

// ═══════════════════════════════════════════════════════════════════════
// Patch 1 tests: enriched event protocol normalization
// ═══════════════════════════════════════════════════════════════════════

// ── Enriched parser tests (wire fields the backend already sends) ────

#[test]
fn parse_tool_start_captures_preview_and_approval() {
    let msg = json!({
        "method": "ui.tool.start",
        "params": {
            "tool_name": "compute_submit",
            "verb": "Running",
            "call_id": "call-42",
            "preview": "{\"image\":\"vasp:6.5\"}",
            "approval_required": true,
        }
    });
    match parse_notification(&msg) {
        AgentMsg::ToolStart {
            tool_name,
            verb,
            call_id,
            preview,
            approval_required,
        } => {
            assert_eq!(tool_name, "compute_submit");
            assert_eq!(verb, "Running");
            assert_eq!(call_id.as_deref(), Some("call-42"));
            assert_eq!(preview.as_deref(), Some("{\"image\":\"vasp:6.5\"}"));
            assert_eq!(approval_required, Some(true));
        }
        other => panic!("expected ToolStart, got {other:?}"),
    }
}

#[test]
fn parse_tool_start_missing_optional_fields_are_none() {
    let msg = json!({
        "method": "ui.tool.start",
        "params": {"tool_name": "gfn_evaluate"}
    });
    match parse_notification(&msg) {
        AgentMsg::ToolStart {
            call_id,
            preview,
            approval_required,
            ..
        } => {
            assert!(call_id.is_none());
            assert!(preview.is_none());
            assert!(approval_required.is_none());
        }
        other => panic!("expected ToolStart, got {other:?}"),
    }
}

#[test]
fn parse_tool_card_captures_call_id_provenance_data() {
    let msg = json!({
        "method": "ui.card",
        "params": {
            "tool_name": "gfn_evaluate",
            "content": "density=7.8",
            "card_type": "results",
            "elapsed_ms": 292,
            "call_id": "call-7",
            "provenance_id": "prov_abc123",
            "data": {"density": 7.8, "vec": 6.5},
        }
    });
    match parse_notification(&msg) {
        AgentMsg::ToolCard {
            tool_name,
            elapsed_ms,
            call_id,
            provenance_id,
            data,
            ..
        } => {
            assert_eq!(tool_name, "gfn_evaluate");
            assert_eq!(elapsed_ms, Some(292));
            assert_eq!(call_id.as_deref(), Some("call-7"));
            assert_eq!(provenance_id.as_deref(), Some("prov_abc123"));
            assert!(data.is_some());
            assert_eq!(data.unwrap()["density"], 7.8);
        }
        other => panic!("expected ToolCard, got {other:?}"),
    }
}

#[test]
fn parse_tool_card_missing_optional_fields_are_none() {
    let msg = json!({
        "method": "ui.card",
        "params": {"tool_name": "t", "content": "ok"}
    });
    match parse_notification(&msg) {
        AgentMsg::ToolCard {
            elapsed_ms,
            call_id,
            provenance_id,
            data,
            ..
        } => {
            assert!(elapsed_ms.is_none());
            assert!(call_id.is_none());
            assert!(provenance_id.is_none());
            assert!(data.is_none());
        }
        other => panic!("expected ToolCard, got {other:?}"),
    }
}

#[test]
fn parse_approval_prompt_captures_rich_fields() {
    let msg = json!({
        "method": "ui.prompt",
        "params": {
            "tool_name": "compute_submit",
            "message": "Allow compute_submit?",
            "call_id": "call-99",
            "tool_args": {"image": "vasp:6.5", "gpu_type": "A100-80GB"},
            "tool_description": "Dispatch a GPU compute job",
            "requires_approval": true,
            "permission_mode": "full_access",
            "choices": ["y", "n", "a", "b"],
            "prompt_type": "approval",
        }
    });
    match parse_notification(&msg) {
        AgentMsg::ApprovalPrompt {
            tool_name,
            message,
            call_id,
            tool_args,
            tool_description,
            requires_approval,
            permission_mode,
            choices,
            prompt_type,
        } => {
            assert_eq!(tool_name, "compute_submit");
            assert_eq!(message, "Allow compute_submit?");
            assert_eq!(call_id.as_deref(), Some("call-99"));
            assert!(tool_args.is_some());
            assert_eq!(tool_args.unwrap()["gpu_type"], "A100-80GB");
            assert_eq!(
                tool_description.as_deref(),
                Some("Dispatch a GPU compute job")
            );
            assert_eq!(requires_approval, Some(true));
            assert_eq!(permission_mode.as_deref(), Some("full_access"));
            assert_eq!(choices, vec!["y", "n", "a", "b"]);
            assert_eq!(prompt_type.as_deref(), Some("approval"));
        }
        other => panic!("expected ApprovalPrompt, got {other:?}"),
    }
}

#[test]
fn parse_approval_prompt_missing_optional_fields_are_none() {
    let msg = json!({
        "method": "ui.prompt",
        "params": {"tool_name": "bash", "message": "ok?"}
    });
    match parse_notification(&msg) {
        AgentMsg::ApprovalPrompt {
            call_id,
            tool_args,
            tool_description,
            requires_approval,
            permission_mode,
            choices,
            prompt_type,
            ..
        } => {
            assert!(call_id.is_none());
            assert!(tool_args.is_none());
            assert!(tool_description.is_none());
            assert!(requires_approval.is_none());
            assert!(permission_mode.is_none());
            assert!(choices.is_empty());
            assert!(prompt_type.is_none());
        }
        other => panic!("expected ApprovalPrompt, got {other:?}"),
    }
}

#[test]
fn parse_cost_captures_token_counts() {
    let msg = json!({
        "method": "ui.cost",
        "params": {
            "turn_cost": 0.01,
            "session_cost": 0.50,
            "input_tokens": 1200,
            "output_tokens": 800,
            "cache_tokens": 400,
        }
    });
    match parse_notification(&msg) {
        AgentMsg::Cost {
            turn_cost,
            session_cost,
            input_tokens,
            output_tokens,
            cache_tokens,
        } => {
            assert!((turn_cost - 0.01).abs() < 1e-9);
            assert!((session_cost - 0.50).abs() < 1e-9);
            assert_eq!(input_tokens, Some(1200));
            assert_eq!(output_tokens, Some(800));
            assert_eq!(cache_tokens, Some(400));
        }
        other => panic!("expected Cost, got {other:?}"),
    }
}

#[test]
fn parse_cost_missing_tokens_are_none() {
    let msg = json!({
        "method": "ui.cost",
        "params": {"turn_cost": 0.0, "session_cost": 0.0}
    });
    match parse_notification(&msg) {
        AgentMsg::Cost {
            input_tokens,
            output_tokens,
            cache_tokens,
            ..
        } => {
            assert!(input_tokens.is_none());
            assert!(output_tokens.is_none());
            assert!(cache_tokens.is_none());
        }
        other => panic!("expected Cost, got {other:?}"),
    }
}

// ── New variant parser tests ─────────────────────────────────────────

#[test]
fn parse_permissions() {
    let msg = json!({
        "method": "ui.permissions",
        "params": {
            "mode": "agent",
            "auto_approved": false,
            "blocked": [],
            "approval_required": true,
            "read_only": false,
            "workspace_write": true,
            "full_access": false,
        }
    });
    match parse_notification(&msg) {
        AgentMsg::Permissions {
            mode,
            auto_approved,
            raw,
        } => {
            assert_eq!(mode.as_deref(), Some("agent"));
            assert_eq!(auto_approved, Some(false));
            // raw retains the full params object
            assert!(raw.get("workspace_write").is_some());
        }
        other => panic!("expected Permissions, got {other:?}"),
    }
}

#[test]
fn parse_permissions_missing_fields_no_panic() {
    let msg = json!({"method": "ui.permissions", "params": {}});
    match parse_notification(&msg) {
        AgentMsg::Permissions {
            mode,
            auto_approved,
            raw,
        } => {
            assert!(mode.is_none());
            assert!(auto_approved.is_none());
            assert!(raw.is_object());
        }
        other => panic!("expected Permissions, got {other:?}"),
    }
}

#[test]
fn parse_session_list() {
    let msg = json!({
        "method": "ui.session.list",
        "params": {
            "sessions": [
                {"id": "s1", "title": "Ti alloy search"},
                {"id": "s2", "title": "HEA discovery"}
            ]
        }
    });
    match parse_notification(&msg) {
        AgentMsg::SessionList { sessions, raw } => {
            assert_eq!(sessions.len(), 2);
            assert_eq!(sessions[0]["id"], "s1");
            assert!(raw.get("sessions").is_some());
        }
        other => panic!("expected SessionList, got {other:?}"),
    }
}

#[test]
fn parse_session_list_empty_no_panic() {
    let msg = json!({"method": "ui.session.list", "params": {}});
    match parse_notification(&msg) {
        AgentMsg::SessionList { sessions, .. } => {
            assert!(sessions.is_empty());
        }
        other => panic!("expected SessionList, got {other:?}"),
    }
}

#[test]
fn parse_backend_warning() {
    let msg = json!({
        "method": "ui.backend.warning",
        "params": {
            "code": "rate_limit",
            "message": "Approaching API rate limit",
        }
    });
    match parse_notification(&msg) {
        AgentMsg::BackendWarning { code, message } => {
            assert_eq!(code.as_deref(), Some("rate_limit"));
            assert_eq!(message, "Approaching API rate limit");
        }
        other => panic!("expected BackendWarning, got {other:?}"),
    }
}

#[test]
fn parse_backend_error_notification() {
    let msg = json!({
        "method": "ui.backend.error",
        "params": {
            "code": 500,
            "message": "Internal backend error",
            "recoverable": true,
        }
    });
    match parse_notification(&msg) {
        AgentMsg::BackendError {
            code,
            message,
            recoverable,
        } => {
            assert_eq!(code, Some(500));
            assert_eq!(message, "Internal backend error");
            assert_eq!(recoverable, Some(true));
        }
        other => panic!("expected BackendError, got {other:?}"),
    }
}

#[test]
fn parse_jsonrpc_error_response_structured() {
    // A JSON-RPC error response (no method, has error object with code+message)
    let msg = json!({
        "error": {
            "code": -32600,
            "message": "Invalid Request",
        }
    });
    match parse_notification(&msg) {
        AgentMsg::BackendError {
            code,
            message,
            recoverable,
        } => {
            assert_eq!(code, Some(-32600));
            assert_eq!(message, "Invalid Request");
            assert!(recoverable.is_none());
        }
        other => panic!("expected BackendError, got {other:?}"),
    }
}

#[test]
fn parse_jsonrpc_error_response_bare_string_falls_back() {
    // A JSON-RPC error where error is a bare string (not an object)
    let msg = json!({"error": "something broke"});
    match parse_notification(&msg) {
        AgentMsg::Error(s) => {
            assert!(s.contains("something broke"));
        }
        other => panic!("expected Error fallback, got {other:?}"),
    }
}

// ── Tolerance tests ──────────────────────────────────────────────────

#[test]
fn parse_extra_unknown_fields_ignored_safely() {
    // The parser must not crash or reject payloads with extra fields
    // it doesn't know about — forward compatibility.
    let msg = json!({
        "method": "ui.welcome",
        "params": {
            "version": "2.0.0",
            "tool_count": 42,
            "future_field": "hello",
            "another_unknown": 123,
        }
    });
    match parse_notification(&msg) {
        AgentMsg::Welcome {
            version,
            tool_count,
        } => {
            assert_eq!(version, "2.0.0");
            assert_eq!(tool_count, 42);
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
}

#[test]
fn parse_missing_method_field_returns_unknown() {
    let msg = json!({"params": {"foo": "bar"}});
    assert!(matches!(parse_notification(&msg), AgentMsg::Unknown(_)));
}

#[test]
fn parse_null_params_no_panic() {
    let msg = json!({"method": "ui.welcome", "params": null});
    match parse_notification(&msg) {
        AgentMsg::Welcome {
            version,
            tool_count,
        } => {
            assert_eq!(version, "?");
            assert_eq!(tool_count, 0);
        }
        other => panic!("expected Welcome with defaults, got {other:?}"),
    }
}

#[test]
fn parse_garbage_method_returns_unknown() {
    let msg = json!({"method": "ui.this.does.not.exist", "params": {}});
    assert!(matches!(parse_notification(&msg), AgentMsg::Unknown(_)));
}

// ── App behavior regression tests for new variants ───────────────────

#[test]
fn permissions_updates_mode_when_present() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::Permissions {
        mode: Some("agent".into()),
        auto_approved: Some(false),
        raw: json!({}),
    });
    assert_eq!(app.session_mode, "agent");
}

#[test]
fn permissions_auto_approved_pushes_system_line() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::Permissions {
        mode: None,
        auto_approved: Some(true),
        raw: json!({}),
    });
    assert!(app.messages.iter().any(|m| m.text.contains("auto-approve")));
}

#[test]
fn permissions_not_auto_approved_no_system_line() {
    let mut app = test_app();
    let before = app.messages.len();
    app.apply_agent_msg(AgentMsg::Permissions {
        mode: None,
        auto_approved: Some(false),
        raw: json!({}),
    });
    assert_eq!(app.messages.len(), before);
}

#[test]
fn session_list_empty_pushes_no_sessions_line() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::SessionList {
        sessions: vec![],
        raw: json!({}),
    });
    assert!(
        app.messages
            .iter()
            .any(|m| m.text.contains("no previous sessions"))
    );
}

#[test]
fn session_list_non_empty_pushes_count_line() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::SessionList {
        sessions: vec![
            json!({"id": "s1"}),
            json!({"id": "s2"}),
            json!({"id": "s3"}),
        ],
        raw: json!({}),
    });
    assert!(
        app.messages
            .iter()
            .any(|m| m.text.contains("3 previous session"))
    );
}

#[test]
fn backend_warning_pushes_system_line() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::BackendWarning {
        code: Some("rate_limit".into()),
        message: "Slow down".into(),
    });
    assert!(
        app.messages
            .iter()
            .any(|m| m.text.contains("rate_limit") && m.text.contains("Slow down"))
    );
}

#[test]
fn backend_error_pushes_error_line() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::BackendError {
        code: Some(500),
        message: "Internal error".into(),
        recoverable: Some(true),
    });
    assert!(
        app.messages
            .iter()
            .any(|m| m.text.contains("error") && m.text.contains("Internal error"))
    );
}

#[test]
fn backend_error_fatal_pushes_error_line() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::BackendError {
        code: Some(-1),
        message: "Backend crashed".into(),
        recoverable: Some(false),
    });
    assert!(app.messages.iter().any(|m| m.text.contains("fatal")));
}

#[test]
fn backend_error_no_code_still_pushes() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::BackendError {
        code: None,
        message: "Unknown failure".into(),
        recoverable: None,
    });
    assert!(
        app.messages
            .iter()
            .any(|m| m.text.contains("Unknown failure"))
    );
}

// ── App behavior regression: existing variants still work ────────────

#[test]
fn tool_start_still_pushes_same_visible_behavior() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolStart {
        tool_name: "alloy_sample".into(),
        verb: "Running".into(),
        call_id: Some("c1".into()),
        preview: Some("{...}".into()),
        approval_required: Some(true),
    });
    let last = app.messages.last().unwrap();
    assert!(matches!(last.role, Role::Tool));
    // The visible text is still "Running alloy_sample" — preview is NOT
    // shown in the current behavior (that's for the tool-card patch).
    assert_eq!(last.text, "Running alloy_sample");
}

#[test]
fn tool_card_result_still_pushes_same_visible_behavior() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        tool_name: "gfn_evaluate".into(),
        content: "density=7.8".into(),
        card_type: "results".into(),
        elapsed_ms: Some(150),
        call_id: Some("c7".into()),
        provenance_id: Some("prov_abc".into()),
        data: Some(json!({"density": 7.8})),
    });
    let last = app.messages.last().unwrap();
    assert!(matches!(
        last.kind,
        LineKind::ToolResult { success: true, .. }
    ));
    // Visible text is still "gfn_evaluate: density=7.8"
    assert_eq!(last.text, "gfn_evaluate: density=7.8");
}

#[test]
fn approval_prompt_still_sets_pending_and_focus() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ApprovalPrompt {
        tool_name: "bash".into(),
        message: "rm?".into(),
        call_id: Some("c1".into()),
        tool_args: Some(json!({"cmd": "rm"})),
        tool_description: Some("Run shell".into()),
        requires_approval: Some(true),
        permission_mode: Some("full_access".into()),
        choices: vec!["y".into(), "n".into(), "a".into()],
        prompt_type: Some("approval".into()),
    });
    assert!(app.approval_pending.is_some());
    assert_eq!(app.approval_pending.as_ref().unwrap().0, "bash");
    assert!(matches!(app.focus, Focus::Approval));
}

#[test]
fn cost_still_updates_existing_fields() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::Cost {
        turn_cost: 0.02,
        session_cost: 0.99,
        input_tokens: Some(500),
        output_tokens: Some(300),
        cache_tokens: Some(100),
    });
    assert!((app.turn_cost - 0.02).abs() < 1e-9);
    assert!((app.session_cost - 0.99).abs() < 1e-9);
}

// ═══════════════════════════════════════════════════════════════════════
// Patch 2 tests: ANSI/control sanitizer ingress
// ═══════════════════════════════════════════════════════════════════════

use prism_tui::sanitize::sanitize_for_render;

/// Assert that stored visible text does not contain terminal control
/// sequences.  Used for every ingress test.
fn assert_no_terminal_controls(text: &str) {
    assert!(!text.contains('\x1b'), "ESC (\\x1b) found in: {text:?}");
    assert!(!text.contains('\x07'), "BEL (\\x07) found in: {text:?}");
    assert!(!text.contains('\x08'), "BS (\\x08) found in: {text:?}");
    assert!(!text.contains('\x0d'), "CR (\\x0d) found in: {text:?}");
    assert!(!text.contains('\x7f'), "DEL (\\x7f) found in: {text:?}");
}

// ── Sanitizer function tests ─────────────────────────────────────────

#[test]
fn sanitize_csi_color_escape() {
    assert_eq!(sanitize_for_render("\x1b[31mred\x1b[0m"), "red");
}

#[test]
fn sanitize_cursor_movement_escape() {
    let input = "\x1b[2J\x1b[Hhello\x1b[1;1H";
    assert_eq!(sanitize_for_render(input), "hello");
}

#[test]
fn sanitize_osc_terminal_title() {
    let input = "\x1b]0;owned\x07hello";
    assert_eq!(sanitize_for_render(input), "hello");
}

#[test]
fn sanitize_osc_with_st_terminator() {
    let input = "\x1b]0;title\x1b\\hello";
    assert_eq!(sanitize_for_render(input), "hello");
}

#[test]
fn sanitize_dcs_payload() {
    let input = "\x1bPqhello\x1b\\world";
    assert_eq!(sanitize_for_render(input), "world");
}

#[test]
fn sanitize_removes_bel() {
    assert_eq!(sanitize_for_render("beep\x07!"), "beep!");
}

#[test]
fn sanitize_removes_backspace() {
    assert_eq!(sanitize_for_render("abc\x08def"), "abcdef");
}

#[test]
fn sanitize_removes_carriage_return() {
    assert_eq!(sanitize_for_render("line1\r\nline2"), "line1\nline2");
}

#[test]
fn sanitize_removes_del() {
    assert_eq!(sanitize_for_render("text\x7fend"), "textend");
}

#[test]
fn sanitize_removes_c1_controls() {
    let input = "a\u{0085}b\u{0099}c";
    assert_eq!(sanitize_for_render(input), "abc");
}

#[test]
fn sanitize_preserves_normal_unicode() {
    let input = "Ti₆Al₄V ΔH_mix 你好 café 🚀";
    assert_eq!(sanitize_for_render(input), input);
}

#[test]
fn sanitize_preserves_newlines() {
    let input = "line1\nline2\nline3";
    assert_eq!(sanitize_for_render(input), input);
}

#[test]
fn sanitize_converts_tabs_to_four_spaces() {
    assert_eq!(sanitize_for_render("a\tb"), "a    b");
}

#[test]
fn sanitize_safe_text_unchanged() {
    let input = "PRISM v2.7.1 — 42 tools available";
    assert_eq!(sanitize_for_render(input), input);
}

#[test]
fn sanitize_empty_string_returns_empty() {
    assert_eq!(sanitize_for_render(""), "");
}

#[test]
fn sanitize_long_safe_text_unchanged() {
    let input = "x".repeat(10_000);
    let result = sanitize_for_render(&input);
    assert_eq!(result.len(), 10_000);
    assert_eq!(result, input);
}

#[test]
fn sanitize_mixed_ansi_and_unicode() {
    let input = "\x1b[32mTi₆Al₄V\x1b[0m 你好 \x1b[1m🚀\x1b[0m";
    assert_eq!(sanitize_for_render(input), "Ti₆Al₄V 你好 🚀");
}

#[test]
fn sanitize_no_escape_left_after_any_input() {
    let inputs = [
        "\x1b[31mred\x1b[0m",
        "\x1b]0;title\x07text",
        "\x1b[2J\x1b[Hclear",
        "beep\x07back\x08del\x7f",
        "cr\r\nline",
        "\u{0085}\u{0099}c1",
        "\x1bPq\x1b\\dcs",
    ];
    for input in inputs {
        let result = sanitize_for_render(input);
        assert_no_terminal_controls(&result);
    }
}

// ── App ingress tests: text is sanitized before storing ─────────────

#[test]
fn text_delta_with_ansi_stores_sanitized_text() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::TextDelta("\x1b[31mred text\x1b[0m".into()));
    let last = app.messages.last().unwrap();
    assert_eq!(last.text, "red text");
    assert_no_terminal_controls(&last.text);
}

#[test]
fn thinking_delta_with_ansi_stores_sanitized_text() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ThinkingDelta(
        "\x1b[33mthinking\x1b[0m about \x1b[2Jstuff".into(),
    ));
    let last = app.messages.last().unwrap();
    assert_eq!(last.text, "thinking about stuff");
    assert_no_terminal_controls(&last.text);
}

#[test]
fn tool_card_content_with_ansi_stores_sanitized_text() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        tool_name: "gfn_evaluate".into(),
        content: "\x1b[32mdensity=7.8\x1b[0m".into(),
        card_type: "results".into(),
        elapsed_ms: Some(150),
        call_id: None,
        provenance_id: None,
        data: None,
    });
    let last = app.messages.last().unwrap();
    assert_eq!(last.text, "gfn_evaluate: density=7.8");
    assert_no_terminal_controls(&last.text);
}

#[test]
fn tool_card_error_with_ansi_stores_sanitized_text() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        tool_name: "\x1b[31mbash\x1b[0m".into(),
        content: "exit \x1b[1m1\x1b[0m".into(),
        card_type: "error".into(),
        elapsed_ms: Some(50),
        call_id: None,
        provenance_id: None,
        data: None,
    });
    let last = app.messages.last().unwrap();
    assert_eq!(last.text, "bash: exit 1");
    assert_no_terminal_controls(&last.text);
}

#[test]
fn tool_start_with_ansi_stores_sanitized_text() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolStart {
        tool_name: "\x1b[36malloy_sample\x1b[0m".into(),
        verb: "\x1b[1mRunning\x1b[0m".into(),
        call_id: None,
        preview: None,
        approval_required: None,
    });
    let last = app.messages.last().unwrap();
    assert_eq!(last.text, "Running alloy_sample");
    assert_no_terminal_controls(&last.text);
}

#[test]
fn backend_error_with_ansi_stores_sanitized_text() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::BackendError {
        code: Some(500),
        message: "\x1b[31mInternal\x1b[0m error\x07".into(),
        recoverable: Some(true),
    });
    let last = app.messages.last().unwrap();
    assert!(last.text.contains("Internal error"));
    assert_no_terminal_controls(&last.text);
}

#[test]
fn approval_prompt_with_ansi_stores_sanitized_text() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ApprovalPrompt {
        tool_name: "\x1b[33mcompute_submit\x1b[0m".into(),
        message: "Allow \x1b[1mcompute_submit\x1b[0m?\x07".into(),
        call_id: None,
        tool_args: None,
        tool_description: None,
        requires_approval: None,
        permission_mode: None,
        choices: vec![],
        prompt_type: None,
    });
    // Check approval_pending is sanitized
    let (tool, msg) = app.approval_pending.as_ref().unwrap();
    assert_eq!(tool, "compute_submit");
    assert_eq!(msg, "Allow compute_submit?");
    assert_no_terminal_controls(tool);
    assert_no_terminal_controls(msg);
    // Check the ChatLine is also sanitized
    let last = app.messages.last().unwrap();
    assert_no_terminal_controls(&last.text);
}

#[test]
fn view_body_with_ansi_stores_sanitized_text() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::View {
        title: "\x1b[1mResults\x1b[0m".into(),
        tabs: vec![("MP".into(), "\x1b[32m5 hits\x1b[0m".into())],
    });
    let last = app.messages.last().unwrap();
    assert!(last.text.contains("Results"));
    assert!(last.text.contains("5 hits"));
    assert_no_terminal_controls(&last.text);
}

#[test]
fn backend_warning_with_ansi_stores_sanitized_text() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::BackendWarning {
        code: Some("\x1b[31mrate_limit\x1b[0m".into()),
        message: "Slow\x07 down\x1b[2J".into(),
    });
    let last = app.messages.last().unwrap();
    assert!(last.text.contains("rate_limit"));
    assert!(last.text.contains("Slow down"));
    assert_no_terminal_controls(&last.text);
}

#[test]
fn user_input_with_control_chars_is_sanitized() {
    let mut app = test_app();
    app.push_user("hello\x1b[31m world\x1b[0m\x07");
    let last = app.messages.last().unwrap();
    assert_eq!(last.text, "hello world");
    assert_no_terminal_controls(&last.text);
}

#[test]
fn push_system_with_control_chars_is_sanitized() {
    let mut app = test_app();
    app.push_system("status\x1b[2J\x1b[H update\x07");
    let last = app.messages.last().unwrap();
    assert_eq!(last.text, "status update");
    assert_no_terminal_controls(&last.text);
}

#[test]
fn push_error_with_control_chars_is_sanitized() {
    let mut app = test_app();
    app.push_error("\x1b[31mfatal\x1b[0m error\x08\x7f");
    let last = app.messages.last().unwrap();
    assert_eq!(last.text, "fatal error");
    assert_no_terminal_controls(&last.text);
}

#[test]
fn append_assistant_text_preserves_unicode_through_sanitizer() {
    let mut app = test_app();
    app.append_assistant_text("Ti₆Al₄V ");
    app.append_assistant_text("ΔH_mix 你好 🚀");
    let last = app.messages.last().unwrap();
    assert_eq!(last.text, "Ti₆Al₄V ΔH_mix 你好 🚀");
    assert_no_terminal_controls(&last.text);
}
