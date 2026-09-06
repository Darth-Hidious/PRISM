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

use prism_tui::app::{App, Focus, LineKind, ObjectStatus, Role, WorkspaceTab};
use prism_tui::artifact::{ArtifactPromotion, ArtifactStoreState, WorkspaceArtifact};
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
    let mut app = App::new(handle);
    // These tests exercise post-launch behavior; dismiss the Mission Control
    // home (the launch overlay) so global keys reach their handlers.
    app.home.open = false;
    app
}

// ── parse_notification ─────────────────────────────────────────────

#[test]
fn parse_welcome() {
    let msg = json!({
        "jsonrpc": "2.0",
        "method": "ui.welcome",
        "params": {
            "version": "2.7.1",
            "tool_count": 42,
            "session_id": "session-1"
        }
    });
    let parsed = parse_notification(&msg);
    match parsed {
        AgentMsg::Welcome {
            version,
            tool_count,
            session_id,
        } => {
            assert_eq!(version, "2.7.1");
            assert_eq!(tool_count, 42);
            assert_eq!(session_id.as_deref(), Some("session-1"));
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
        "params": {"tool_name": "sample_material", "verb": "Running", "call_id": "c1"}
    });
    match parse_notification(&msg) {
        AgentMsg::ToolStart {
            tool_name,
            verb,
            call_id,
            preview,
            approval_required,
            agent,
        } => {
            assert_eq!(tool_name, "sample_material");
            assert_eq!(verb, "Running");
            assert_eq!(call_id.as_deref(), Some("c1"));
            assert!(preview.is_none());
            assert!(approval_required.is_none());
            assert!(agent.is_none(), "the parent's own work carries no name");
        }
        other => panic!("expected ToolStart, got {other:?}"),
    }
}

#[test]
fn parse_tool_card() {
    let msg = json!({
        "method": "ui.card",
        "params": {
            "tool_name": "evaluate_material",
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
            agent,
        } => {
            assert_eq!(tool_name, "evaluate_material");
            assert_eq!(content, "Fe: 0.3, Ni: 0.3");
            assert_eq!(card_type, "results");
            assert_eq!(elapsed_ms, Some(292));
            assert!(call_id.is_none());
            assert!(provenance_id.is_none());
            assert!(data.is_none());
            assert!(agent.is_none(), "the parent's own work carries no name");
        }
        other => panic!("expected ToolCard, got {other:?}"),
    }
}

#[test]
fn parse_tool_start_with_agent_names_the_delegated_lane() {
    let msg = json!({
        "method": "ui.tool.start",
        "params": {
            "tool_name": "dft_relax",
            "verb": "Relaxing — TiO2",
            "call_id": "c1",
            "agent": "Bhabha"
        }
    });
    match parse_notification(&msg) {
        AgentMsg::ToolStart { agent, .. } => {
            assert_eq!(agent.as_deref(), Some("Bhabha"));
        }
        other => panic!("expected ToolStart, got {other:?}"),
    }
}

#[test]
fn parse_tool_card_with_agent_names_the_delegated_lane() {
    let msg = json!({
        "method": "ui.card",
        "params": {
            "tool_name": "band_structure",
            "content": "gap 1.2 eV",
            "card_type": "results",
            "agent": "Sarabhai"
        }
    });
    match parse_notification(&msg) {
        AgentMsg::ToolCard { agent, .. } => {
            assert_eq!(agent.as_deref(), Some("Sarabhai"));
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
            session_id: _,
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
        session_id: None,
    });
    assert_eq!(app.prism_version, "2.7.1");
    assert_eq!(app.tool_count, 99);
    // Should push a system message
    assert!(!app.messages.is_empty());
    // The welcome carries the count into STATE (asserted above), but does not
    // recite it at the user: the tool inventory is implementation detail, and
    // a greeting that opens with "99 tools" invites being asked about all 99.
    // It stays reachable through the tools pane and the usage stats.
    assert!(app.messages.last().unwrap().text.contains("PRISM ready"));
    assert!(
        !app.messages.last().unwrap().text.contains("99 tools"),
        "the greeting must not advertise the tool count"
    );
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
        agent: None,
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
    // Metrics are estimated from accumulated visible-text BYTES (~4 chars per
    // token), not from the count of SSE deltas (which conflated chunk count
    // with token count). 8 bytes ⇒ ~2 tokens.
    app.apply_agent_msg(AgentMsg::TextDelta("abcdefgh".into()));
    assert_eq!(app.tokens_received, 2);
    assert!(app.first_token_time.is_some());
    app.apply_agent_msg(AgentMsg::TextDelta("ijklmnop".into()));
    assert_eq!(app.tokens_received, 4); // 16 bytes / 4
}

#[test]
fn text_flush_clears_waiting_but_does_not_declare_the_turn_done() {
    let mut app = test_app();
    app.is_waiting = true;
    app.status_text = "Thinking…".into();
    app.apply_agent_msg(AgentMsg::TextFlush);
    assert!(!app.is_waiting);
    // A text segment ending is not the turn ending. The flush used to write
    // "Ready" here — exactly when a tool call begins — so the footer read
    // Ready for the whole of a running search (driven live, 2026-09-05).
    assert_ne!(app.status_text, "Ready");
}

#[test]
fn tool_start_pushes_tool_message() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolStart {
        agent: None,
        tool_name: "sample_material".into(),
        verb: "Running".into(),
        call_id: Some("c1".into()),
        preview: None,
        approval_required: None,
    });
    let last = app.messages.last().unwrap();
    assert!(matches!(last.role, Role::Tool));
    assert!(last.text.contains("sample_material"));
    assert!(matches!(last.kind, LineKind::ToolStart { .. }));
}

#[test]
fn tool_card_success_pushes_result_with_text_evidence_token() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        agent: None,
        tool_name: "evaluate_material".into(),
        content: "density=7.8".into(),
        card_type: "results".into(),
        elapsed_ms: Some(150),
        call_id: None,
        provenance_id: None,
        data: Some(json!({"evidence_class": "screening"})),
    });
    let last = app.messages.last().unwrap();
    assert!(matches!(
        last.kind,
        LineKind::ToolResult { success: true, .. }
    ));
    assert!(last.text.contains("[YELLOW screening]"), "{}", last.text);
}

/// Saying nothing and saying something unreadable are different, and only one
/// of them is the tool's fault.
///
/// Both stay visibly not-verified — an unmarked result reads as a verified one
/// — but RED is reserved for a claim, so that it still means something when it
/// appears. Nearly every tool in the tree declares no class at all; painting
/// all of them RED is what hid the results that genuinely are ungrounded.
#[test]
fn a_tool_that_declared_nothing_is_unclassified_not_ungrounded() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        agent: None,
        tool_name: "evaluate_material".into(),
        content: "reward=0.75".into(),
        card_type: "results".into(),
        elapsed_ms: Some(10),
        call_id: None,
        provenance_id: None,
        data: None,
    });
    let last = app.messages.last().unwrap();
    assert!(last.text.contains("[unclassified]"), "{}", last.text);
    assert!(
        !last.text.contains("[GREEN"),
        "an unclassified result must never read as verified: {}",
        last.text
    );
}

/// A class PRISM cannot read is a claim it cannot check, so it stays RED. The
/// tool asserted a grounding; we simply do not know which — that is a worse
/// position than silence, not a better one.
#[test]
fn an_unreadable_evidence_class_stays_indeterminate() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        agent: None,
        tool_name: "evaluate_material".into(),
        content: "reward=0.75".into(),
        card_type: "results".into(),
        elapsed_ms: Some(10),
        call_id: None,
        provenance_id: None,
        data: Some(json!({"evidence_class": "unrecognized"})),
    });
    let last = app.messages.last().unwrap();
    assert!(
        last.text.contains("[RED indeterminate]"),
        "an unreadable claim is not the same as no claim: {}",
        last.text
    );
}

#[test]
fn tool_card_error_pushes_error_line() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        agent: None,
        tool_name: "bash".into(),
        content: "exit 1".into(),
        card_type: "error".into(),
        elapsed_ms: Some(50),
        call_id: None,
        provenance_id: None,
        data: None,
    });
    let last = app.messages.last().unwrap();
    assert!(matches!(last.kind, LineKind::Error(..)));
}

#[test]
fn tool_start_with_agent_stores_the_name_on_the_line() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolStart {
        agent: Some("Sarabhai".into()),
        tool_name: "dft_relax".into(),
        verb: "Relaxing — TiO2".into(),
        call_id: None,
        preview: None,
        approval_required: None,
    });
    let last = app.messages.last().unwrap();
    match &last.kind {
        LineKind::ToolStart { agent, .. } => {
            assert_eq!(agent.as_deref(), Some("Sarabhai"));
        }
        other => panic!("expected ToolStart, got {other:?}"),
    }
}

#[test]
fn tool_card_with_agent_stores_the_name_on_the_result() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        agent: Some("Bhabha".into()),
        tool_name: "band_structure".into(),
        content: "gap 1.2 eV".into(),
        card_type: "results".into(),
        elapsed_ms: Some(10),
        call_id: None,
        provenance_id: None,
        data: None,
    });
    let last = app.messages.last().unwrap();
    match &last.kind {
        LineKind::ToolResult { agent, .. } => {
            assert_eq!(agent.as_deref(), Some("Bhabha"));
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[test]
fn a_parent_tool_card_stays_unnamed() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        agent: None,
        tool_name: "band_structure".into(),
        content: "gap 1.2 eV".into(),
        card_type: "results".into(),
        elapsed_ms: Some(10),
        call_id: None,
        provenance_id: None,
        data: None,
    });
    let last = app.messages.last().unwrap();
    match &last.kind {
        LineKind::ToolResult { agent, .. } => {
            assert!(agent.is_none(), "absence on the wire must stay absence");
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[test]
fn a_failed_tool_card_keeps_its_agent() {
    // A failure is exactly when lane attribution matters most, so the
    // error line carries the name even though it is not a ToolResult.
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        agent: Some("Wagner".into()),
        tool_name: "compute_submit".into(),
        content: "budget exceeded".into(),
        card_type: "error".into(),
        elapsed_ms: Some(5),
        call_id: None,
        provenance_id: None,
        data: None,
    });
    let last = app.messages.last().unwrap();
    assert!(
        matches!(&last.kind, LineKind::Error(_, agent) if agent.as_deref() == Some("Wagner")),
        "expected an Error line attributed to Wagner, got {:?}",
        last.kind
    );
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
        reason: None,
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
fn view_opens_panel_with_tabs() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::View {
        title: "Results".into(),
        tabs: vec![
            ("MP".into(), "5 hits".into()),
            ("OQMD".into(), "3 hits".into()),
        ],
    });
    // Views now render as a tabbed panel, not chat messages.
    assert!(app.view.open);
    assert_eq!(app.view.title, "Results");
    assert_eq!(app.view.tabs.len(), 2);
    assert_eq!(app.view.tabs[0].0, "MP");
}

