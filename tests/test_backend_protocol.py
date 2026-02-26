"""Tests for the UI protocol event definitions."""
import pytest


def test_ui_events_dict_exists():
    from app.backend.protocol import UI_EVENTS
    assert isinstance(UI_EVENTS, dict)
    assert "ui.text.delta" in UI_EVENTS
    assert "ui.card" in UI_EVENTS
    assert "ui.cost" in UI_EVENTS
    assert "ui.prompt" in UI_EVENTS
    assert "ui.welcome" in UI_EVENTS
    assert "ui.turn.complete" in UI_EVENTS


def test_input_events_dict_exists():
    from app.backend.protocol import INPUT_EVENTS
    assert isinstance(INPUT_EVENTS, dict)
    assert "init" in INPUT_EVENTS
    assert "input.message" in INPUT_EVENTS
    assert "input.command" in INPUT_EVENTS
    assert "input.prompt_response" in INPUT_EVENTS


def test_make_event_creates_valid_jsonrpc():
    from app.backend.protocol import make_event
    event = make_event("ui.text.delta", {"text": "hello"})
    assert event["jsonrpc"] == "2.0"
    assert event["method"] == "ui.text.delta"
    assert event["params"]["text"] == "hello"


def test_make_event_rejects_unknown_method():
    from app.backend.protocol import make_event
    with pytest.raises(ValueError, match="Unknown event"):
        make_event("ui.nonexistent", {})


def test_parse_input_parses_valid_jsonrpc():
    from app.backend.protocol import parse_input
    msg = '{"jsonrpc":"2.0","method":"input.message","params":{"text":"hello"},"id":1}'
    parsed = parse_input(msg)
    assert parsed["method"] == "input.message"
    assert parsed["params"]["text"] == "hello"
    assert parsed["id"] == 1


def test_parse_input_rejects_invalid_json():
    from app.backend.protocol import parse_input
    with pytest.raises(ValueError):
        parse_input("not json")


def test_all_event_methods_listed():
    """Ensure all 10 backend->frontend and 5 frontend->backend events exist."""
    from app.backend.protocol import UI_EVENTS, INPUT_EVENTS
    expected_ui = [
        "ui.text.delta", "ui.text.flush", "ui.tool.start", "ui.card",
        "ui.cost", "ui.prompt", "ui.welcome", "ui.status",
        "ui.turn.complete", "ui.session.list",
    ]
    expected_input = [
        "init", "input.message", "input.command",
        "input.prompt_response", "input.load_session",
    ]
    for method in expected_ui:
        assert method in UI_EVENTS, f"Missing UI event: {method}"
    for method in expected_input:
        assert method in INPUT_EVENTS, f"Missing input event: {method}"


def test_emit_ts_produces_typescript():
    """The --emit-ts flag should produce parseable TypeScript type declarations."""
    from app.backend.protocol import emit_typescript
    ts = emit_typescript()
    assert "export interface" in ts or "export type" in ts
    assert "UiTextDelta" in ts
    assert "UiCard" in ts
    assert "InputMessage" in ts
