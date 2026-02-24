"""Tests for REPL card rendering and result type detection."""
from unittest.mock import MagicMock, patch
from io import StringIO


def test_detect_result_type_error():
    from app.agent.repl import _detect_result_type
    assert _detect_result_type({"error": "something broke"}) == "error"


def test_detect_result_type_metrics():
    from app.agent.repl import _detect_result_type
    assert _detect_result_type({"metrics": {"mae": 0.04}, "algorithm": "rf"}) == "metrics"


def test_detect_result_type_calphad_phases_present():
    from app.agent.repl import _detect_result_type
    assert _detect_result_type({"phases_present": ["BCC"]}) == "calphad"


def test_detect_result_type_calphad_gibbs():
    from app.agent.repl import _detect_result_type
    assert _detect_result_type({"phases": {"BCC": 0.5}, "gibbs_energy": -1234.5}) == "calphad"


def test_detect_result_type_validation():
    from app.agent.repl import _detect_result_type
    assert _detect_result_type({"findings": [], "quality_score": 0.9}) == "validation"


def test_detect_result_type_plot():
    from app.agent.repl import _detect_result_type
    assert _detect_result_type({"filename": "scatter.png"}) == "plot"


def test_detect_result_type_results():
    from app.agent.repl import _detect_result_type
    assert _detect_result_type({"results": [{"a": 1}] * 5, "count": 5}) == "results"


def test_detect_result_type_default():
    from app.agent.repl import _detect_result_type
    assert _detect_result_type({"success": True}) == "tool"


def test_format_elapsed_seconds():
    from app.agent.repl import AgentREPL
    repl = MagicMock(spec=AgentREPL)
    assert AgentREPL._format_elapsed(repl, 5000) == "5.0s"


def test_format_elapsed_milliseconds():
    from app.agent.repl import AgentREPL
    repl = MagicMock(spec=AgentREPL)
    assert AgentREPL._format_elapsed(repl, 450) == "450ms"


def test_format_elapsed_zero():
    from app.agent.repl import AgentREPL
    repl = MagicMock(spec=AgentREPL)
    assert AgentREPL._format_elapsed(repl, 0) == ""


def test_spinner_uses_rich_status():
    from app.agent.spinner import Spinner
    from rich.console import Console
    console = Console(file=StringIO())
    s = Spinner(console)
    s.start("Testing...")
    assert s._status is not None
    s.stop()
    assert s._status is None


def test_spinner_update():
    from app.agent.spinner import Spinner
    from rich.console import Console
    console = Console(file=StringIO())
    s = Spinner(console)
    s.start("First...")
    s.update("Second...")
    s.stop()


def test_spinner_double_stop():
    from app.agent.spinner import Spinner
    from rich.console import Console
    console = Console(file=StringIO())
    s = Spinner(console)
    s.start("Test...")
    s.stop()
    s.stop()  # Should not raise


def test_mascot_lines():
    from app.agent.repl import _MASCOT
    assert len(_MASCOT) == 4
    # Check hex characters are present
    all_text = "".join(_MASCOT)
    assert "\u2b22" in all_text  # filled hex
    assert "\u2b21" in all_text  # empty hex


def test_repl_commands_dict():
    from app.agent.repl import REPL_COMMANDS
    assert "/help" in REPL_COMMANDS
    assert "/exit" in REPL_COMMANDS
    assert "/tools" in REPL_COMMANDS
    assert "/approve-all" in REPL_COMMANDS