#[test]
fn error_pushes_error_message() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::Error("bad thing".into()));
    let last = app.messages.last().unwrap();
    assert!(matches!(last.kind, LineKind::Error(..)));
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
    assert!(matches!(app.focus, Focus::Workspace));
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

/// `/sessions` typed at the prompt must still open the picker when its list
/// arrives — the stale-reply guard keys off the pending-fetch flag, so the
/// typed path has to raise that flag when it sends.
#[test]
fn typed_sessions_command_still_opens_the_picker() {
    let mut app = test_app();
    app.input.insert_str("/sessions");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        app.session_picker.loading,
        "typed /sessions marks the fetch pending"
    );
    app.handle_backend_message(&json!({
        "jsonrpc": "2.0",
        "method": "ui.session.list",
        "params": {"sessions": [{"session_id": "sess-1", "turn_count": 2}]},
    }));
    assert!(!app.session_picker.loading, "the reply completes the fetch");
    assert!(
        app.session_picker.open,
        "the reply to a typed /sessions opens the picker"
    );
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
    assert!(matches!(app.messages[0].kind, LineKind::Error(..)));
}

/// The transcript is KEPT, not windowed.
///
/// This test used to assert the opposite: `max_messages = 3` then four pushes
/// left `["b", "c", "d"]`, with "a" dropped. That sliding window shipped as a
/// 500-entry cap, and it deleted the reader's own history with no marker — a
/// long session could not be scrolled back to its start and nothing said why.
/// It also emptied the Workspace Activity feed of the same turns, because that
/// feed is derived from this buffer, so there was no surface left where the
/// dropped turns survived.
///
/// The cap was paying for render scope, not memory: `draw_chat` rebuilds every
/// line each frame regardless. Bounding what is DRAWN is free; bounding what is
/// KEPT destroys history. So the rule is inverted here, deliberately.
#[test]
fn every_message_is_retained_and_none_are_silently_dropped() {
    let mut app = test_app();
    for text in ["a", "b", "c", "d"] {
        app.push_user(text);
    }
    assert_eq!(
        app.messages.len(),
        4,
        "no message may be dropped: a truncated transcript renders identically \
         to a complete one, which is the failure this replaced"
    );
    assert_eq!(
        app.messages[0].text, "a",
        "the OLDEST message is the one the sliding window used to eat"
    );
    assert_eq!(app.messages[3].text, "d");
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
        agent: None,
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
            agent,
        } => {
            assert_eq!(tool_name, "compute_submit");
            assert_eq!(verb, "Running");
            assert_eq!(call_id.as_deref(), Some("call-42"));
            assert_eq!(preview.as_deref(), Some("{\"image\":\"vasp:6.5\"}"));
            assert_eq!(approval_required, Some(true));
            assert!(agent.is_none(), "no agent field on the wire stays None");
        }
        other => panic!("expected ToolStart, got {other:?}"),
    }
}

#[test]
fn parse_tool_start_missing_optional_fields_are_none() {
    let msg = json!({
        "method": "ui.tool.start",
        "params": {"tool_name": "evaluate_material"}
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
            "tool_name": "evaluate_material",
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
            assert_eq!(tool_name, "evaluate_material");
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

// ── Agent attribution: the wire names WHICH delegated agent ─────────
//
// The WITH-agent direction is covered by
// `parse_tool_start_with_agent_names_the_delegated_lane` and
// `parse_tool_card_with_agent_names_the_delegated_lane` above; these pin
// the other half of the contract.

/// No `agent` on the wire means the PARENT did the work. That must parse
/// as `None` — a defaulted name would attribute the parent's work to a
/// lane that never existed.
#[test]
fn parse_tool_card_without_agent_is_none_not_a_guess() {
    let msg = json!({
        "method": "ui.card",
        "params": {"tool_name": "prior_art_search", "content": "3 hits"}
    });
    match parse_notification(&msg) {
        AgentMsg::ToolCard { agent, .. } => assert!(agent.is_none()),
        other => panic!("expected ToolCard, got {other:?}"),
    }
}

/// Same rule on the start notification: the parent's own work stays
/// unnamed — absence on the wire is absence in the model.
#[test]
fn parse_tool_start_without_agent_is_none_not_a_guess() {
    let msg = json!({
        "method": "ui.tool.start",
        "params": {"tool_name": "web_browse", "verb": "Searching the web"}
    });
    match parse_notification(&msg) {
        AgentMsg::ToolStart { agent, .. } => assert!(agent.is_none()),
        other => panic!("expected ToolStart, got {other:?}"),
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
            reason: _,
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
            ..
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
            ..
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
            session_id: _,
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
            session_id: _,
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
fn session_list_empty_does_not_open_picker() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::SessionList {
        sessions: vec![],
        raw: json!({}),
    });
    // Empty list: no picker, no transcript line (a toast is shown instead).
    assert!(!app.session_picker.open);
    assert!(app.session_picker.sessions.is_empty());
}

#[test]
fn session_list_non_empty_opens_picker() {
    let mut app = test_app();
    // The list completes a fetch that was actually started — `open_sessions`
    // or `/sessions` typed at the prompt marks it pending. A list with no
    // pending fetch is a stale reply and must not grab the screen (see
    // `picking_a_session_keeps_the_picker_closed_against_a_stale_list`).
    app.session_picker.loading = true;
    app.apply_agent_msg(AgentMsg::SessionList {
        sessions: vec![
            json!({"session_id": "s1"}),
            json!({"session_id": "s2"}),
            json!({"session_id": "s3"}),
        ],
        raw: json!({}),
    });
    // Non-empty list: the session picker opens with the sessions loaded.
    assert!(app.session_picker.open);
    assert_eq!(app.session_picker.sessions.len(), 3);
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
        rpc_id: None,
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
        rpc_id: None,
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
        rpc_id: None,
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
        agent: None,
        tool_name: "sample_material".into(),
        verb: "Running".into(),
        call_id: Some("c1".into()),
        preview: Some("{...}".into()),
        approval_required: Some(true),
    });
    let last = app.messages.last().unwrap();
    assert!(matches!(last.role, Role::Tool));
    // The row names the tool AND the object of the call: the backend's
    // preview follows a bare verb so the reader sees what is being run
    // before the result lands.
    assert_eq!(last.text, "Running sample_material — {...}");
}

// ── Workspace detail modal (Enter) ───────────────────────────────────

#[test]
fn workspace_enter_on_tools_opens_detail_modal() {
    let mut app = test_app();
    app.tool_catalog = vec![json!({
        "name": "web",
        "description": "Open-web access",
        "approval": false,
    })];
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Tools;
    app.workspace_selected = 0;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.view.open, "Enter on a tool must open the detail modal");
    assert_eq!(app.view.title, "Tool — web");
    let body = &app.view.tabs[0].1;
    assert!(body.contains("auto-approved"), "approval must be shown");
    assert!(
        body.contains("Open-web access"),
        "description must be shown"
    );
    assert!(
        body.contains("tools.d/web.toml"),
        "per-tool config path must be shown"
    );
}

#[test]
fn workspace_space_still_expands_inline() {
    let mut app = test_app();
    app.focus = Focus::Workspace;
    assert!(!app.workspace_expanded);
    app.handle_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
    assert!(app.workspace_expanded, "Space must keep the inline expand");
}

#[test]
fn workspace_enter_on_activity_shows_event_json() {
    let mut app = test_app();
    app.push_user("sample alloy");
    app.apply_agent_msg(AgentMsg::ToolCard {
        agent: None,
        tool_name: "sample_material".into(),
        content: "W0.3 Mo0.2".into(),
        card_type: "results".into(),
        elapsed_ms: Some(292),
        call_id: None,
        provenance_id: None,
        data: None,
    });
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Activity;
    app.workspace_selected = 1; // row 2 = the tool result
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        app.view.open,
        "Enter on an activity row must open the modal"
    );
    assert_eq!(app.view.title, "Activity — 2. tool");
    let body = &app.view.tabs[0].1;
    assert!(body.contains("\"tool_name\": \"sample_material\""));
    assert!(body.contains("\"success\": true"));
    assert!(body.contains("\"elapsed_ms\": 292"));
}

#[test]
fn workspace_enter_on_files_shows_file_content() {
    let path = std::env::temp_dir().join(format!("prism_tui_files_tab_{}.txt", std::process::id()));
    std::fs::write(&path, "file body for the modal").expect("write temp file");

    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        agent: None,
        tool_name: "write_file".into(),
        content: format!("Wrote {}", path.display()),
        card_type: "results".into(),
        elapsed_ms: Some(10),
        call_id: None,
        provenance_id: None,
        data: None,
    });
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Files;
    app.workspace_selected = 0;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    let _ = std::fs::remove_file(&path);

    assert!(app.view.open, "Enter on a file must open the viewer modal");
    assert!(app.view.title.starts_with("File — "));
    assert!(app.view.tabs[0].1.contains("file body for the modal"));
}

#[test]
fn workspace_enter_on_empty_tab_toasts_instead_of_opening() {
    let mut app = test_app();
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Files;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(!app.view.open, "no files → no modal");
    assert!(!app.toasts.is_empty(), "user must get feedback via a toast");
}

#[test]
fn artifact_list_empty_and_store_unavailable_remain_distinct() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::SessionChanged {
        session_id: "session-a".into(),
    });
    app.apply_agent_msg(AgentMsg::ArtifactsListed {
        session_id: "session-a".into(),
        artifacts: Vec::new(),
    });
    assert!(matches!(
        app.artifact_store,
        ArtifactStoreState::Ready(ref rows) if rows.is_empty()
    ));

    app.apply_agent_msg(AgentMsg::ArtifactStoreUnavailable {
        message: "database could not be opened".into(),
    });
    assert!(matches!(
        app.artifact_store,
        ArtifactStoreState::Unavailable(ref reason)
            if reason == "database could not be opened"
    ));
}

#[test]
fn malformed_artifact_list_is_unavailable_not_empty() {
    let parsed = parse_notification(&json!({
        "method": "ui.artifacts.list",
        "params": {"session_id": "session-a"}
    }));
    assert!(matches!(parsed, AgentMsg::ArtifactStoreUnavailable { .. }));
}

