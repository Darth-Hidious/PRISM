"""Code execution tool: run Python in a subprocess."""
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

from app.tools.base import Tool, ToolRegistry


MAX_TIMEOUT = 300  # Hard cap: 5 minutes regardless of agent request

# How many full-traceback dump files to keep in ~/.prism/state/tracebacks/.
_TRACEBACK_KEEP = 32


def _traceback_dir() -> Path:
    """Directory for full-traceback dumps: ~/.prism/state/tracebacks/.

    The agent-facing stderr is FILTERED (see _filter_traceback) so the model
    sees the actionable header + final error line, not 50 numpy frames. The
    FULL raw traceback is written here so nothing is unrecoverable. Mirrors
    the ~/.prism/state/notebook convention (notebook.rs).
    """
    base = Path.home() / ".prism" / "state" / "tracebacks"
    base.mkdir(parents=True, exist_ok=True)
    return base


def _persist_full_traceback(raw: str) -> str:
    """Write the raw stderr to a timestamped file, prune to KEEP newest. Return path."""
    if not raw:
        return ""
    try:
        out_dir = _traceback_dir()
        path = out_dir / f"tb-{int(time.time() * 1000)}.txt"
        path.write_text(raw, encoding="utf-8", errors="replace")
        # Prune: keep only the newest _TRACEBACK_KEEP files by mtime.
        files = sorted(out_dir.glob("tb-*.txt"), key=lambda p: p.stat().st_mtime, reverse=True)
        for stale in files[_TRACEBACK_KEEP:]:
            try:
                stale.unlink()
            except OSError:
                pass
        return str(path)
    except OSError:
        # Traceback persistence is best-effort — never fail the tool because of it.
        return ""


# Markers that introduce a new "final exception block" in a chained traceback.
# Python emits one of these before each subsequent exception in a chain. When we
# see one, everything from that line to the end is load-bearing and must be kept
# (VS1/F2 lesson: never drop the final error line(s) — for a chain, keep ALL of them).
_CHAIN_MARKERS = (
    "During handling of the above exception, another exception occurred:",
    "The above exception was the direct cause of the following exception:",
)


def _is_user_frame(line: str, cwd: str) -> bool:
    """A frame line references user code if it names <string> or the cwd."""
    if not line.strip().startswith("File "):
        return False
    return ('File "<string>"' in line) or (cwd and cwd in line)


def _is_library_frame(line: str) -> bool:
    """A frame line references library code (site-packages / stdlib / venv)."""
    if not line.strip().startswith("File "):
        return False
    return any(
        marker in line
        for marker in ("site-packages", "dist-packages", "python3.", "/lib/python", "/Frameworks/")
    )


