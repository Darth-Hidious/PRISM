"""Code execution tool: run Python in a subprocess."""
import os
import subprocess
import sys
from pathlib import Path

from app.tools.base import Tool, ToolRegistry


MAX_TIMEOUT = 300  # Hard cap: 5 minutes regardless of agent request


def _execute_python(code: str, timeout: int = 60, description: str = "") -> dict:
    """Execute Python code in a subprocess. Returns stdout, stderr, exit code."""
    timeout = min(timeout, MAX_TIMEOUT)
    cwd = str(Path.cwd())
    try:
        result = subprocess.run(
            [sys.executable, "-c", code],
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=cwd,
            env={**os.environ},
            # Kill entire process group on timeout
            preexec_fn=os.setsid if sys.platform != "win32" else None,
        )
        return {
            "exit_code": result.returncode,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "success": result.returncode == 0,
            "description": description,
            "cwd": cwd,
            "timed_out": False,
        }
    except subprocess.TimeoutExpired as e:
        # subprocess.run already kills the child on timeout
        return {
            "error": f"Timed out after {timeout}s",
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
        },
        func=_execute_python,
        requires_approval=True,
    ))