#[test]
fn workspace_enter_on_artifact_fetches_into_existing_view_panel() {
    let mut app = test_app();
    app.session_id = Some("session-a".into());
    app.artifact_store = ArtifactStoreState::Ready(vec![WorkspaceArtifact {
        id: "art_123".into(),
        tool: "materials_search".into(),
        summary: "two candidates".into(),
        record_count: Some(2),
        bytes_size: 512,
        created_at: "2026-08-11T12:00:00+00:00".into(),
        age: "now".into(),
        promotion: ArtifactPromotion::Promoted,
        session_id: "session-a".into(),
    }]);
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Artifacts;

    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.view.open, "Enter must reuse the existing View panel");
    assert!(app.view.tabs[0].1.contains("Loading artifact content"));

    app.apply_agent_msg(AgentMsg::ArtifactFetched {
        artifact_id: "art_123".into(),
        session_id: "session-a".into(),
        artifact: json!({
            "artifact_id": "art_123",
            "session_id": "session-a",
            "tool": "materials_search",
            "args": {"elements": ["W", "Mo"]},
            "result": {"candidates": [1, 2]},
            "summary": "two candidates",
            "record_count": 2,
            "bytes_size": 512,
            "created_at": "2026-08-11T12:00:00+00:00",
            "promoted_to_kg": true
        }),
    });
    let body = &app.view.tabs[0].1;
    assert!(body.contains("\"args\""), "{body}");
    assert!(body.contains("\"result\""), "{body}");
}

// ── Link picker (`o`) ────────────────────────────────────────────────

#[test]
fn new_session_clears_the_old_artifact_scope_while_backend_changes_session() {
    let mut app = test_app();
    app.session_id = Some("session-a".into());
    app.artifact_store = ArtifactStoreState::Ready(Vec::new());
    app.workspace_selected = 4;

    app.new_session();

    assert!(app.session_id.is_none());
    assert!(matches!(app.artifact_store, ArtifactStoreState::Loading));
    assert_eq!(app.workspace_selected, 0);
    assert!(app.is_waiting);
    assert_eq!(app.status_text, "Starting new session…");
}

#[test]
fn new_session_does_not_clear_state_during_an_active_turn() {
    let mut app = test_app();
    app.session_id = Some("session-a".into());
    app.artifact_store = ArtifactStoreState::Ready(Vec::new());
    app.input.insert_str("work in progress");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    app.apply_agent_msg(AgentMsg::TextDelta("partial answer".into()));
    assert!(
        !app.is_waiting,
        "streaming clears only the waiting indicator"
    );

    app.new_session();

    assert_eq!(app.session_id.as_deref(), Some("session-a"));
    assert!(matches!(app.artifact_store, ArtifactStoreState::Ready(_)));
    assert!(
        app.messages
            .iter()
            .any(|line| line.text == "work in progress")
    );
    assert!(
        app.toasts
            .iter()
            .any(|toast| toast.message.contains("current turn"))
    );
}

#[test]
fn artifact_selection_stops_at_the_last_loaded_row() {
    let mut app = test_app();
    app.artifact_store = ArtifactStoreState::Ready(vec![WorkspaceArtifact {
        id: "art_only".into(),
        tool: "materials_search".into(),
        summary: "one candidate".into(),
        record_count: Some(1),
        bytes_size: 64,
        created_at: "2026-08-11T12:00:00+00:00".into(),
        age: "now".into(),
        promotion: ArtifactPromotion::NotPromoted,
        session_id: "session-a".into(),
    }]);
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Artifacts;

    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE));

    assert_eq!(app.workspace_selected, 0);
}

#[test]
fn artifact_refresh_clamps_a_selection_past_the_new_end() {
    let mut app = test_app();
    app.session_id = Some("session-a".into());
    app.workspace_selected = 999;

    app.apply_agent_msg(AgentMsg::ArtifactsListed {
        session_id: "session-a".into(),
        artifacts: vec![json!({
            "artifact_id": "art_only",
            "tool": "materials_search",
            "summary": "one candidate",
            "record_count": 1,
            "bytes_size": 64,
            "created_at": "2026-08-11T12:00:00+00:00",
            "promoted_to_kg": false,
            "session_id": "session-a"
        })],
    });

    assert_eq!(app.workspace_selected, 0);
}

#[test]
fn session_change_closes_an_open_artifact_view() {
    let mut app = test_app();
    app.session_id = Some("session-a".into());
    app.artifact_store = ArtifactStoreState::Ready(vec![WorkspaceArtifact {
        id: "art_123".into(),
        tool: "materials_search".into(),
        summary: "two candidates".into(),
        record_count: Some(2),
        bytes_size: 512,
        created_at: "2026-08-11T12:00:00+00:00".into(),
        age: "now".into(),
        promotion: ArtifactPromotion::Promoted,
        session_id: "session-a".into(),
    }]);
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Artifacts;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.view.open);

    app.apply_agent_msg(AgentMsg::SessionChanged {
        session_id: "session-b".into(),
    });

    assert!(
        !app.view.open,
        "old-session artifact content must be closed"
    );
    assert_eq!(app.session_id.as_deref(), Some("session-b"));
    assert!(matches!(app.artifact_store, ArtifactStoreState::Loading));
}

#[test]
fn artifact_list_rejects_a_control_modified_session_id() {
    let mut app = test_app();
    app.session_id = Some("session-a".into());

    app.apply_agent_msg(AgentMsg::ArtifactsListed {
        session_id: "session-a\u{1b}[31m".into(),
        artifacts: Vec::new(),
    });

    assert!(matches!(
        app.artifact_store,
        ArtifactStoreState::Unavailable(ref reason)
            if reason == "artifact list reported an invalid session id"
    ));
}

#[test]
fn invalid_session_change_cancels_a_scheduled_artifact_refresh() {
    let mut app = test_app();
    app.artifact_policy.refresh_debounce = std::time::Duration::ZERO;
    app.apply_agent_msg(AgentMsg::SessionChanged {
        session_id: "session-a".into(),
    });
    app.apply_agent_msg(AgentMsg::SessionChanged {
        session_id: "session-a\u{1b}[31m".into(),
    });

    app.poll_artifact_requests();

    assert!(matches!(
        app.artifact_store,
        ArtifactStoreState::Unavailable(ref reason)
            if reason == "backend reported an invalid artifact session id"
    ));
}

// The link picker lives on the shifted letter: plain `o` opens the first
// reference on the cursor line, the same "open" it means on a structure row.
#[test]
fn shift_o_in_chat_focus_opens_link_picker_newest_first() {
    let mut app = test_app();
    app.push_user("see https://old.example.org");
    app.apply_agent_msg(AgentMsg::TextDelta(
        "source: [paper](https://new.example.org/x)".into(),
    ));
    app.focus = Focus::Chat;
    app.handle_key(key(KeyCode::Char('O'), KeyModifiers::SHIFT));
    assert!(app.link_picker.open, "O must open the link picker");
    assert!(
        !app.link_picker.confirm,
        "multiple links must show the list first"
    );
    assert_eq!(app.link_picker.urls[0], "https://new.example.org/x");
    assert_eq!(app.link_picker.urls[1], "https://old.example.org");
}

#[test]
fn single_link_still_shows_confirm_dialog() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::TextDelta(
        "source: https://example.org/paper".into(),
    ));
    app.focus = Focus::Chat;
    app.handle_key(key(KeyCode::Char('O'), KeyModifiers::SHIFT));
    assert!(app.link_picker.open);
    assert!(
        app.link_picker.confirm,
        "even a single URL must go through the confirm dialog"
    );
    // Esc cancels without opening anything.
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        !app.link_picker.open,
        "Esc in confirm must close the picker"
    );
}

#[test]
fn link_picker_digit_selects_and_asks_confirmation() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::TextDelta(
        "see https://a.example.org and https://b.example.org".into(),
    ));
    app.focus = Focus::Chat;
    app.handle_key(key(KeyCode::Char('O'), KeyModifiers::SHIFT));
    assert!(!app.link_picker.confirm);
    app.handle_key(key(KeyCode::Char('2'), KeyModifiers::NONE));
    assert_eq!(app.link_picker.selected, 1);
    assert!(app.link_picker.confirm, "digit must jump to confirm");
    // n backs out to the list (more than one URL), not out of the picker.
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(app.link_picker.open);
    assert!(!app.link_picker.confirm);
    // Esc from the list closes.
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.link_picker.open);
}

#[test]
fn shift_o_with_no_links_shows_toast_only() {
    let mut app = test_app();
    app.push_user("no links here");
    app.focus = Focus::Chat;
    app.handle_key(key(KeyCode::Char('O'), KeyModifiers::SHIFT));
    assert!(!app.link_picker.open, "no URLs → no picker");
    assert!(!app.toasts.is_empty(), "user must get feedback via a toast");
}

#[test]
fn tool_start_humanized_verb_is_shown_verbatim() {
    // Modern backends send a full humanized verb; the TUI must not append
    // the tool name again (that produced lines like "Running web web").
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolStart {
        agent: None,
        tool_name: "web".into(),
        verb: "Searching the web — \"NiTi damping\"".into(),
        call_id: Some("c1".into()),
        preview: None,
        approval_required: None,
    });
    let last = app.messages.last().unwrap();
    assert_eq!(last.text, "Searching the web — \"NiTi damping\"");
    // The structured tool name is still recorded for the sidebar.
    assert!(matches!(
        &last.kind,
        LineKind::ToolStart { tool_name, .. } if tool_name == "web"
    ));
}

#[test]
fn tool_card_result_without_class_is_visibly_unclassified() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        agent: None,
        tool_name: "evaluate_material".into(),
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
    // The tool declared no class. That is "nobody said", not "the model
    // asserted this with no grounding" — RED is reserved for the latter, and
    // for failures, so it still means something when it appears.
    assert_eq!(last.text, "[unclassified] evaluate_material: density=7.8");
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
        reason: None,
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
        agent: None,
        tool_name: "evaluate_material".into(),
        content: "\x1b[32mdensity=7.8\x1b[0m".into(),
        card_type: "results".into(),
        elapsed_ms: Some(150),
        call_id: None,
        provenance_id: None,
        data: None,
    });
    let last = app.messages.last().unwrap();
    assert_eq!(last.text, "[unclassified] evaluate_material: density=7.8");
    assert_no_terminal_controls(&last.text);
}

#[test]
fn tool_card_error_with_ansi_stores_sanitized_text() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolCard {
        agent: None,
        tool_name: "\x1b[31mbash\x1b[0m".into(),
        content: "exit \x1b[1m1\x1b[0m".into(),
        card_type: "error".into(),
        elapsed_ms: Some(50),
        call_id: None,
        provenance_id: None,
        data: None,
    });
    let last = app.messages.last().unwrap();
    assert_eq!(last.text, "[RED indeterminate] bash: exit 1");
    assert_no_terminal_controls(&last.text);
}

