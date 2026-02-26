"""Tests for REPL card rendering and result type detection."""
from unittest.mock import MagicMock, patch
from io import StringIO


def test_detect_result_type_error():
    from app.cli.tui.cards import detect_result_type
    assert detect_result_type({"error": "something broke"}) == "error"


def test_detect_result_type_metrics():
    from app.cli.tui.cards import detect_result_type
    assert detect_result_type({"metrics": {"mae": 0.04}, "algorithm": "rf"}) == "metrics"


def test_detect_result_type_calphad_phases_present():
    from app.cli.tui.cards import detect_result_type
    assert detect_result_type({"phases_present": ["BCC"]}) == "calphad"


def test_detect_result_type_calphad_gibbs():
    from app.cli.tui.cards import detect_result_type
    assert detect_result_type({"phases": {"BCC": 0.5}, "gibbs_energy": -1234.5}) == "calphad"


def test_detect_result_type_validation():
    from app.cli.tui.cards import detect_result_type
    assert detect_result_type({"findings": [], "quality_score": 0.9}) == "validation"


def test_detect_result_type_plot():
    from app.cli.tui.cards import detect_result_type
    assert detect_result_type({"filename": "scatter.png"}) == "plot"


def test_detect_result_type_results():
    from app.cli.tui.cards import detect_result_type
    assert detect_result_type({"results": [{"a": 1}] * 5, "count": 5}) == "results"


def test_detect_result_type_default():
    from app.cli.tui.cards import detect_result_type
    assert detect_result_type({"success": True}) == "tool"


def test_format_elapsed_seconds():
    from app.cli.tui.cards import format_elapsed
    assert format_elapsed(5000) == "5.0s"


def test_format_elapsed_milliseconds():
    from app.cli.tui.cards import format_elapsed
    assert format_elapsed(450) == "450ms"


def test_format_elapsed_zero():
    from app.cli.tui.cards import format_elapsed
    assert format_elapsed(0) == ""


def test_spinner_uses_rich_status():
    from app.cli.tui.spinner import Spinner
    from rich.console import Console
    console = Console(file=StringIO())
    s = Spinner(console)
    s.start("Testing...")
    assert s._status is not None
    s.stop()
    assert s._status is None


def test_spinner_update():
    from app.cli.tui.spinner import Spinner
    from rich.console import Console
    console = Console(file=StringIO())
    s = Spinner(console)
    s.start("First...")
    s.update("Second...")
    s.stop()


def test_spinner_double_stop():
    from app.cli.tui.spinner import Spinner
    from rich.console import Console
    console = Console(file=StringIO())
    s = Spinner(console)
    s.start("Test...")
    s.stop()
    s.stop()  # Should not raise


def test_mascot_lines():
    from app.cli.tui.theme import MASCOT
    assert len(MASCOT) == 4
    all_text = "".join(MASCOT)
    assert "\u2b22" in all_text  # filled hex
    assert "\u2b21" in all_text  # empty hex


def test_repl_commands_dict():
    from app.cli.slash.registry import REPL_COMMANDS
    assert "/help" in REPL_COMMANDS
    assert "/exit" in REPL_COMMANDS
    assert "/tools" in REPL_COMMANDS
    assert "/approve-all" in REPL_COMMANDS


def test_input_card_renders_cyan_panel():
    from app.cli.tui.cards import render_input_card
    from rich.console import Console
    output = StringIO()
    console = Console(file=output, highlight=False, force_terminal=True)
    render_input_card(console, "hello world")
    text = output.getvalue()
    assert "hello world" in text
    assert "\u256d" in text or "\u2570" in text


def test_output_card_renders_dim_panel():
    from app.cli.tui.cards import render_output_card
    from rich.console import Console
    output = StringIO()
    console = Console(file=output, highlight=False, force_terminal=True)
    render_output_card(console, "short text")
    text = output.getvalue()
    assert "short text" in text
    assert "\u256d" in text or "\u2570" in text


def test_output_card_truncates_long_text():
    from app.cli.tui.cards import render_output_card
    from app.cli.tui.theme import TRUNCATION_LINES
    from rich.console import Console
    output = StringIO()
    console = Console(file=output, highlight=False, force_terminal=True)
    long_text = "\n".join(f"Line {i}" for i in range(20))
    render_output_card(console, long_text)
    text = output.getvalue()
    assert "more lines" in text


def test_crystal_3tier_colors():
    from app.cli.tui.theme import CRYSTAL_OUTER_DIM, CRYSTAL_OUTER, CRYSTAL_INNER, CRYSTAL_CORE
    assert CRYSTAL_OUTER_DIM == "#555577"
    assert CRYSTAL_OUTER == "#7777aa"
    assert CRYSTAL_INNER == "#ccccff"
    assert CRYSTAL_CORE == "#ffffff"


def test_borders_has_input_and_output():
    from app.cli.tui.theme import BORDERS
    assert "input" in BORDERS
    assert "output" in BORDERS


def test_header_commands_defined():
    from app.cli.tui.theme import HEADER_COMMANDS_L, HEADER_COMMANDS_R
    assert "/help" in HEADER_COMMANDS_L
    assert "/scratchpad" in HEADER_COMMANDS_R


def test_render_input_card_still_available():
    """render_input_card works as standalone function."""
    from app.cli.tui.cards import render_input_card
    from rich.console import Console
    output = StringIO()
    console = Console(file=output, highlight=False, force_terminal=True, width=80)
    render_input_card(console, "test message")
    text = output.getvalue()
    assert "test message" in text


def test_backward_compat_shim_imports():
    """Old import paths still work via shims."""
    from app.agent.repl import AgentREPL, _detect_result_type, _BORDERS, _MASCOT
    from app.agent.commands import REPL_COMMANDS, COMMAND_ALIASES, CLI_FLAGS
    from app.agent.spinner import Spinner
    assert AgentREPL is not None
    assert callable(_detect_result_type)
    assert "/help" in REPL_COMMANDS
    assert Spinner is not None


def test_crystal_mascot_alignment():
    """Top/bottom rows should be centered under middle rows."""
    from app.cli.tui.theme import MASCOT
    for i in [0, 3]:
        leading_spaces = len(MASCOT[i]) - len(MASCOT[i].lstrip())
        assert leading_spaces == 4, f"Row {i} has {leading_spaces} leading spaces, expected 4"
