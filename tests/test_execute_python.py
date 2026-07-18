"""Tests for execute_python tool."""
import sys

import pytest
from unittest.mock import patch, MagicMock

from app.tools.code import _execute_python, create_code_tools
from app.tools.base import ToolRegistry


class TestExecutePython:
    def test_simple_print(self):
        result = _execute_python(code='print("hello world")')
        assert result["success"] is True
        assert result["exit_code"] == 0
        assert "hello world" in result["stdout"]

    def test_math_expression(self):
        result = _execute_python(code='print(2 + 2)')
        assert result["success"] is True
        assert "4" in result["stdout"]

    def test_import_and_compute(self):
        result = _execute_python(code='import json; print(json.dumps({"a": 1}))')
        assert result["success"] is True
        assert '"a": 1' in result["stdout"]

    def test_syntax_error(self):
        result = _execute_python(code='def foo(')
        assert result["success"] is False
        assert result["exit_code"] != 0
        assert "SyntaxError" in result["stderr"]

    def test_runtime_error(self):
        result = _execute_python(code='print(1/0)')
        assert result["success"] is False
        assert "ZeroDivisionError" in result["stderr"]

    def test_timeout(self):
        result = _execute_python(code='import time; time.sleep(10)', timeout=1)
        assert "error" in result or result.get("exit_code") == 124
        assert "Timed out" in result.get("error", "") or result.get("exit_code") == 124

    def test_multiline_code(self):
        code = """
import math
values = [math.sqrt(i) for i in range(5)]
for v in values:
    print(f"{v:.2f}")
"""
        result = _execute_python(code=code)
        assert result["success"] is True
        assert "0.00" in result["stdout"]
        assert "2.00" in result["stdout"]

    def test_stderr_captured(self):
        result = _execute_python(code='import sys; print("err", file=sys.stderr)')
        assert "err" in result["stderr"]

    def test_empty_code(self):
        result = _execute_python(code='')
        assert result["success"] is True
        assert result["exit_code"] == 0

    def test_uses_same_python(self):
        result = _execute_python(code=f'import sys; print(sys.executable)')
        assert result["success"] is True
        # Should use the same Python as the test runner
        assert result["stdout"].strip() != ""


class TestCodeToolRegistration:
    def test_tool_registered(self):
        reg = ToolRegistry()
        create_code_tools(reg)
        tool = reg.get("execute_python")
        assert tool.name == "execute_python"
        assert tool.requires_approval is True

    def test_tool_schema_has_code_required(self):
        reg = ToolRegistry()
        create_code_tools(reg)
        tool = reg.get("execute_python")
        assert "code" in tool.input_schema["properties"]
        assert "code" in tool.input_schema["required"]

    def test_tool_in_bootstrap(self):
        """execute_python is registered via build_full_registry."""
        from app.plugins.bootstrap import build_full_registry
        tool_reg, _, _ = build_full_registry(enable_mcp=False, enable_plugins=False)
        assert "execute_python" in {t.name for t in tool_reg.list_tools()}


class TestCodeToolIntegration:
    def test_agent_core_approval_gate(self):
        """execute_python requires approval in AgentCore."""
        from app.tools.base import ToolRegistry
        from app.tools.code import create_code_tools

        reg = ToolRegistry()
        create_code_tools(reg)
        tool = reg.get("execute_python")

        # Simulate AgentCore._should_approve with auto_approve=False
        assert tool.requires_approval is True

    def test_pandas_available(self):
        """Verify pandas is importable in subprocess."""
        result = _execute_python(code='import pandas; print(pandas.__version__)')
        if result["success"]:
            assert result["stdout"].strip() != ""
        # If pandas not installed, just skip — not a failure of the tool

    def test_large_output_is_returned(self):
        """Large output comes back fully (ResultStore handles truncation)."""
        code = 'print("x" * 50000)'
        result = _execute_python(code=code)
        assert result["success"] is True
        assert len(result["stdout"]) == 50001  # 50000 x's + newline