#[test]
fn tool_start_with_ansi_stores_sanitized_text() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ToolStart {
        agent: None,
        tool_name: "\x1b[36msample_material\x1b[0m".into(),
        verb: "\x1b[1mRunning\x1b[0m".into(),
        call_id: None,
        preview: None,
        approval_required: None,
    });
    let last = app.messages.last().unwrap();
    assert_eq!(last.text, "Running sample_material");
    assert_no_terminal_controls(&last.text);
}

#[test]
fn backend_error_with_ansi_stores_sanitized_text() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::BackendError {
        code: Some(500),
        message: "\x1b[31mInternal\x1b[0m error\x07".into(),
        recoverable: Some(true),
        rpc_id: None,
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
        reason: None,
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
fn view_body_with_ansi_is_sanitized_in_panel() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::View {
        title: "\x1b[1mResults\x1b[0m".into(),
        tabs: vec![("MP".into(), "\x1b[32m5 hits\x1b[0m".into())],
    });
    // Views now render in a panel; the title + body must be sanitized.
    assert!(app.view.open);
    assert!(app.view.title.contains("Results"));
    assert!(!app.view.title.contains('\x1b'));
    let body = &app.view.tabs[0].1;
    assert!(body.contains("5 hits"));
    assert!(!body.contains('\x1b'));
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

// ═══════════════════════════════════════════════════════════════════════
// Patch 3A tests: fake backend seam
// ═══════════════════════════════════════════════════════════════════════

use prism_tui::backend::{FakeBackend, FakeScenario};

// ── FakeScenario parsing ─────────────────────────────────────────────

#[test]
fn fake_scenario_parses_basic_chat() {
    let scenario = FakeScenario::from_name("basic_chat").unwrap();
    assert_eq!(scenario, FakeScenario::BasicChat);
}

#[test]
fn fake_scenario_rejects_unknown_name() {
    let err = FakeScenario::from_name("nonexistent").unwrap_err();
    assert!(err.to_string().contains("unknown fake backend scenario"));
    assert!(err.to_string().contains("basic_chat"));
}

#[test]
fn fake_scenario_as_name_roundtrips() {
    assert_eq!(FakeScenario::BasicChat.as_name(), "basic_chat");
    let s = FakeScenario::from_name(FakeScenario::BasicChat.as_name()).unwrap();
    assert_eq!(s, FakeScenario::BasicChat);
}

// ── Fake backend construction ───────────────────────────────────────

#[test]
fn fake_backend_does_not_spawn_subprocess() {
    // Construct a fake backend — this should not spawn any process.
    let backend = BackendHandle::fake(FakeScenario::BasicChat);
    // It should be the Fake variant.
    assert!(matches!(backend, BackendHandle::Fake(_)));
}

#[test]
fn real_backend_from_parts_still_compiles() {
    // The test_app() helper already uses from_parts — verify it still
    // works by constructing a test app (which uses from_parts internally).
    let _app = test_app();
}

// ── Fake backend startup events ─────────────────────────────────────

#[tokio::test]
async fn fake_backend_emits_welcome_on_startup() {
    let mut backend = FakeBackend::new(FakeScenario::BasicChat);
    let msg = backend.recv().await.expect("no message");
    assert_eq!(
        msg.get("method").and_then(|m| m.as_str()),
        Some("ui.welcome")
    );
    let params = msg.get("params").unwrap();
    assert_eq!(
        params.get("version").and_then(|v| v.as_str()),
        Some("2.7.1-fake")
    );
    assert_eq!(params.get("tool_count").and_then(|v| v.as_u64()), Some(99));
}

#[tokio::test]
async fn fake_backend_emits_status_after_welcome() {
    let mut backend = FakeBackend::new(FakeScenario::BasicChat);
    // First message is welcome
    let _welcome = backend.recv().await.unwrap();
    // Second message is status
    let msg = backend.recv().await.expect("no status message");
    assert_eq!(
        msg.get("method").and_then(|m| m.as_str()),
        Some("ui.status")
    );
    let params = msg.get("params").unwrap();
    assert_eq!(
        params.get("model").and_then(|m| m.as_str()),
        Some("fake-backend")
    );
}

// ── Fake backend send_message response ──────────────────────────────

#[tokio::test]
async fn fake_backend_send_message_emits_text_deltas() {
    let mut backend = FakeBackend::new(FakeScenario::BasicChat);
    // Drain startup events
    backend.recv().await.unwrap(); // welcome
    backend.recv().await.unwrap(); // status

    // Send a message — should enqueue response events
    backend
        .send_message("hello", serde_json::json!([]))
        .unwrap();

    // Collect all response events
    let mut methods = Vec::new();
    while let Some(msg) = backend.recv().await {
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        methods.push(method.to_string());
        if method == "ui.turn.complete" {
            break;
        }
    }

    // Should have at least one text delta, a flush, cost, and turn complete
    assert!(
        methods.iter().any(|m| m == "ui.text.delta"),
        "no text delta in: {methods:?}"
    );
    assert!(
        methods.iter().any(|m| m == "ui.text.flush"),
        "no text flush in: {methods:?}"
    );
    assert!(
        methods.iter().any(|m| m == "ui.cost"),
        "no cost in: {methods:?}"
    );
    assert!(
        methods.iter().any(|m| m == "ui.turn.complete"),
        "no turn complete in: {methods:?}"
    );
}

#[tokio::test]
async fn fake_backend_response_text_contains_fake_message() {
    let mut backend = FakeBackend::new(FakeScenario::BasicChat);
    backend.recv().await.unwrap(); // welcome
    backend.recv().await.unwrap(); // status
    backend.send_message("test", serde_json::json!([])).unwrap();

    // Collect all text deltas and concatenate
    let mut full_text = String::new();
    while let Some(msg) = backend.recv().await {
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        if method == "ui.text.delta"
            && let Some(text) = msg
                .get("params")
                .and_then(|p| p.get("text"))
                .and_then(|t| t.as_str())
        {
            full_text.push_str(text);
        }
        if method == "ui.turn.complete" {
            break;
        }
    }
    assert!(
        full_text.contains("Fake backend response"),
        "text was: {full_text}"
    );
    assert!(
        full_text.contains("deterministic test mode"),
        "text was: {full_text}"
    );
}

// ── Fake backend events pass through parse_notification ─────────────

#[tokio::test]
async fn fake_backend_events_parse_correctly() {
    use prism_tui::msg::{AgentMsg, parse_notification};

    let mut backend = FakeBackend::new(FakeScenario::BasicChat);

    // Welcome
    let welcome = backend.recv().await.unwrap();
    let parsed = parse_notification(&welcome);
    match parsed {
        AgentMsg::Welcome {
            version,
            tool_count,
            session_id,
        } => {
            assert_eq!(version, "2.7.1-fake");
            assert_eq!(tool_count, 99);
            assert_eq!(session_id.as_deref(), Some("fake-session"));
        }
        other => panic!("expected Welcome, got {other:?}"),
    }

    // Status
    let status = backend.recv().await.unwrap();
    let parsed = parse_notification(&status);
    match parsed {
        AgentMsg::Status { model, mode, .. } => {
            assert_eq!(model, "fake-backend");
            assert_eq!(mode, "chat");
        }
        other => panic!("expected Status, got {other:?}"),
    }

    // Tools catalog (pushed at startup).
    let catalog = backend.recv().await.unwrap();
    assert_eq!(
        catalog.get("method").and_then(|m| m.as_str()),
        Some("ui.tools.catalog")
    );

    // Send message and check response events parse
    backend
        .send_message("hello", serde_json::json!([]))
        .unwrap();
    let delta = backend.recv().await.unwrap();
    let parsed = parse_notification(&delta);
    assert!(
        matches!(parsed, AgentMsg::TextDelta(_)),
        "expected TextDelta, got {parsed:?}"
    );
}

// ── App receives fake backend events ─────────────────────────────────

#[tokio::test]
async fn app_with_fake_backend_produces_assistant_text() {
    // Build an App with a fake backend, apply startup events, send a
    // message, apply response events, and verify assistant text appears.
    let handle = BackendHandle::fake(FakeScenario::BasicChat);
    let mut app = prism_tui::app::App::new(handle);

    // Apply startup events (welcome + status)
    if let Some(msg) = app.backend.recv().await {
        app.handle_backend_message(&msg);
    }
    if let Some(msg) = app.backend.recv().await {
        app.handle_backend_message(&msg);
    }

    // Verify welcome was applied
    assert_eq!(app.prism_version, "2.7.1-fake");
    assert_eq!(app.tool_count, 99);
    assert_eq!(app.model, "fake-backend");

    // Simulate user sending a message
    app.push_user("hello");
    let _ = app.backend.send_message("hello", serde_json::json!([]));

    // Apply response events
    let mut received_text = String::new();
    while let Some(msg) = app.backend.recv().await {
        app.handle_backend_message(&msg);
        // Check if we got assistant text
        if let Some(last) = app.messages.last()
            && matches!(last.role, prism_tui::app::Role::Assistant)
        {
            received_text = last.text.clone();
        }
        // Stop after turn complete
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        if method == "ui.turn.complete" {
            break;
        }
    }

    // The assistant should have produced text containing the fake response
    assert!(
        received_text.contains("Fake backend response"),
        "assistant text was: {received_text}"
    );
}

// ── Fake backend slash command ───────────────────────────────────────

#[tokio::test]
async fn fake_backend_send_command_emits_response() {
    let mut backend = FakeBackend::new(FakeScenario::BasicChat);
    backend.recv().await.unwrap(); // welcome
    backend.recv().await.unwrap(); // status

    backend.send_command("/tools").unwrap();

    // Should get a status update and possibly a view, then turn complete
    let mut methods = Vec::new();
    while let Some(msg) = backend.recv().await {
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        methods.push(method.to_string());
        if method == "ui.turn.complete" {
            break;
        }
    }
    assert!(methods.iter().any(|m| m == "ui.turn.complete"));
}

// ═══════════════════════════════════════════════════════════════════════
// Patch 3B tests: scenario library
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn fake_backend_clear_changes_the_artifact_session_scope() {
    let mut backend = FakeBackend::new(FakeScenario::BasicChat);
    backend.recv().await.unwrap(); // welcome
    backend.recv().await.unwrap(); // status
    backend.recv().await.unwrap(); // tool catalog

    backend.send_command("/clear").unwrap();
    let changed = backend.recv().await.expect("session change notification");
    assert_eq!(
        changed.get("method").and_then(|method| method.as_str()),
        Some("ui.session.changed")
    );
    let session_id = changed["params"]["session_id"]
        .as_str()
        .expect("new fake session id")
        .to_string();
    let _turn_complete = backend.recv().await.expect("turn complete");

    backend.request_artifacts(10).unwrap();
    let listed = backend.recv().await.expect("artifact list");
    assert_eq!(
        listed["params"]["session_id"].as_str(),
        Some(session_id.as_str())
    );
}