def _filter_traceback(raw_stderr: str, cwd: str = "") -> dict:
    """Filter a Python traceback for the AGENT-FACING stderr (VS2-P1a).

    Owner decision: the human debug pane keeps the FULL/RAW traceback; only the
    agent sees the filtered trace. For execute_python the agent-only stderr IS
    this filtered value (the tool result reaches the model, the model never
    sees the subprocess's raw stderr directly).

    Rules:
      - Keep the "Traceback (most recent call last):" header.
      - Keep every frame referencing <string> or the cwd (user code).
      - Collapse CONSECUTIVE library (site-packages / stdlib) frames to a single
        marker naming how many were elided and where the full trace lives.
      - ALWAYS keep the final exception line(s). For a chained exception
        ("During handling..." / "The above exception...") keep BOTH final
        blocks — the root cause lives in the earlier one.
      - NEVER put raw stderr in the returned dict. The full raw trace is
        written to ~/.prism/state/tracebacks/ and only its PATH is returned.

    Returns {stderr, traceback_elided_frames, stderr_full_path}. If there is
    nothing to filter (no traceback, or no library frames), stderr is returned
    verbatim with elided=0.
    """
    if not raw_stderr or not raw_stderr.strip():
        return {"stderr": raw_stderr, "traceback_elided_frames": 0, "stderr_full_path": ""}

    full_path = _persist_full_traceback(raw_stderr)
    lines = raw_stderr.splitlines()

    # If this isn't a traceback at all (e.g. a compiler SyntaxError preamble, a
    # warning, or plain stdout leak), don't filter — return verbatim with a
    # full-path pointer so it's still recoverable.
    has_tb_header = any("Traceback (most recent call last)" in ln for ln in lines)
    if not has_tb_header:
        return {
            "stderr": raw_stderr,
            "traceback_elided_frames": 0,
            "stderr_full_path": full_path,
        }

    # Split off the trailing "final exception block(s)" — everything from the
    # LAST non-traceback-frame chain marker onward is kept verbatim. We walk
    # from the end to find where the final exception line starts (the line that
    # is NOT a "File " frame and NOT a code-source line, typically "EType: msg").
    # Simpler & robust: keep everything from the first chain marker onward, plus
    # the final exception line at the very end.
    kept: list[str] = []
    run_of_library = 0
    # FIX-3: count FRAMES elided (sum of each run), not marker lines emitted.
    # The old code counted "[... N library frame(s) elided]" marker lines, so a
    # single collapsed run of 8 frames reported traceback_elided_frames=1.
    total_elided_frames = 0
    in_chain_tail = False

    def flush_library_run():
        """Emit a single collapse-marker for a run of consecutive library frames."""
        nonlocal run_of_library, total_elided_frames
        if run_of_library > 0:
            kept.append(
                f"[... {run_of_library} library frame(s) elided — full trace: {full_path}]"
            )
            total_elided_frames += run_of_library
            run_of_library = 0

    i = 0
    n = len(lines)
    while i < n:
        ln = lines[i]

        # Once we hit a chain marker, keep everything from here to the end —
        # chained-exception root causes are load-bearing (VS1/F2 lesson: for a
        # chain, keep BOTH final blocks).
        if any(marker in ln for marker in _CHAIN_MARKERS):
            flush_library_run()
            # Tail = this line + everything remaining, verbatim.
            kept.extend(lines[i:])
            break

        # A traceback frame is the `File "..."` line + ALL following indented
        # continuation lines (the source line AND, on Python 3.11+, the PEP 657
        # caret/annotation lines like `    ~~~~^~~`). Treat them as a unit so
        # eliding a frame removes its code+caret lines too (no orphaned carets —
        # FIX-3: the old code consumed only ONE indented line, so the caret fell
        # through and flushed the library run each iteration -> one marker per
        # frame + orphaned `^^^^`).
        if ln.lstrip().startswith("File "):
            # Grab ALL following indented continuation lines.
            continuation: list[str] = []
            consumed = 1
            while i + consumed < n and lines[i + consumed].startswith("    "):
                continuation.append(lines[i + consumed])
                consumed += 1

            # FIX-2: classify LIBRARY first (see comment above).
            if _is_library_frame(ln):
                run_of_library += 1
            else:
                # User frame (<string>/cwd) OR an unrecognized File frame — keep
                # it and ALL its continuation lines verbatim.
                flush_library_run()
                kept.append(ln)
                kept.extend(continuation)
            i += consumed
            continue

        # Header, code-source lines, or the final exception line. Flush a
        # pending library run, then keep this line.
        flush_library_run()
        kept.append(ln)
        i += 1

    # If the walk ended in a library run (no trailing exception line — rare),
    # flush it so the marker appears.
    flush_library_run()

    # FIX-3: report the FRAME count (total_elided_frames), not the marker-line
    # count. The old `sum(1 for ln in kept if "library frame(s) elided")` counted
    # MARKERS, so a single collapsed run of N frames reported 1.
    elided_frames = total_elided_frames
    # If we never collapsed anything, return verbatim (no marker pollution).
    if elided_frames == 0:
        return {
            "stderr": raw_stderr,
            "traceback_elided_frames": 0,
            "stderr_full_path": full_path,
        }
    filtered = "\n".join(kept)
    if raw_stderr.endswith("\n"):
        filtered += "\n"
    return {
        "stderr": filtered,
        "traceback_elided_frames": elided_frames,
        "stderr_full_path": full_path,
    }


def _signal_name(returncode: int) -> str:
    """Map a negative subprocess return code (killed-by-signal) to its name."""
    try:
        return signal.Signals(-returncode).name
    except (ValueError, TypeError):
        return f"signal {-returncode}"


def _child_env() -> dict:
    """Environment for the code subprocess.

    Two headless-safety defaults (caller env wins if already set):
      - MPLBACKEND=Agg — the child is headless; matplotlib's default macOS
        backend ('macosx') can crash on plot/savefig. Force non-interactive Agg.
      - PYTHONFAULTHANDLER=1 — if a native library (numpy/torch/BLAS, …) segfaults
        the child, dump a C-level traceback to stderr so the crash is diagnosable
        instead of a bare exit code.
    """
    env = {**os.environ}
    env.setdefault("MPLBACKEND", "Agg")
    env.setdefault("PYTHONFAULTHANDLER", "1")
    return env


