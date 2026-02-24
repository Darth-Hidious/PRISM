"""Tests for TUI key bindings."""

def test_keymap_has_required_bindings():
    from app.tui.keymap import KEYMAP
    required = ["ctrl+o", "ctrl+q", "ctrl+s", "ctrl+l", "ctrl+p", "ctrl+t"]
    for key in required:
        assert key in KEYMAP, f"Missing binding: {key}"

def test_keymap_actions_are_strings():
    from app.tui.keymap import KEYMAP
    for key, action in KEYMAP.items():
        assert isinstance(action, str), f"{key} action is not a string"

def test_card_actions_exist():
    from app.tui.keymap import CARD_ACTIONS
    assert "r" in CARD_ACTIONS
    assert "s" in CARD_ACTIONS
    assert "y" in CARD_ACTIONS
    assert "n" in CARD_ACTIONS
    assert "a" in CARD_ACTIONS
    assert "e" in CARD_ACTIONS

def test_binding_descriptions_match_keymap():
    from app.tui.keymap import KEYMAP, BINDING_DESCRIPTIONS
    for key in BINDING_DESCRIPTIONS:
        assert key in KEYMAP, f"Description for '{{key}}' but no binding"