/// Helper: drain startup events (welcome + status) from a fake backend.
async fn drain_startup(backend: &mut FakeBackend) {
    // Startup emits: welcome, status, tools.catalog.
    let _ = backend.recv().await; // welcome
    let _ = backend.recv().await; // status
    let _ = backend.recv().await; // tools.catalog
}

/// Helper: collect all events until ui.turn.complete, returning the
/// list of method names.
async fn collect_until_turn_complete(backend: &mut FakeBackend) -> Vec<String> {
    let mut methods = Vec::new();
    while let Some(msg) = backend.recv().await {
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        methods.push(method.to_string());
        if method == "ui.turn.complete" {
            break;
        }
    }
    methods
}

// ── Scenario name parsing ────────────────────────────────────────────

#[test]
fn all_scenario_names_parse() {
    for &name in FakeScenario::all_names() {
        assert!(
            FakeScenario::from_name(name).is_ok(),
            "failed to parse: {name}"
        );
    }
}

#[test]
fn unknown_scenario_error_lists_all() {
    let err = FakeScenario::from_name("nonexistent").unwrap_err();
    let msg = err.to_string();
    for &name in FakeScenario::all_names() {
        assert!(msg.contains(name), "error missing '{name}': {msg}");
    }
}

// ── Every scenario emits startup welcome/status ─────────────────────

#[tokio::test]
async fn all_scenarios_emit_welcome() {
    for &scenario in FakeScenario::all_names() {
        let s = FakeScenario::from_name(scenario).unwrap();
        let mut backend = FakeBackend::new(s);
        let msg = backend.recv().await.unwrap();
        assert_eq!(
            msg.get("method").and_then(|m| m.as_str()),
            Some("ui.welcome"),
            "scenario {scenario} did not emit welcome"
        );
    }
}

#[tokio::test]
async fn all_scenarios_emit_status() {
    for &scenario in FakeScenario::all_names() {
        let s = FakeScenario::from_name(scenario).unwrap();
        let mut backend = FakeBackend::new(s);
        backend.recv().await.unwrap(); // welcome
        let msg = backend.recv().await.unwrap();
        assert_eq!(
            msg.get("method").and_then(|m| m.as_str()),
            Some("ui.status"),
            "scenario {scenario} did not emit status"
        );
    }
}

// ── streaming_answer ────────────────────────────────────────────────

#[tokio::test]
async fn streaming_answer_emits_multiple_text_deltas() {
    let mut backend = FakeBackend::new(FakeScenario::StreamingAnswer);
    drain_startup(&mut backend).await;
    backend.send_message("test", serde_json::json!([])).unwrap();
    let methods = collect_until_turn_complete(&mut backend).await;
    let delta_count = methods.iter().filter(|m| *m == "ui.text.delta").count();
    assert!(
        delta_count > 5,
        "expected >5 text deltas, got {delta_count}"
    );
}

// ── thinking_stream ────────────────────────────────────────────────

#[tokio::test]
async fn thinking_stream_emits_thinking_and_text_deltas() {
    let mut backend = FakeBackend::new(FakeScenario::ThinkingStream);
    drain_startup(&mut backend).await;
    backend.send_message("test", serde_json::json!([])).unwrap();
    let methods = collect_until_turn_complete(&mut backend).await;
    assert!(
        methods.iter().any(|m| m == "ui.thinking.delta"),
        "no thinking delta"
    );
    assert!(
        methods.iter().any(|m| m == "ui.text.delta"),
        "no text delta"
    );
}

// ── tool_success ───────────────────────────────────────────────────

#[tokio::test]
async fn tool_success_emits_tool_start_and_card() {
    let mut backend = FakeBackend::new(FakeScenario::ToolSuccess);
    drain_startup(&mut backend).await;
    backend
        .send_message("sample alloy", serde_json::json!([]))
        .unwrap();
    let methods = collect_until_turn_complete(&mut backend).await;
    assert!(
        methods.iter().any(|m| m == "ui.tool.start"),
        "no tool.start"
    );
    assert!(methods.iter().any(|m| m == "ui.card"), "no card");
    // Verify the card has results (success)
}

#[tokio::test]
async fn tool_success_card_parses_as_result() {
    let mut backend = FakeBackend::new(FakeScenario::ToolSuccess);
    drain_startup(&mut backend).await;
    backend.send_message("test", serde_json::json!([])).unwrap();
    // Find the card event
    while let Some(msg) = backend.recv().await {
        if msg.get("method").and_then(|m| m.as_str()) == Some("ui.card") {
            let parsed = parse_notification(&msg);
            match parsed {
                AgentMsg::ToolCard { card_type, .. } => {
                    assert_eq!(card_type, "results");
                }
                other => panic!("expected ToolCard, got {other:?}"),
            }
            break;
        }
    }
}

// ── tool_error ──────────────────────────────────────────────────────

#[tokio::test]
async fn tool_error_emits_tool_start_and_error_card() {
    let mut backend = FakeBackend::new(FakeScenario::ToolError);
    drain_startup(&mut backend).await;
    backend
        .send_message("submit job", serde_json::json!([]))
        .unwrap();
    let methods = collect_until_turn_complete(&mut backend).await;
    assert!(
        methods.iter().any(|m| m == "ui.tool.start"),
        "no tool.start"
    );
    assert!(methods.iter().any(|m| m == "ui.card"), "no card");
}

#[tokio::test]
async fn tool_error_card_parses_as_error() {
    let mut backend = FakeBackend::new(FakeScenario::ToolError);
    drain_startup(&mut backend).await;
    backend.send_message("test", serde_json::json!([])).unwrap();
    while let Some(msg) = backend.recv().await {
        if msg.get("method").and_then(|m| m.as_str()) == Some("ui.card") {
            let parsed = parse_notification(&msg);
            match parsed {
                AgentMsg::ToolCard { card_type, .. } => {
                    assert_eq!(card_type, "error");
                }
                other => panic!("expected ToolCard, got {other:?}"),
            }
            break;
        }
    }
}

// ── approval_required ───────────────────────────────────────────────

#[tokio::test]
async fn approval_required_emits_prompt_with_rich_fields() {
    let mut backend = FakeBackend::new(FakeScenario::ApprovalRequired);
    drain_startup(&mut backend).await;
    backend
        .send_message("run compute", serde_json::json!([]))
        .unwrap();

    // The first event should be ui.prompt
    let msg = backend.recv().await.unwrap();
    assert_eq!(
        msg.get("method").and_then(|m| m.as_str()),
        Some("ui.prompt")
    );
    let parsed = parse_notification(&msg);
    match parsed {
        AgentMsg::ApprovalPrompt {
            tool_name,
            call_id,
            tool_args,
            requires_approval,
            choices,
            prompt_type,
            ..
        } => {
            assert_eq!(tool_name, "compute_submit");
            assert!(call_id.is_some());
            assert!(tool_args.is_some());
            assert_eq!(requires_approval, Some(true));
            assert!(choices.contains(&"y".to_string()));
            assert!(choices.contains(&"n".to_string()));
            assert!(choices.contains(&"a".to_string()));
            assert_eq!(prompt_type.as_deref(), Some("approval"));
        }
        other => panic!("expected ApprovalPrompt, got {other:?}"),
    }
}

#[tokio::test]
async fn approval_required_send_y_emits_tool_success() {
    let mut backend = FakeBackend::new(FakeScenario::ApprovalRequired);
    drain_startup(&mut backend).await;
    backend.send_message("run", serde_json::json!([])).unwrap();
    backend.recv().await.unwrap(); // prompt

    backend.send_approval("y", "test_tool").unwrap();
    let methods = collect_until_turn_complete(&mut backend).await;
    assert!(methods.iter().any(|m| m == "ui.card"), "no card after y");
    assert!(
        methods.iter().any(|m| m == "ui.turn.complete"),
        "no turn complete after y"
    );
}

#[tokio::test]
async fn approval_required_send_n_emits_status() {
    let mut backend = FakeBackend::new(FakeScenario::ApprovalRequired);
    drain_startup(&mut backend).await;
    backend.send_message("run", serde_json::json!([])).unwrap();
    backend.recv().await.unwrap(); // prompt

    backend.send_approval("n", "test_tool").unwrap();
    let methods = collect_until_turn_complete(&mut backend).await;
    assert!(
        methods.iter().any(|m| m == "ui.turn.complete"),
        "no turn complete after n"
    );
}

#[tokio::test]
async fn approval_required_send_a_emits_permissions_and_card() {
    let mut backend = FakeBackend::new(FakeScenario::ApprovalRequired);
    drain_startup(&mut backend).await;
    backend.send_message("run", serde_json::json!([])).unwrap();
    backend.recv().await.unwrap(); // prompt

    backend.send_approval("a", "test_tool").unwrap();
    let methods = collect_until_turn_complete(&mut backend).await;
    assert!(
        methods.iter().any(|m| m == "ui.permissions"),
        "no permissions after a"
    );
    assert!(methods.iter().any(|m| m == "ui.card"), "no card after a");
}

// ── cost_metrics ────────────────────────────────────────────────────

#[tokio::test]
async fn cost_metrics_emits_cost_with_token_counts() {
    let mut backend = FakeBackend::new(FakeScenario::CostMetrics);
    drain_startup(&mut backend).await;
    backend
        .send_message("show cost", serde_json::json!([]))
        .unwrap();

    let msg = backend.recv().await.unwrap();
    assert_eq!(msg.get("method").and_then(|m| m.as_str()), Some("ui.cost"));
    let parsed = parse_notification(&msg);
    match parsed {
        AgentMsg::Cost {
            input_tokens,
            output_tokens,
            cache_tokens,
            ..
        } => {
            assert_eq!(input_tokens, Some(1200));
            assert_eq!(output_tokens, Some(800));
            assert_eq!(cache_tokens, Some(400));
        }
        other => panic!("expected Cost, got {other:?}"),
    }
}

// ── backend_warning_error ──────────────────────────────────────────

#[tokio::test]
async fn backend_warning_error_emits_warning_and_error() {
    let mut backend = FakeBackend::new(FakeScenario::BackendWarningError);
    drain_startup(&mut backend).await;
    backend
        .send_message("trigger error", serde_json::json!([]))
        .unwrap();

    let msg1 = backend.recv().await.unwrap();
    assert_eq!(
        msg1.get("method").and_then(|m| m.as_str()),
        Some("ui.backend.warning")
    );
    let parsed1 = parse_notification(&msg1);
    assert!(matches!(parsed1, AgentMsg::BackendWarning { .. }));

    let msg2 = backend.recv().await.unwrap();
    assert_eq!(
        msg2.get("method").and_then(|m| m.as_str()),
        Some("ui.backend.error")
    );
    let parsed2 = parse_notification(&msg2);
    match parsed2 {
        AgentMsg::BackendError {
            code, recoverable, ..
        } => {
            assert_eq!(code, Some(429));
            assert_eq!(recoverable, Some(true));
        }
        other => panic!("expected BackendError, got {other:?}"),
    }
}