def _execute_python(code: str, timeout: int = 60, description: str = "") -> dict:
    """Execute Python code in a subprocess. Returns stdout, stderr, exit code.

    Spawned via the default posix_spawn path — deliberately NO preexec_fn.
    A preexec_fn forces the fork() path, which is macOS-fragile from the
    multithreaded tool server and buys nothing here (subprocess.run's timeout
    only kills the direct child, never the process group). See #68.
    """
    timeout = min(timeout, MAX_TIMEOUT)
    cwd = str(Path.cwd())
    try:
        result = subprocess.run(
            [sys.executable, "-c", code],
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=cwd,
            env=_child_env(),
        )
        out = {
            "exit_code": result.returncode,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "success": result.returncode == 0,
            "description": description,
            "cwd": cwd,
            "timed_out": False,
        }
        # Negative return code = killed by signal (e.g. -11 = SIGSEGV). Surface
        # it as an actionable error instead of a bare code the model can't read.
        if result.returncode < 0:
            sig = _signal_name(result.returncode)
            out["error"] = (
                f"The code subprocess was killed by {sig}. This is almost always a "
                f"crash inside a native library (numpy/torch/BLAS, a GUI plotting "
                f"backend, etc.) in the executed code — not a PRISM failure. Check "
                f"stderr for a fault traceback (PYTHONFAULTHANDLER is on), isolate "
                f"the failing import/op, and retry. Plotting is headless "
                f"(MPLBACKEND=Agg) — use plt.savefig(), never plt.show()."
            )
        # VS2-P1a: filter the traceback for the agent-facing stderr. The RAW
        # stderr is persisted to ~/.prism/state/tracebacks/ and only its path
        # is returned — the raw trace never reaches the model. On a clean run
        # (no traceback) this is a no-op pass-through.
        filtered = _filter_traceback(result.stderr, cwd)
        out["stderr"] = filtered["stderr"]
        out["traceback_elided_frames"] = filtered["traceback_elided_frames"]
        if filtered["stderr_full_path"]:
            out["stderr_full_path"] = filtered["stderr_full_path"]
        return out
    except subprocess.TimeoutExpired as e:
        # subprocess.run already kills the child on timeout
        raw_stderr = (e.stderr or "").decode() if isinstance(e.stderr, bytes) else (e.stderr or "")
        filtered = _filter_traceback(raw_stderr, cwd)
        out = {
            "error": f"Timed out after {timeout}s",
            "success": False,
            "exit_code": 124,
            "stdout": (e.stdout or "").decode() if isinstance(e.stdout, bytes) else (e.stdout or ""),
            "stderr": filtered["stderr"],
            "traceback_elided_frames": filtered["traceback_elided_frames"],
            "description": description,
            "cwd": cwd,
            "timed_out": True,
        }
        if filtered["stderr_full_path"]:
            out["stderr_full_path"] = filtered["stderr_full_path"]
        return out
    except Exception as e:
        return {
            "error": str(e),
            "success": False,
            "description": description,
            "cwd": cwd,
            "timed_out": False,
        }


def create_code_tools(registry: ToolRegistry) -> None:
    """Register code execution tools."""
    # Models need the intended workflow stated plainly here: this tool is the
    # local Python workbench, while shell/process orchestration belongs in bash.
    registry.register(Tool(
        name="execute_python",
        description=(
            "Execute Python code for data analysis, transformation, plotting, "
            "quick calculations, and local inspection of files or datasets. "
            "The user's full Python environment is available (pandas, numpy, "
            "matplotlib, pymatgen, ASE, scikit-learn, pycalphad, etc.). "
            "Use this instead of execute_bash when the task is primarily Python "
            "logic rather than shell orchestration. Use print() to show output "
            "and write files explicitly inside the project when you need durable "
            "artifacts. Use plt.savefig('filename.png') to save plots. Code runs "
            "in a subprocess with the current PRISM Python interpreter."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": (
                        "Python code to execute verbatim. Include print() calls for "
                        "anything you want returned in stdout."
                    ),
                },
                "timeout": {
                    "type": "integer",
                    "description": f"Timeout in seconds (default 60, max {MAX_TIMEOUT}).",
                },
                "description": {
                    "type": "string",
                    "description": (
                        "Brief explanation of what the code does. Use this when "
                        "the code body is terse or not self-explanatory."
                    ),
                },
            },
            "required": ["code"],
            "additionalProperties": False,
        },
        func=_execute_python,
        requires_approval=True,
    ))
