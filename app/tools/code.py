"""Code execution tool: run Python in a subprocess."""
import os
import subprocess
import sys

from app.tools import spawn
from app.tools.base import Tool, ToolRegistry
from app.tools.spawn import signal_name as _signal_name


MAX_TIMEOUT = 300  # Hard cap: 5 minutes regardless of agent request


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

    Spawned through app.tools.spawn, which guarantees posix_spawn and never
    fork(). fork() is not survivable here: once a materials search has loaded
    Network.framework, forking SIGSEGVs in an atfork handler and the child dies
    blank. The child needs no process group of its own — subprocess.run's
    timeout only ever kills the direct child anyway.
    """
    timeout = min(timeout, MAX_TIMEOUT)
    cwd = os.getcwd()
    try:
        result = spawn.run(
            [sys.executable, "-c", code],
            capture_output=True,
            text=True,
            timeout=timeout,
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
                f"The code subprocess was killed by {sig}. Anything it had not "
                f"already flushed is lost, so empty stdout/stderr here means "
                f"output was destroyed, not that the code produced none. A signal "
                f"death is almost always a crash inside a native library "
                f"(numpy/torch/BLAS, a GUI plotting backend, …) in the executed "
                f"code. Check stderr for a fault traceback (PYTHONFAULTHANDLER is "
                f"on), isolate the failing import/op, and retry. Plotting is "
                f"headless (MPLBACKEND=Agg) — use plt.savefig(), never plt.show()."
            )
        return out
    except subprocess.TimeoutExpired as e:
        # subprocess.run already kills the child on timeout
        return {
            "error": f"Timed out after {timeout}s",
            "success": False,
            "exit_code": 124,
            "stdout": (e.stdout or "").decode() if isinstance(e.stdout, bytes) else (e.stdout or ""),
            "stderr": (e.stderr or "").decode() if isinstance(e.stderr, bytes) else (e.stderr or ""),
            "description": description,
            "cwd": cwd,
            "timed_out": True,
        }
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