// ── ansi_injection ──────────────────────────────────────────────────

#[tokio::test]
async fn ansi_injection_events_contain_ansi() {
    let mut backend = FakeBackend::new(FakeScenario::AnsiInjection);
    drain_startup(&mut backend).await;
    backend
        .send_message("inject", serde_json::json!([]))
        .unwrap();

    // First event: text delta with ANSI
    let msg = backend.recv().await.unwrap();
    let text = msg
        .get("params")
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("");
    assert!(text.contains("\x1b["), "text delta should contain ANSI");
}

#[tokio::test]
async fn ansi_injection_app_state_is_sanitized() {
    // Verify the sanitizer strips ANSI from visible App state.
    let handle = BackendHandle::fake(FakeScenario::AnsiInjection);
    let mut app = prism_tui::app::App::new(handle);

    // Drain startup
    if let Some(msg) = app.backend.recv().await {
        app.handle_backend_message(&msg);
    }
    if let Some(msg) = app.backend.recv().await {
        app.handle_backend_message(&msg);
    }

    // Send a message to trigger the ansi_injection response
    app.push_user("inject");
    let _ = app.backend.send_message("inject", serde_json::json!([]));

    // Apply all response events (with timeout for scenarios that
    // don't emit ui.turn.complete).
    let timeout = tokio::time::Duration::from_millis(500);
    loop {
        match tokio::time::timeout(timeout, app.backend.recv()).await {
            Ok(Some(msg)) => {
                app.handle_backend_message(&msg);
                let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
                if method == "ui.turn.complete" {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => break, // timeout
        }
    }

    // Assert no terminal controls in any visible text
    for line in &app.messages {
        assert_no_terminal_controls(&line.text);
    }
}

// ── All scenarios pass through parse_notification ──────────────────

#[tokio::test]
async fn all_scenarios_events_parse_without_unknown() {
    for &scenario_name in FakeScenario::all_names() {
        let scenario = FakeScenario::from_name(scenario_name).unwrap();
        let mut backend = FakeBackend::new(scenario);
        drain_startup(&mut backend).await;
        backend.send_message("test", serde_json::json!([])).unwrap();

        // Collect events with a timeout — some scenarios (e.g.
        // approval_required) don't emit ui.turn.complete after
        // send_message; they wait for send_approval instead.
        let mut found_unknown = false;
        let timeout = tokio::time::Duration::from_millis(500);
        loop {
            match tokio::time::timeout(timeout, backend.recv()).await {
                Ok(Some(msg)) => {
                    let parsed = parse_notification(&msg);
                    if matches!(parsed, AgentMsg::Unknown(_)) {
                        found_unknown = true;
                        break;
                    }
                    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
                    if method == "ui.turn.complete" {
                        break;
                    }
                }
                Ok(None) => break, // channel closed
                Err(_) => break,   // timeout — no more events
            }
        }
        assert!(
            !found_unknown,
            "scenario {scenario_name} produced Unknown events"
        );
    }
}

// ── Objects tab ───────────────────────────────────────────────────────

#[test]
fn object_update_running_keeps_running_status_in_the_model() {
    // MODEL-level only — this file has no render harness, so it cannot check
    // anything about rendering and used to be named as if it did. The render
    // invariant lives in tests/render_snapshots.rs
    // (`a_running_object_never_renders_as_done`), which is where
    // `render_app_to_string` is.
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "sim-1".into(),
        kind: "simulation".into(),
        label: "MD NPT 300K".into(),
        status: "running".into(),
        progress_current: Some(5000),
        progress_total: Some(10000),
        detail: None,
    });
    assert_eq!(app.objects.len(), 1);
    let obj = &app.objects[0];
    assert_eq!(obj.status, ObjectStatus::Running);
    assert_eq!(obj.progress, Some((5000, 10000)));
    assert!(
        obj.detail.is_none(),
        "running object must not have a result detail"
    );

    // Simulate an update with NO progress — must still be running.
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "sim-1".into(),
        kind: "simulation".into(),
        label: "MD NPT 300K".into(),
        status: "running".into(),
        progress_current: None,
        progress_total: None,
        detail: None,
    });
    let obj = &app.objects[0];
    assert_eq!(obj.status, ObjectStatus::Running);
    assert!(
        obj.progress.is_none(),
        "no progress reported yet — must stay None, not invented"
    );
}

/// Notifications are not ordered. A `running` tick emitted just before the
/// job finished can arrive after `completed` (retry, replay, the 50-step
/// progress callback racing the finish). Before the guard, the upsert
/// overwrote unconditionally and a finished simulation went back to
/// "running" — the user would then wait for a result he already had.
/// Mutation: delete the `if !existing.status.is_terminal()` guard in
/// `app.rs` and this fails on the status assertion.
#[test]
fn object_terminal_status_is_never_resurrected_by_a_late_running() {
    let mut app = test_app();
    for status in ["running", "completed"] {
        app.apply_agent_msg(AgentMsg::ObjectUpdate {
            id: "sim-9".into(),
            kind: "simulation".into(),
            label: "MD NPT 300K".into(),
            status: status.into(),
            progress_current: Some(10000),
            progress_total: Some(10000),
            detail: None,
        });
    }
    assert_eq!(app.objects[0].status, ObjectStatus::Completed);

    // The straggler.
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "sim-9".into(),
        kind: "simulation".into(),
        label: "MD NPT 300K".into(),
        status: "running".into(),
        progress_current: Some(9950),
        progress_total: Some(10000),
        detail: None,
    });
    assert_eq!(
        app.objects[0].status,
        ObjectStatus::Completed,
        "a late `running` must not resurrect a finished object"
    );
    assert_eq!(
        app.objects[0].progress,
        Some((10000, 10000)),
        "the stale progress must not overwrite the final one either"
    );
}

/// An unrecognised status string used to fall through to `Running`, so a
/// `cancelled` or `queued` object rendered as actively running — a state the
/// backend never reported. Mutation: change the `_` arm back to
/// `Self::Running` and this fails.
#[test]
fn object_unrecognised_status_is_not_reported_as_running() {
    let mut app = test_app();
    for (i, status) in ["cancelled", "queued", "skipped"].iter().enumerate() {
        app.apply_agent_msg(AgentMsg::ObjectUpdate {
            id: format!("obj-{i}"),
            kind: "simulation".into(),
            label: format!("job {i}"),
            status: (*status).into(),
            progress_current: None,
            progress_total: None,
            detail: None,
        });
    }
    for obj in &app.objects {
        assert_eq!(
            obj.status,
            ObjectStatus::Unknown,
            "`{}` is not a status this build knows — claiming it is Running \
             invents state the backend never reported",
            obj.label
        );
    }
}

/// `parse_notification` defaulted an absent `status` to the literal "running"
/// and an absent `id` to "". Both fabricate: the first claims a state the
/// backend never reported, the second makes every anonymous update collapse
/// onto ONE row, so unrelated simulations overwrite each other in front of
/// the user. Mutations: restore `.unwrap_or("running")` in msg.rs, or delete
/// the empty-id guard in app.rs — each fails this.
#[test]
fn object_update_without_id_or_status_is_not_invented() {
    let mut app = test_app();

    // No id at all — unaddressable, must be dropped rather than merged.
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "".into(),
        kind: "simulation".into(),
        label: "anonymous A".into(),
        status: "running".into(),
        progress_current: None,
        progress_total: None,
        detail: None,
    });
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "   ".into(),
        kind: "simulation".into(),
        label: "anonymous B".into(),
        status: "running".into(),
        progress_current: None,
        progress_total: None,
        detail: None,
    });
    assert!(
        app.objects.is_empty(),
        "an update with no id is unaddressable: nothing can target it again, so it \
         becomes a row that never updates — and two updates sharing the SAME empty \
         id silently overwrite each other. Leaked: {:?}",
        app.objects.iter().map(|o| &o.label).collect::<Vec<_>>()
    );

    // Absent status (empty after parse_notification's default) is Unknown,
    // NOT Running.
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "sim-x".into(),
        kind: "simulation".into(),
        label: "no status reported".into(),
        status: "".into(),
        progress_current: None,
        progress_total: None,
        detail: None,
    });
    assert_eq!(
        app.objects[0].status,
        ObjectStatus::Unknown,
        "the backend reported no status — claiming Running invents it"
    );
}

/// The PARSE-side default, which the test above does not reach — it builds an
/// AgentMsg directly. I mutation-checked that: restoring
/// `.unwrap_or("running")` in msg.rs left it green. A notification that omits
/// `status` must not become Running on the way in.
#[test]
fn parsed_object_update_without_status_does_not_become_running() {
    let msg = json!({
        "method": "ui.object.update",
        "params": { "id": "sim-p", "kind": "simulation", "label": "MD 300K" }
    });
    match parse_notification(&msg) {
        AgentMsg::ObjectUpdate { status, .. } => {
            assert_ne!(
                status, "running",
                "the notification carried no status — defaulting to `running` \
                 reports a state the backend never sent"
            );
            assert_eq!(
                ObjectStatus::from_str_loose(&status),
                ObjectStatus::Unknown,
                "an absent status must land on Unknown, got {status:?}"
            );
        }
        other => panic!("expected ObjectUpdate, got {other:?}"),
    }
}

/// PRISM is not a metals tool. A ceramic, composite, MOF or electrolyte used
/// to collapse into `ObjectKind::Result` — the UI told the user a ceramic was
/// a "Result", a label the backend never sent. Same class of lie as the status
/// defect in 746ec620, and the same mistake the ml_train design warns about:
/// class is DATA, not an enum arm.
///
/// Mutation: restore `_ => Self::Result` in `from_str_loose` and this fails.
#[test]
fn an_unknown_material_class_keeps_its_own_name() {
    for kind in ["ceramic", "composite", "MOF", "electrolyte", "thin_film"] {
        let parsed = prism_tui::app::ObjectKind::from_str_loose(kind);
        assert_eq!(
            parsed.as_str(),
            kind,
            "`{kind}` must render as itself, not be relabelled"
        );
        assert_ne!(
            parsed,
            prism_tui::app::ObjectKind::Result,
            "`{kind}` collapsed into Result — that invents a label"
        );
    }
    // The known kinds still resolve, and an empty kind is honestly a Result
    // rather than an object named "".
    assert_eq!(
        prism_tui::app::ObjectKind::from_str_loose("polymer"),
        prism_tui::app::ObjectKind::Polymer
    );
    assert_eq!(
        prism_tui::app::ObjectKind::from_str_loose("  "),
        prism_tui::app::ObjectKind::Result
    );
}