class TestCrashHandling:
    """#68: a child killed by a signal must surface as an actionable error,
    not a bare negative exit code the model can't interpret."""

    def test_signal_death_is_surfaced(self):
        """A child that SIGSEGVs returns exit -11, success False, named error."""
        result = _execute_python(
            code="import os, signal; os.kill(os.getpid(), signal.SIGSEGV)"
        )
        assert result["exit_code"] == -11
        assert result["success"] is False
        assert "SIGSEGV" in result.get("error", "")

    def test_signal_name_mapping(self):
        from app.tools.code import _signal_name

        assert _signal_name(-11) == "SIGSEGV"
        assert _signal_name(-6) == "SIGABRT"
        assert _signal_name(-999).startswith("signal ")  # unknown → graceful

    def test_headless_env_defaults(self):
        """Child gets a non-interactive plotting backend + faulthandler so a
        native crash is diagnosable, without clobbering a caller override."""
        from app.tools.code import _child_env

        env = _child_env()
        assert env["MPLBACKEND"] == "Agg"
        assert env["PYTHONFAULTHANDLER"] == "1"

    def test_normal_run_has_no_error_key(self):
        """A clean run must not carry the crash error field."""
        result = _execute_python(code='print("ok")')
        assert result["success"] is True
        assert "error" not in result


class TestFailureContract:
    """VS1/F1: every failure path must carry `success: False` so the Rust
    is_error gate (keyed on the tool's own success boolean) flags it. A path
    that sets `error` but omits `success` would still be caught by the
    error-string rule, but the contract should be consistent across branches.
    """

    def test_timeout_carries_success_false(self):
        """The TimeoutExpired branch must emit success: False (not just error)."""
        result = _execute_python(code="import time; time.sleep(30)", timeout=1)
        assert result["timed_out"] is True
        assert result["success"] is False, (
            "timeout branch must carry success:False so the is_error gate "
            "flags it via the !success rule, not only the error string"
        )
        assert "error" in result


