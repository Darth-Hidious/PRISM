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
    try:
        result = subprocess.run(
            [sys.executable, "-c", code],
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=str(Path.cwd()),
            env={**os.environ},
            # Kill entire process group on timeout
            preexec_fn=os.setsid if sys.platform != "win32" else None,
        )
        return {
            "exit_code": result.returncode,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "success": result.returncode == 0,
        }
    except subprocess.TimeoutExpired as e:
        # subprocess.run already kills the child on timeout
        return {
            "error": f"Timed out after {timeout}s",
            "exit_code": 124,
            "stdout": (e.stdout or "").decode() if isinstance(e.stdout, bytes) else (e.stdout or ""),
            "stderr": (e.stderr or "").decode() if isinstance(e.stderr, bytes) else (e.stderr or ""),
        }
    except Exception as e:
        return {"error": str(e)}


def create_code_tools(registry: ToolRegistry) -> None:
    """Register code execution tools."""
    registry.register(Tool(
        name="execute_python",
        description=(
            "Execute Python code for data analysis, transformation, and plotting. "
            "The user's full Python environment is available (pandas, numpy, "
            "matplotlib, pymatgen, ASE, scikit-learn, pycalphad, etc.). "
            "Use print() to show output. Use plt.savefig('filename.png') to save plots. "
            "Code runs in a subprocess — safe to import any installed package."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "Python code to execute",
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default 60)",
                },
                "description": {
                    "type": "string",
                    "description": "Brief description of what the code does",
                },
            },
            "required": ["code"],
        },
        func=_execute_python,
        requires_approval=True,
    ))