/// Opening `ObjectKind` to `Other(String)` put BACKEND-SUPPLIED text on the
/// path to the terminal for the first time — every variant used to be a
/// `&'static str`. `label` and `detail` have always gone through
/// `sanitize_for_render`; `kind` had never needed to, so it did not. That is
/// an ANSI-injection vector into the sidebar, the exact class
/// `snapshot_ansi_injection_sanitized` exists for.
///
/// Mutation: drop the `sanitize_for_render` around `kind` in `apply_agent_msg`
/// and this fails.
#[test]
fn an_unknown_kind_cannot_carry_terminal_control_sequences() {
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "obj-esc".into(),
        kind: "cera\u{1b}[31mmic\u{7}".into(),
        label: "evil".into(),
        status: "running".into(),
        progress_current: None,
        progress_total: None,
        detail: None,
    });
    let rendered = app.objects[0].kind.as_str().to_string();
    for (name, ch) in [("ESC", '\u{1b}'), ("BEL", '\u{7}')] {
        assert!(
            !rendered.contains(ch),
            "{name} survived into the rendered kind: {rendered:?}"
        );
    }
}

#[test]
fn object_update_failed_shows_error() {
    // A failed simulation must NOT silently vanish or read as done.
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "sim-2".into(),
        kind: "simulation".into(),
        label: "MD NVT 500K".into(),
        status: "running".into(),
        progress_current: Some(2341),
        progress_total: Some(5000),
        detail: None,
    });
    // Now it fails.
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "sim-2".into(),
        kind: "simulation".into(),
        label: "MD NVT 500K".into(),
        status: "failed".into(),
        progress_current: None,
        progress_total: None,
        detail: Some("divergence at step 2341".into()),
    });
    assert_eq!(app.objects.len(), 1);
    let obj = &app.objects[0];
    assert_eq!(obj.status, ObjectStatus::Failed);
    assert_eq!(
        obj.detail.as_deref(),
        Some("divergence at step 2341"),
        "failed sim must show its error"
    );
}

#[test]
fn object_tag_toggle_and_send_message_prefix() {
    // Tagging must put the object into the outgoing message, using
    // the same prefix mechanism as the standing goal.
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "alloy-1".into(),
        kind: "alloy".into(),
        label: "CrMnFeCoNi".into(),
        status: "completed".into(),
        progress_current: None,
        progress_total: None,
        detail: Some("best HEA candidate".into()),
    });
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "struct-1".into(),
        kind: "structure".into(),
        label: "W-BCC".into(),
        status: "completed".into(),
        progress_current: None,
        progress_total: None,
        detail: None,
    });
    assert_eq!(app.objects.len(), 2);

    // Switch to Objects tab and tag the first one.
    app.workspace_tab = WorkspaceTab::Objects;
    app.workspace_selected = 0;
    app.focus = Focus::Workspace;
    app.handle_key(key(KeyCode::Char('t'), KeyModifiers::NONE));
    assert!(app.objects[0].tagged, "t must tag the selected object");
    assert!(!app.objects[1].tagged, "other objects stay untagged");

    // Untag it.
    app.handle_key(key(KeyCode::Char('t'), KeyModifiers::NONE));
    assert!(!app.objects[0].tagged, "second t must untag");

    // Tag both and verify the outgoing message prefix.
    app.objects[0].tagged = true;
    app.objects[1].tagged = true;
    // The send_message path writes to the backend (cat subprocess).
    // We verify indirectly: tagged objects should produce a non-empty
    // context block in the payload logic. Let's call send_message
    // and check it doesn't panic (the cat backend absorbs the write).
    app.focus = Focus::Input;
    app.input.insert_str("analyze this");
    // Should not panic even with tagged objects.
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    // The message was sent — is_waiting flips true.
    assert!(
        app.is_waiting,
        "send_message must set is_waiting after sending"
    );
}

#[test]
fn object_upsert_updates_in_place() {
    // Same id must update in place, not create a duplicate.
    let mut app = test_app();
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "sim-1".into(),
        kind: "simulation".into(),
        label: "MD run".into(),
        status: "running".into(),
        progress_current: Some(100),
        progress_total: Some(1000),
        detail: None,
    });
    assert_eq!(app.objects.len(), 1);
    app.apply_agent_msg(AgentMsg::ObjectUpdate {
        id: "sim-1".into(),
        kind: "simulation".into(),
        label: "MD run".into(),
        status: "running".into(),
        progress_current: Some(500),
        progress_total: Some(1000),
        detail: None,
    });
    assert_eq!(app.objects.len(), 1, "upsert must not duplicate");
    assert_eq!(app.objects[0].progress, Some((500, 1000)));
}

#[test]
fn objects_tab_empty_state_does_not_look_broken() {
    // Empty tab must say so plainly.
    let mut app = test_app();
    assert!(app.objects.is_empty());
    app.workspace_tab = WorkspaceTab::Objects;
    app.focus = Focus::Workspace;
    // Enter on empty tab should toast, not open detail modal.
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(!app.view.open, "empty objects tab must not open a modal");
    assert!(!app.toasts.is_empty(), "user must get feedback via a toast");
}

#[test]
fn parse_object_update_notification() {
    let msg = json!({
        "jsonrpc": "2.0",
        "method": "ui.object.update",
        "params": {
            "id": "obj-1",
            "kind": "polymer",
            "label": "PEG-4000",
            "status": "running",
            "progress_current": 50,
            "progress_total": 200,
        },
    });
    let parsed = parse_notification(&msg);
    match parsed {
        AgentMsg::ObjectUpdate {
            id,
            kind,
            label,
            status,
            progress_current,
            progress_total,
            detail,
        } => {
            assert_eq!(id, "obj-1");
            assert_eq!(kind, "polymer");
            assert_eq!(label, "PEG-4000");
            assert_eq!(status, "running");
            assert_eq!(progress_current, Some(50));
            assert_eq!(progress_total, Some(200));
            assert!(detail.is_none());
        }
        other => panic!("expected ObjectUpdate, got {other:?}"),
    }
}

// ── Structures plane (materials sidebar) ────────────────────────────

fn structure_row_json(cache_key: &str) -> serde_json::Value {
    json!({
        "cache_key": cache_key,
        "cache_ref": format!("cache://{cache_key}/structure.cif"),
        "tool": "structure_import",
        "name": "TiAl gamma",
        "formula": "TiAl",
        "n_atoms": 2,
        "composition": {"Al": 1, "Ti": 1},
        "source": "user_import",
    })
}

#[test]
fn parse_structures_list_notification() {
    let msg = json!({
        "jsonrpc": "2.0",
        "method": "ui.structures.list",
        "params": {
            "session_id": "session-a",
            "structures": [structure_row_json("key-1")],
        },
    });
    match parse_notification(&msg) {
        AgentMsg::StructuresListed {
            session_id,
            structures,
        } => {
            assert_eq!(session_id, "session-a");
            assert_eq!(structures.len(), 1);
        }
        other => panic!("expected StructuresListed, got {other:?}"),
    }
}

#[test]
fn parse_structures_list_malformed_becomes_unavailable() {
    // Missing session_id — never silently accepted.
    let msg = json!({
        "jsonrpc": "2.0",
        "method": "ui.structures.list",
        "params": {"structures": []},
    });
    assert!(matches!(
        parse_notification(&msg),
        AgentMsg::StructureStoreUnavailable { .. }
    ));
}

#[test]
fn parse_structure_fetched_notification() {
    let msg = json!({
        "jsonrpc": "2.0",
        "method": "ui.structure.fetched",
        "params": {
            "session_id": "session-a",
            "cache_key": "key-1",
            "cif": "data_TiAl\n",
            "truncated": true,
        },
    });
    match parse_notification(&msg) {
        AgentMsg::StructureFetched {
            session_id,
            cache_key,
            cif,
            truncated,
        } => {
            assert_eq!(session_id, "session-a");
            assert_eq!(cache_key, "key-1");
            assert_eq!(cif, "data_TiAl\n");
            assert!(truncated);
        }
        other => panic!("expected StructureFetched, got {other:?}"),
    }
}

#[test]
fn parse_structure_fetched_malformed_becomes_error() {
    // Missing CIF text — a fetch response without the text is an error,
    // not an empty CIF.
    let msg = json!({
        "jsonrpc": "2.0",
        "method": "ui.structure.fetched",
        "params": {"session_id": "session-a", "cache_key": "key-1"},
    });
    assert!(matches!(
        parse_notification(&msg),
        AgentMsg::StructureFetchError { .. }
    ));
}

#[test]
fn parse_structure_error_notification_keeps_key() {
    let msg = json!({
        "jsonrpc": "2.0",
        "method": "ui.structure.error",
        "params": {"cache_key": "key-9", "message": "not found"},
    });
    match parse_notification(&msg) {
        AgentMsg::StructureFetchError { cache_key, message } => {
            assert_eq!(cache_key.as_deref(), Some("key-9"));
            assert_eq!(message, "not found");
        }
        other => panic!("expected StructureFetchError, got {other:?}"),
    }
}

#[test]
fn jsonrpc_error_response_carries_the_request_id() {
    let msg = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "error": {"code": -32601, "message": "Method not found: workspace.structures.list"},
    });
    match parse_notification(&msg) {
        AgentMsg::BackendError { code, rpc_id, .. } => {
            assert_eq!(code, Some(-32601));
            assert_eq!(rpc_id, Some(7));
        }
        other => panic!("expected BackendError, got {other:?}"),
    }
}

#[test]
fn structures_listed_populates_the_store() {
    let mut app = test_app();
    app.session_id = Some("session-a".into());
    app.workspace_selected = 5; // must be clamped to the row count

    app.apply_agent_msg(AgentMsg::StructuresListed {
        session_id: "session-a".into(),
        structures: vec![structure_row_json("key-1"), structure_row_json("key-2")],
    });

    match &app.structure_store {
        prism_tui::structures::StructuresStoreState::Ready(rows) => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].formula_display(), "TiAl");
            assert_eq!(rows[0].source_display(), "user_import");
        }
        other => panic!("expected Ready store, got {other:?}"),
    }
    assert_eq!(app.workspace_selected, 1);
}

