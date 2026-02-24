"""Tests for TUI approval flow."""


def test_approval_card_renders_args():
    from app.tui.widgets.cards import ApprovalCard
    card = ApprovalCard(
        tool_name="calculate_phase_diagram",
        tool_args={"system": "W-Rh", "temperature": "300-2000K"},
    )
    assert card.tool_name == "calculate_phase_diagram"
    assert "W-Rh" in str(card.tool_args)


def test_approval_callback_exists():
    from app.tui.app import PrismApp
    app = PrismApp()
    assert hasattr(app, "_approval_callback")
    assert callable(app._approval_callback)


def test_resolve_approval():
    from app.tui.app import PrismApp
    app = PrismApp()
    app._resolve_approval(True)
    assert app._approval_result is True
    assert app._approval_event.is_set()
