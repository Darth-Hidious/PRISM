"""Tests for the input bar widget."""


def test_input_bar_instantiates():
    from app.tui.widgets.input_bar import InputBar
    ib = InputBar()
    assert ib is not None


def test_input_bar_has_placeholder():
    from app.tui.widgets.input_bar import InputBar
    ib = InputBar()
    assert ib.placeholder != ""


def test_input_bar_placeholder_text():
    from app.tui.widgets.input_bar import InputBar
    ib = InputBar()
    assert "PRISM" in ib.placeholder