#[test]
fn structures_listed_from_a_stale_session_is_not_trusted() {
    let mut app = test_app();
    app.session_id = Some("session-b".into());

    app.apply_agent_msg(AgentMsg::StructuresListed {
        session_id: "session-a".into(),
        structures: vec![structure_row_json("key-1")],
    });

    assert!(
        matches!(
            &app.structure_store,
            prism_tui::structures::StructuresStoreState::Loading
        ),
        "stale response must leave the store loading, not populate it"
    );
}

#[test]
fn structures_listed_with_a_malformed_row_is_unavailable() {
    let mut app = test_app();
    app.session_id = Some("session-a".into());

    app.apply_agent_msg(AgentMsg::StructuresListed {
        session_id: "session-a".into(),
        // Second row has no cache_key — the list is malformed, and a
        // half-parsed store would be a lie of omission.
        structures: vec![structure_row_json("key-1"), json!({"formula": "TiAl"})],
    });

    assert!(matches!(
        &app.structure_store,
        prism_tui::structures::StructuresStoreState::Unavailable(_)
    ));
}

#[test]
fn structure_unavailable_and_empty_are_distinct_facts() {
    let mut app = test_app();
    app.session_id = Some("session-a".into());

    app.apply_agent_msg(AgentMsg::StructuresListed {
        session_id: "session-a".into(),
        structures: vec![],
    });
    assert!(
        matches!(
            &app.structure_store,
            prism_tui::structures::StructuresStoreState::Ready(rows) if rows.is_empty()
        ),
        "an empty list is healthy"
    );

    app.apply_agent_msg(AgentMsg::StructureStoreUnavailable {
        message: "cache directory unreadable".into(),
    });
    assert!(
        matches!(
            &app.structure_store,
            prism_tui::structures::StructuresStoreState::Unavailable(reason)
                if reason.contains("cache directory unreadable")
        ),
        "unavailable must never collapse into empty"
    );
}

#[test]
fn structures_enter_on_empty_tab_toasts_instead_of_opening() {
    let mut app = test_app();
    app.session_id = Some("session-a".into());
    app.apply_agent_msg(AgentMsg::StructuresListed {
        session_id: "session-a".into(),
        structures: vec![],
    });
    app.workspace_tab = WorkspaceTab::Structures;
    app.focus = Focus::Workspace;

    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));

    assert!(!app.view.open);
    assert!(
        app.toasts
            .iter()
            .any(|t| t.message.contains("no structures yet")),
        "empty structures tab must say why"
    );
}

#[test]
fn structures_enter_fetches_cif_into_the_view_panel() {
    let mut app = test_app();
    app.session_id = Some("session-a".into());
    app.apply_agent_msg(AgentMsg::StructuresListed {
        session_id: "session-a".into(),
        structures: vec![structure_row_json("key-1")],
    });
    app.workspace_tab = WorkspaceTab::Structures;
    app.focus = Focus::Workspace;

    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.view.open);
    assert!(app.view.title.contains("TiAl"), "formula leads the title");
    assert!(
        app.view.tabs[0].1.contains("Loading CIF"),
        "the panel must say the CIF is on its way"
    );

    app.apply_agent_msg(AgentMsg::StructureFetched {
        session_id: "session-a".into(),
        cache_key: "key-1".into(),
        cif: "data_TiAl\n_cell_length_a 4.005\n".into(),
        truncated: false,
    });

    let body = &app.view.tabs[0].1;
    assert!(body.contains("data_TiAl"), "the actual CIF text is shown");
    assert!(
        body.contains("cache://key-1/structure.cif"),
        "full ref visible"
    );
    assert!(body.contains("source:      user_import"), "source visible");
}

#[test]
fn structure_fetch_for_another_key_does_not_hijack_the_view() {
    let mut app = test_app();
    app.session_id = Some("session-a".into());
    app.apply_agent_msg(AgentMsg::StructuresListed {
        session_id: "session-a".into(),
        structures: vec![structure_row_json("key-1")],
    });
    app.workspace_tab = WorkspaceTab::Structures;
    app.focus = Focus::Workspace;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));

    app.apply_agent_msg(AgentMsg::StructureFetched {
        session_id: "session-a".into(),
        cache_key: "key-OTHER".into(),
        cif: "data_SomethingElse\n".into(),
        truncated: false,
    });

    assert!(
        app.view.tabs[0].1.contains("Loading CIF"),
        "a fetch response for a key we did not ask for must be ignored"
    );
}

#[test]
fn structure_fetch_error_is_shown_in_the_view() {
    let mut app = test_app();
    app.session_id = Some("session-a".into());
    app.apply_agent_msg(AgentMsg::StructuresListed {
        session_id: "session-a".into(),
        structures: vec![structure_row_json("key-1")],
    });
    app.workspace_tab = WorkspaceTab::Structures;
    app.focus = Focus::Workspace;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));

    app.apply_agent_msg(AgentMsg::StructureFetchError {
        cache_key: Some("key-1".into()),
        message: "no structure with cache key 'key-1'".into(),
    });

    assert!(app.view.tabs[0].1.contains("Structure unavailable"));
}

#[test]
fn backend_method_not_found_becomes_unavailable_not_chat_noise() {
    // A backend without structures support answers -32601 to the list
    // request. That is "structure cache unavailable" — not a chat error.
    let mut app = test_app();
    app.session_id = Some("session-a".into());
    app.structure_policy.refresh_debounce = std::time::Duration::ZERO;

    // Enter the Structures tab through the real key path, then let the
    // event-loop poll send the list request (id 1 on a fresh backend).
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Objects;
    app.handle_key(key(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(app.workspace_tab, WorkspaceTab::Structures);
    app.poll_structure_requests();

    app.apply_agent_msg(AgentMsg::BackendError {
        code: Some(-32601),
        message: "Method not found: workspace.structures.list".into(),
        recoverable: None,
        rpc_id: Some(1),
    });

    match &app.structure_store {
        prism_tui::structures::StructuresStoreState::Unavailable(reason) => {
            assert!(reason.contains("Method not found"));
        }
        other => panic!("expected Unavailable store, got {other:?}"),
    }
    assert!(
        !app.messages
            .iter()
            .any(|m| matches!(m.kind, LineKind::Error(..))),
        "an attributed protocol error must not land in the chat transcript"
    );

    // An UNattributed backend error still reaches the chat, as before.
    app.apply_agent_msg(AgentMsg::BackendError {
        code: Some(500),
        message: "boom".into(),
        recoverable: None,
        rpc_id: None,
    });
    assert!(app.messages.iter().any(|m| m.text.contains("boom")));
}

#[test]
fn structures_tab_cycles_between_objects_and_artifacts() {
    let mut app = test_app();
    app.focus = Focus::Workspace;
    app.workspace_tab = WorkspaceTab::Objects;
    app.handle_key(key(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(app.workspace_tab, WorkspaceTab::Structures);
    app.handle_key(key(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(app.workspace_tab, WorkspaceTab::Artifacts);
    app.handle_key(key(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(app.workspace_tab, WorkspaceTab::Structures);
    app.handle_key(key(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(app.workspace_tab, WorkspaceTab::Objects);
}

/// Adding a provider must produce an entry PRISM's registry actually loads,
/// and must never write the key into the shared providers file.
///
/// The bar the owner set is: palette → paste URL → paste key → done. Before
/// this, the only way to add a provider PRISM did not ship was editing
/// `~/.prism/providers.toml` by hand.
#[test]
fn a_display_name_slugs_into_a_registry_id() {
    use prism_tui::app::App;
    assert_eq!(App::provider_slug("Alibaba DashScope"), "alibaba-dashscope");
    assert_eq!(App::provider_slug("z.ai  GLM (coding)"), "z-ai-glm-coding");
    assert_eq!(App::provider_slug("  Moonshot  "), "moonshot");
    // Nothing usable in it -> refused upstream rather than minting an empty id.
    assert_eq!(App::provider_slug("!!!"), "");
}

/// Clicking a workspace row selects it, exactly as the keyboard would.
///
/// Mouse presses reached `handle_mouse` all along and were discarded — only
/// the wheel was handled, and `ev.column`/`ev.row` were read nowhere in the
/// crate. Pointing could not refer to anything.
///
/// Pointing and typing must land on the SAME selection state, or the sidebar
/// would disagree with itself depending on which you used last.
#[test]
fn clicking_a_workspace_row_selects_it() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use prism_tui::app::WorkspaceTab;
    use prism_tui::hit_map::HitTarget;
    use ratatui::layout::Rect;

    let mut app = test_app();
    app.hit_map.borrow_mut().push(
        Rect::new(60, 8, 20, 1),
        HitTarget::WorkspaceRow {
            tab: WorkspaceTab::Artifacts,
            index: 4,
        },
    );

    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 65,
        row: 8,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });

    assert_eq!(app.workspace_tab, WorkspaceTab::Artifacts);
    assert_eq!(
        app.workspace_selected, 4,
        "the clicked row is the selected row"
    );
}

/// Hover records what is under the pointer and nothing else.
///
/// Deliberately asserts that only the reference is held: what it points at is
/// fetched when opened, never when the pointer passes over it. Pre-resolving
/// would do work for every mark the reader never looks at.
#[test]
fn hovering_records_the_target_under_the_pointer() {
    use crossterm::event::{MouseEvent, MouseEventKind};
    use prism_tui::hit_map::HitTarget;
    use ratatui::layout::Rect;

    let mut app = test_app();
    app.hit_map.borrow_mut().push(
        Rect::new(4, 2, 30, 1),
        HitTarget::Reference {
            id: "structure:cache://a3f9".to_string(),
        },
    );

    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 10,
        row: 2,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
    assert_eq!(
        app.hovered,
        Some(HitTarget::Reference {
            id: "structure:cache://a3f9".to_string()
        })
    );

    // Off the mark: hover clears rather than sticking to the last thing seen.
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 10,
        row: 9,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
    assert_eq!(app.hovered, None, "moving off a mark must clear the hover");
}

#[test]
fn a_single_body_view_is_one_tab() {
    // Live 2026-09-05: `/papers search` answered with a `ui.view` carrying
    // `title` and `body`, and the panel read "nothing to show … yet". The
    // parser read only `tabs`; every single-body view from the backend
    // (doctor, providers, billing, papers) rendered empty.
    let msg = parse_notification(&serde_json::json!({
        "method": "ui.view",
        "params": {"view_type": "papers", "title": "Papers — search", "body": "2 paper(s)\n  • A", "tone": "info"}
    }));
    match msg {
        AgentMsg::View { title, tabs } => {
            assert_eq!(title, "Papers — search");
            assert_eq!(tabs.len(), 1, "the body is the one tab");
            assert_eq!(tabs[0].0, "Papers — search", "named after the view");
            assert!(tabs[0].1.contains("2 paper(s)"), "{:?}", tabs[0].1);
        }
        other => panic!("expected a View, got {other:?}"),
    }
}