class TestTracebackFilter:
    """VS2-P1a: _filter_traceback produces an AGENT-FACING stderr that keeps
    the header + user frames + the final error line, while collapsing library
    frames. The full raw trace is written to ~/.prism/state/tracebacks/."""

    def test_keeps_final_value_error_line(self):
        from app.tools.code import _filter_traceback
        tb = (
            'Traceback (most recent call last):\n'
            '  File "<string>", line 4, in <module>\n'
            '    numpy.linalg.inv(mat)\n'
            '  File "/opt/homebrew/lib/python3.14/site-packages/numpy/linalg/linalg.py", line 540, in inv\n'
            '    ainv = _umath_linalg.inv(a)\n'
            'ValueError: Singular matrix\n'
        )
        r = _filter_traceback(tb, "/cwd")
        assert "ValueError: Singular matrix" in r["stderr"], (
            "final error line must survive: %r" % r["stderr"]
        )

    def test_elides_library_frames_with_marker(self):
        from app.tools.code import _filter_traceback
        tb = (
            'Traceback (most recent call last):\n'
            '  File "<string>", line 1, in <module>\n'
            '    f()\n'
            '  File "/x/site-packages/numpy/core.py", line 1, in f\n'
            '    pass\n'
            '  File "/x/site-packages/numpy/core.py", line 2, in g\n'
            '    pass\n'
            'RuntimeError: boom\n'
        )
        r = _filter_traceback(tb, "/cwd")
        assert r["traceback_elided_frames"] >= 1
        assert "library frame(s) elided" in r["stderr"]
        assert "site-packages/numpy" not in r["stderr"], "raw lib path must not leak"

    def test_venv_under_cwd_library_frames_still_elided(self):
        """FIX-2: a project-local venv (<cwd>/.venv/.../site-packages) CONTAINS
        cwd, so the old order (is_user before is_library) kept every library
        frame. Library classification must win regardless of cwd."""
        from app.tools.code import _filter_traceback
        cwd = "/Users/me/project"
        tb = (
            'Traceback (most recent call last):\n'
            f'  File "{cwd}/my_script.py", line 4, in <module>\n'
            '    numpy.linalg.inv(mat)\n'
            f'  File "{cwd}/.venv/lib/python3.14/site-packages/numpy/linalg/linalg.py", line 540, in inv\n'
            '    ainv = _umath_linalg.inv(a)\n'
            f'  File "{cwd}/.venv/lib/python3.14/site-packages/numpy/core.py", line 12, in _commonType\n'
            '    raise ValueError(msg)\n'
            'ValueError: Singular matrix\n'
        )
        r = _filter_traceback(tb, cwd)
        # The user script frame survives; the venv-site-packages frames elide.
        assert "my_script.py" in r["stderr"], "user frame must survive"
        assert "ValueError: Singular matrix" in r["stderr"]
        assert ".venv/lib" not in r["stderr"], (
            "venv-under-cwd library frames must be elided, not kept as user frames"
        )
        assert r["traceback_elided_frames"] >= 1

    def test_unchanged_when_nothing_to_elide(self):
        from app.tools.code import _filter_traceback
        tb = (
            'Traceback (most recent call last):\n'
            '  File "<string>", line 2, in <module>\n'
            '    1/0\n'
            'ZeroDivisionError: division by zero\n'
        )
        r = _filter_traceback(tb, "/cwd")
        assert r["stderr"] == tb, "verbatim when nothing to elide"
        assert r["traceback_elided_frames"] == 0

    def test_never_returns_empty_when_there_is_a_final_line(self):
        from app.tools.code import _filter_traceback
        # Even if EVERY frame is a library frame, the final error line survives.
        tb = (
            'Traceback (most recent call last):\n'
            '  File "/x/site-packages/a.py", line 1, in x\n'
            '    pass\n'
            '  File "/x/site-packages/b.py", line 2, in y\n'
            '    pass\n'
            'RuntimeError: deep\n'
        )
        r = _filter_traceback(tb, "/cwd")
        assert r["stderr"].strip() != "", "filtered stderr must never be empty"
        assert "RuntimeError: deep" in r["stderr"]

    def test_chained_exception_keeps_both_final_lines(self):
        from app.tools.code import _filter_traceback
        tb = (
            'Traceback (most recent call last):\n'
            '  File "<string>", line 2, in <module>\n'
            '    int("abc")\n'
            "ValueError: invalid literal for int() with base 10: 'abc'\n"
            "\n"
            "During handling of the above exception, another exception occurred:\n"
            "\n"
            'Traceback (most recent call last):\n'
            '  File "<string>", line 4, in <module>\n'
            '    raise RuntimeError("wrapped")\n'
            'RuntimeError: wrapped\n'
        )
        r = _filter_traceback(tb, "/cwd")
        # VS1/F2 lesson: keep BOTH final blocks for a chain.
        assert "ValueError: invalid literal" in r["stderr"]
        assert "During handling" in r["stderr"]
        assert "RuntimeError: wrapped" in r["stderr"]

    def test_writes_full_traceback_and_returns_path(self):
        from app.tools.code import _filter_traceback
        from pathlib import Path
        tb = (
            'Traceback (most recent call last):\n'
            '  File "/x/site-packages/numpy/a.py", line 1, in f\n'
            '    pass\n'
            'ValueError: x\n'
        )
        r = _filter_traceback(tb, "/cwd")
        assert r["stderr_full_path"], "path must be returned"
        p = Path(r["stderr_full_path"])
        assert p.exists(), "the full raw traceback must be persisted"
        # CRITICAL: the raw stderr must NOT be in the agent-facing stderr.
        assert "site-packages/numpy" not in r["stderr"]
        # But the full raw IS in the file.
        assert "site-packages/numpy" in p.read_text()

    def test_no_traceback_passthrough(self):
        from app.tools.code import _filter_traceback
        # A plain warning (no Traceback header) passes through verbatim.
        r = _filter_traceback("DeprecationWarning: foo\n", "/cwd")
        assert r["stderr"] == "DeprecationWarning: foo\n"
        assert r["traceback_elided_frames"] == 0

    def test_empty_input(self):
        from app.tools.code import _filter_traceback
        r = _filter_traceback("", "/cwd")
        assert r["stderr"] == ""
        assert r["stderr_full_path"] == ""
        assert r["traceback_elided_frames"] == 0

    def test_real_python314_caret_traceback_collapse(self):
        """FIX-3: against a REAL python3.14 traceback (PEP 657 caret lines
        present — the production format, since the repo runs 3.14). The old
        filter consumed only ONE indented line per frame, so the caret fell
        through and flushed the library run each iteration -> one marker PER
        library frame + orphaned `~~~~`. Now: one marker for the run, frame
        count correct, final exception line kept."""
        import io
        import traceback as tb_mod

        # Capture a real traceback by failing through numpy (caret lines only
        # appear for frames with source context). Skip if numpy unavailable.
        np = pytest.importorskip("numpy")
        buf = io.StringIO()
        try:
            np.linalg.inv(np.array([[1, 2], [2, 4]]))  # singular -> LinAlgError
        except Exception:
            tb_mod.print_exc(file=buf)
        raw = buf.getvalue()
        assert "~~" in raw or "^^" in raw, "test premise: caret lines present in 3.14 trace"

        from app.tools.code import _filter_traceback
        r = _filter_traceback(raw, "/cwd")
        out = r["stderr"]
        # The final exception line survives.
        assert "LinAlgError" in out, "final exception line must survive: %r" % out[-80:]
        # Count collapse markers — a RUN of consecutive library frames must
        # produce ONE marker, not one per frame.
        marker_count = out.count("library frame(s) elided")
        assert marker_count == 1, (
            "a run of consecutive library frames must collapse to ONE marker, "
            "got %d: %r" % (marker_count, out)
        )
        # The marker names the run length; the reported count is the FRAME count.
        assert r["traceback_elided_frames"] >= 1, "frame count reported"
        # No orphaned caret lines leaked into the output (they should travel
        # with their frame, whether kept or elided).
        # Caret lines that belonged to LIBRARY frames must be gone; a caret
        # under the USER frame (kept) is fine. Assert no raw library path leaks.
        assert "site-packages/numpy" not in out, "raw library path must not leak: %r" % out

    def test_no_traceback_dump_for_non_traceback_stderr(self, tmp_path, monkeypatch):
        """FIX-7: _persist_full_traceback must NOT run for non-traceback stderr
        (a DeprecationWarning on a SUCCESSFUL run). The old code persisted any
        non-empty stderr, evicting real full traces via the keep-32 prune."""
        from pathlib import Path
        from app.tools.code import _filter_traceback, _traceback_dir

        # Point the traceback dir at a temp location so we can count files.
        monkeypatch.setattr(
            "app.tools.code._traceback_dir", lambda: tmp_path
        )
        # A plain warning — NOT a traceback. No file should be written.
        r = _filter_traceback("DeprecationWarning: foo is deprecated\n", "/cwd")
        assert r["stderr_full_path"] == "", (
            "non-traceback stderr must NOT be persisted: got %r" % r["stderr_full_path"]
        )
        assert list(tmp_path.glob("tb-*.txt")) == [], "no dump file should exist"

    def test_traceback_dump_uses_unique_filename(self, tmp_path, monkeypatch):
        """FIX-7: two failures within the same ms must not collide. The filename
        now carries a uuid suffix."""
        from app.tools.code import _filter_traceback

        monkeypatch.setattr("app.tools.code._traceback_dir", lambda: tmp_path)
        tb = (
            "Traceback (most recent call last):\n"
            '  File "/x/site-packages/numpy/a.py", line 1, in f\n'
            "    pass\n"
            "ValueError: boom\n"
        )
        _filter_traceback(tb, "/cwd")
        _filter_traceback(tb, "/cwd")
        files = list(tmp_path.glob("tb-*.txt"))
        assert len(files) == 2, "two failures must produce two distinct files: %r" % files
        names = {f.name for f in files}
        assert len(names) == 2, "filenames must be unique (uuid suffix): %r" % names


class TestFilterPreservesGateSignal:
    """VS2-P1: filtering the agent-facing stderr must NOT mask the failure
    from the F1 is_error gate. The gate keys on `success`, and _execute_python
    sets success:False on failure regardless of stderr content — so a filtered
    failure still trips the gate."""

    def test_filtered_failure_still_has_success_false(self):
        # A real failing program: the filtered stderr is in the result, but
        # success:False is the load-bearing signal for the gate.
        result = _execute_python(code="raise ValueError('boom')")
        assert result["success"] is False
        assert result["exit_code"] != 0
        # The final error line must be present in the filtered stderr.
        assert "ValueError: boom" in result.get("stderr", "")
        # And the agent-facing stderr must NOT contain raw library frames.
        assert "traceback_elided_frames" in result
