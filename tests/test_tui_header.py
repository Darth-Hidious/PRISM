"""Tests for the header widget."""

def test_header_widget_renders():
    from app.tui.widgets.header import HeaderWidget
    widget = HeaderWidget()
    assert widget is not None

def test_header_contains_mascot_chars():
    from app.tui.widgets.header import MASCOT_LINES
    combined = "".join(MASCOT_LINES)
    assert "⬢" in combined
    assert "⬡" in combined

def test_header_contains_commands():
    from app.tui.widgets.header import HEADER_COMMANDS
    assert "/help" in HEADER_COMMANDS
    assert "/tools" in HEADER_COMMANDS
    assert "/skills" in HEADER_COMMANDS
    assert "/scratchpad" in HEADER_COMMANDS

def test_build_header_text():
    from app.tui.widgets.header import build_header_text
    text = build_header_text(provider="Claude", ml_ready=True, calphad_ready=False)
    plain = text.plain
    assert "MARC27" in plain
    assert "Claude" in plain

def test_build_mascot_line():
    from app.tui.widgets.header import build_mascot_line
    line = build_mascot_line(1)
    assert "⬢" in line.plain
    assert "⬡" in line.plain

def test_build_rainbow_bar():
    from app.tui.widgets.header import build_rainbow_bar
    bar = build_rainbow_bar(10)
    assert len(bar.plain) == 10
