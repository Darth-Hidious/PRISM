# Data Command & Code Execution — Design Document

**Date:** 2026-02-25
**Status:** Approved
**Scope:** `app/tools/code.py`, `app/commands/data.py`, system prompts

## Problem

PRISM's agent can find data (search_materials, query_materials_project) and export it
(export_results_csv), but cannot transform, filter, analyze, or plot data. A researcher
who says "remove outliers from band_gap, normalize formation_energy, plot the correlation"
has to leave PRISM to do it.

Additionally, `prism data collect` uses the old `OPTIMADECollector` directly instead of
the newer federated `SearchEngine` that powers `prism search` and `prism run`.

## Architecture Decision

**Code execution: Approach B — Subprocess-isolated with approval gate**

Rejected alternatives:
- **A: Import blocklist** — too restrictive, researchers need pymatgen/ASE/pycalphad,
  blocklists are always incomplete.
- **C: No restrictions** — the approval gate is cheap and prevents accidents.

Python-only. Materials science runs on Python (pandas, numpy, pymatgen, ASE, pycalphad,
scikit-learn). No value in supporting other languages.

---

## Section 1: `execute_python` Tool

### File: `app/tools/code.py` (NEW)

```python
import os
import sys
import subprocess
from pathlib import Path

def _execute_python(code: str, timeout: int = 60, description: str = "") -> dict:
    try:
        result = subprocess.run(
            [sys.executable, "-c", code],
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=str(Path.cwd()),
            env={**os.environ},
        )
        return {
            "exit_code": result.returncode,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "success": result.returncode == 0,
        }
    except subprocess.TimeoutExpired:
        return {"error": f"Timed out after {timeout}s", "exit_code": 124}
    except Exception as e:
        return {"error": str(e)}
```

### Tool registration

```python
Tool(
    name="execute_python",
    description="Execute Python code for data analysis. User's full Python environment "
                "is available (pandas, numpy, matplotlib, pymatgen, etc.). Use print() "
                "to show results. Use plt.savefig() to save plots.",
    input_schema={
        "type": "object",
        "properties": {
            "code": {"type": "string", "description": "Python code to execute"},
            "timeout": {"type": "integer", "description": "Timeout in seconds (default 60)"},
            "description": {"type": "string", "description": "Brief description of what the code does"},
        },
        "required": ["code"],
    },
    func=_execute_python,
    requires_approval=True,
)
```

### Safety model

- `requires_approval = True` — user sees code before it runs
- `--dangerously-accept-all` auto-approves (existing flag)
- Subprocess isolation — agent can't corrupt its own process
- Timeout with process kill — prevents infinite loops
- stdout/stderr captured and returned as structured dict
- Large outputs handled by existing ResultStore (>30K chars → peek_result)
- Doom loop detection catches repeated failing code

---

## Section 2: `prism data collect` Upgrade

### Current: uses `OPTIMADECollector` directly
### New: uses `SearchEngine` (same as `prism search`)

```python
# Before
from app.data.collector import OPTIMADECollector
collector = OPTIMADECollector()
records = collector.collect(filter_string=filter_string, ...)
df = normalize_records(records)

# After
from app.search import SearchEngine, MaterialSearchQuery
engine = SearchEngine()
query = MaterialSearchQuery(elements=elems, formula=formula, ...)
results = asyncio.run(engine.search(query))
df = materials_to_dataframe(results)
```

New helper `materials_to_dataframe(results)` converts `list[Material]` to pandas
DataFrame, preserving sources for provider tracking.

Same CLI options: `--elements`, `--formula`, `--providers`, `--limit`, `--name`.
Same output: saved to DataStore (Parquet + JSON metadata).

---

## Section 3: System Prompt Updates

Add to both `DEFAULT_SYSTEM_PROMPT` (core.py) and `AUTONOMOUS_SYSTEM_PROMPT` (autonomous.py):

```
You can execute Python code for data analysis using the execute_python tool.
The user's full Python environment is available (pandas, numpy, matplotlib,
pymatgen, ASE, scikit-learn, etc.). Use this for data manipulation, filtering,
plotting, and custom calculations. Use print() to show output. Use
plt.savefig("filename.png") to save plots.
```

---

## Files Touched

| File | Action |
|------|--------|
| `app/tools/code.py` | CREATE — execute_python tool |
| `app/commands/data.py` | MODIFY — rewire collect to SearchEngine |
| `app/agent/core.py` | MODIFY — add execute_python to system prompt |
| `app/agent/autonomous.py` | MODIFY — same system prompt addition |
| `app/plugins/bootstrap.py` | MODIFY — register code tools |
| `tests/test_execute_python.py` | CREATE — tests for code execution |
| `tests/test_data_collect.py` | CREATE — tests for upgraded collect |
| `docs/data.md` | CREATE — documentation |
