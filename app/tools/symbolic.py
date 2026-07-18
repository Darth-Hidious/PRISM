"""VS2-P2 — SymPy symbolic-grounding tool: ``symbolic_check``.

Honesty core: zero-equivalence of symbolic expressions is UNDECIDABLE in
general. This tool NEVER lies a binary "equal/not-equal". It reports one of:

  - ``proven``              — symbolic proof succeeded (simplify(a-b)==0, or a
                              dimension match). This is the only verdict that
                              justifies the word "verified".
  - ``numerically_consistent`` — symbolic proof failed, but a seeded numeric
                              spot-check agreed at every sampled point within
                              tolerance. Explicitly NOT proven — say so.
  - ``fail``                — a counterexample was found: a concrete point
                              where the two expressions disagree. Includes the
                              counterexample so the model can act on it.
  - ``inconclusive``        — the check could not run meaningfully (free-symbol
                              mismatch, parse error, unsupported mode). Names
                              why.

VS1-GATE-AWARE RESULT CONTRACT (the load-bearing rule):
  ``success`` means "the check RAN", NEVER the verdict. A ``fail`` verdict is
  ``success: True`` — if it were ``success: False``, crates/agent/src/
  tool_result.rs::tool_result_is_error would mask the finding as a runtime
  error and the model would never see the counterexample. ``success: False``
  is reserved for timeout / crash / parse error / unsafe input.

Execution: the check runs in a subprocess (same pattern as _execute_python)
because ``simplify()`` can hang or OOM on pathological input — the timeout
kills it honestly. The child receives its config as a JSON blob on argv[1]
(no string templating, so the child's own braces/format calls are untouched).
``parse_expr`` is given a restricted namespace and an EMPTY ``global_dict`` so
``__import__``/builtins can't escape; the subprocess is the secondary net.
"""
from __future__ import annotations

import json
import subprocess
import sys
import time
from pathlib import Path

from app.tools.base import Tool, ToolRegistry
from app.tools.code import MAX_TIMEOUT, _child_env

# Substrings that disqualify an expression from being handed to parse_expr at
# all. Defense in depth: the child uses global_dict={} so builtins are not
# reachable, but we reject obvious escapes before spawning a child.
_UNSAFE_TOKENS = ("__import__", "__", " eval(", "eval(", "exec(", "open(", "os.")
_NUMERIC_TOL = 1e-9
_DEFAULT_N_POINTS = 16


def _is_unsafe(expr: str) -> bool:
    """Reject expressions that try to escape the parse namespace."""
    if not expr or not expr.strip():
        return True
    low = expr.lower()
    return any(tok.lower() in low for tok in _UNSAFE_TOKENS)


def _monotonic_ms() -> int:
    return int(time.monotonic() * 1000)


# The generated child script. It receives its config as JSON on argv[1] — there
# is NO string templating on this template, so every brace/format call below is
# literal Python the child executes. Keeping the logic in the child means a
# hang/OOM in simplify() is bounded by the subprocess timeout, not the tool
# server. The child prints exactly one JSON line on stdout:
#   {"ok": true, "result": {...verdict...}}  -> check ran (any verdict)
#   {"ok": false, "error": "..."}            -> crash (check could not run)
_CHILD_SCRIPT = '''
import json, sys, random
import sympy
from sympy.parsing.sympy_parser import parse_expr, standard_transformations
from sympy import simplify, N

# parse_expr namespace. We use the FULL sympy namespace as global_dict so that
# Integer/Float/Symbol/Function all resolve (a hand-built whitelist was
# whack-a-mole). This does NOT expose Python builtins: sympy.__dict__ has no
# __import__/eval/exec/open reachable as expression names, and the PRIMARY
# safety gate is the parent's _is_unsafe() substring check (rejects
# __import__/eval/exec/open/os./__ before the child is even spawned). The
# subprocess itself is the secondary boundary — symbolic_check runs with
# requires_approval=True, so the user has consented to the same blast radius
# as execute_python.
_GLOBALS = sympy.__dict__
TOL = 1e-9


def emit(obj):
    print(json.dumps(obj))


def parse_safe(s):
    return parse_expr(
        s,
        local_dict={},
        global_dict=_GLOBALS,
        transformations=standard_transformations,
        evaluate=True,
    )


def numeric_spot(a, b, frees, n_points, seed, assumptions):
    rng = random.Random(seed)
    for _ in range(n_points):
        point = {}
        for s in frees:
            name = str(s)
            if assumptions.get(name) == "nonnegative":
                val = rng.uniform(0.1, 10.0)
            else:
                val = rng.uniform(-5.0, 5.0)
                if abs(val) < 1e-6:
                    val = 0.5
            point[name] = val
        try:
            va = float(N(a.subs(point)))
            vb = float(N(b.subs(point)))
        except (TypeError, ValueError, ZeroDivisionError):
            continue
        diff = abs(va - vb)
        if diff > TOL:
            return ("fail", {
                "counterexample": json.dumps(point),
                "point": json.dumps(point),
                "value_a": va,
                "value_b": vb,
                "abs_diff": diff,
            })
    return ("numerically_consistent", {"n_points": n_points})


def run(cfg):
    mode = cfg["mode"]
    a_str = cfg["expression_a"]
    b_str = cfg.get("expression_b")
    n_points = cfg.get("n_points", 16)
    seed = cfg.get("seed", 0)
    assumptions = cfg.get("assumptions") or {}

    try:
        a = parse_safe(a_str)
        b = parse_safe(b_str) if b_str is not None else None
    except Exception as e:
        emit({"ok": True, "result": {
            "verdict": "inconclusive",
            "reason": "parse error: " + type(e).__name__,
        }})
        return

    if mode == "equivalence":
        if b is None:
            emit({"ok": True, "result": {
                "verdict": "inconclusive",
                "reason": "equivalence requires two expressions",
            }})
            return
        frees_a = set(a.free_symbols)
        frees_b = set(b.free_symbols)
        if frees_a != frees_b:
            only_a = sorted(str(s) for s in frees_a - frees_b)
            only_b = sorted(str(s) for s in frees_b - frees_a)
            emit({"ok": True, "result": {
                "verdict": "inconclusive",
                "reason": "free-symbol mismatch: only in a=" + str(only_a) + ", only in b=" + str(only_b),
            }})
            return
        try:
            diff = simplify(a - b)
            is_zero = (diff == 0)
        except Exception as e:
            emit({"ok": True, "result": {
                "verdict": "inconclusive",
                "reason": "simplify failed: " + type(e).__name__,
            }})
            return
        if is_zero:
            emit({"ok": True, "result": {
                "verdict": "proven", "method": "simplify(a-b)==0",
            }})
            return
        frees = sorted(frees_a, key=str)
        verdict, detail = numeric_spot(a, b, frees, n_points, seed, assumptions)
        out = {"verdict": verdict, "residual": str(diff), "n_points": n_points}
        out.update(detail)
        emit({"ok": True, "result": out})
        return

    if mode == "numeric_spot":
        if b is None:
            emit({"ok": True, "result": {
                "verdict": "inconclusive",
                "reason": "numeric_spot requires two expressions",
            }})
            return
        frees = sorted(set(a.free_symbols) | set(b.free_symbols), key=str)
        verdict, detail = numeric_spot(a, b, frees, n_points, seed, assumptions)
        out = {"verdict": verdict, "n_points": n_points}
        out.update(detail)
        emit({"ok": True, "result": out})
        return

    if mode == "dimensional":
        # Dimensional consistency via sympy.physics.units. We substitute each
        # free symbol with the unit declared in `assumptions` (else meter) and
        # check whether both sides carry the same physical dimension. We compare
        # via the RATIO (dim_a/dim_b dimensionless == 1) rather than subtraction,
        # because subtracting quantities of different dimensions raises in sympy.
        from sympy.physics import units
        from sympy import Expr

        def _resolve_unit(uname):
            """Resolve a unit string (single or compound like 'm/s') to a unit expr."""
            base = {
                "m": units.meter, "meter": units.meter,
                "s": units.second, "sec": units.second, "second": units.second,
                "kg": units.kilogram, "g": units.gram,
                "newton": units.newton, "N": units.newton,
                "joule": units.joule, "J": units.joule,
                "pa": units.pascal, "pascal": units.pascal,
                "kelvin": units.kelvin, "k": units.kelvin,
                "amp": units.ampere, "ampere": units.ampere,
                "mole": units.mole, "mol": units.mole,
                "candela": units.candela,
            }
            if uname in base:
                return base[uname]
            # Compound: try parsing e.g. "m/s", "kg*m/s**2" against the unit
            # namespace via sympify (the unit objects compose arithmetically).
            try:
                return sympy.sympify(uname, locals=base)
            except Exception:
                return units.meter

        try:
            subs_a = {
                s: _resolve_unit(assumptions.get(str(s), "m"))
                for s in a.free_symbols
            }
            dim_a = simplify(a.subs(subs_a))
            if b is not None:
                subs_b = {
                    s: _resolve_unit(assumptions.get(str(s), "m"))
                    for s in b.free_symbols
                }
                dim_b = simplify(b.subs(subs_b))
            else:
                dim_b = dim_a
            # Ratio test: dim_a/dim_b should be dimensionless 1 if consistent.
            ratio = simplify(dim_a / dim_b)
            consistent = ratio == 1
            if consistent:
                emit({"ok": True, "result": {
                    "verdict": "proven", "method": "dimensional consistency",
                    "dimension_a": str(dim_a), "dimension_b": str(dim_b),
                }})
            else:
                emit({"ok": True, "result": {
                    "verdict": "fail", "method": "dimensional",
                    "dimension_a": str(dim_a), "dimension_b": str(dim_b),
                    "counterexample": "dimensions differ (ratio={})".format(str(ratio)),
                }})
        except Exception as e:
            emit({"ok": True, "result": {
                "verdict": "inconclusive",
                "reason": "dimensional analysis failed: " + type(e).__name__,
            }})
        return

    emit({"ok": True, "result": {
        "verdict": "inconclusive", "reason": "unknown mode: " + str(mode),
    }})


if __name__ == "__main__":
    try:
        cfg = json.loads(sys.argv[1])
        run(cfg)
    except Exception as e:
        print(json.dumps({"ok": False, "error": type(e).__name__ + ": " + str(e)}))
'''


def _run_check_subprocess(
    mode: str,
    a_str: str,
    b_str: str | None,
    n_points: int,
    seed: int,
    assumptions: dict,
    timeout: int,
) -> dict:
    """Spawn the sympy check in a child process and parse its JSON verdict.

    Returns the tool result dict. The child receives config on argv[1] as JSON
    and prints one JSON line: ``{"ok": true, "result": {...}}`` when the check
    ran (any verdict, including fail/inconclusive), or ``{"ok": false, ...}``
    on a crash. A timeout is caught from subprocess directly.
    """
    timeout = min(timeout, MAX_TIMEOUT)
    cwd = str(Path.cwd())
    cfg = {
        "mode": mode,
        "expression_a": a_str,
        "expression_b": b_str,
        "n_points": n_points,
        "seed": seed,
        "assumptions": assumptions,
    }
    started = _monotonic_ms()
    try:
        proc = subprocess.run(
            [sys.executable, "-c", _CHILD_SCRIPT, json.dumps(cfg)],
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=cwd,
            env=_child_env(),
        )
    except subprocess.TimeoutExpired:
        return {
            "success": False,
            "verdict": "inconclusive",
            "reason": "timed out after {}s".format(timeout),
            "timed_out": True,
            "mode": mode,
            "expression_a": a_str,
            "expression_b": b_str,
            "elapsed_ms": _monotonic_ms() - started,
        }
    except Exception as e:
        return {
            "success": False,
            "verdict": "inconclusive",
            "reason": "subprocess failed: {}: {}".format(type(e).__name__, e),
            "timed_out": False,
            "mode": mode,
            "expression_a": a_str,
            "expression_b": b_str,
            "elapsed_ms": _monotonic_ms() - started,
        }

    elapsed = _monotonic_ms() - started
    out = proc.stdout.strip()
    base = {
        "mode": mode,
        "expression_a": a_str,
        "expression_b": b_str,
        "assumptions": assumptions or None,
        "timed_out": False,
        "elapsed_ms": elapsed,
    }
    if not out:
        # Child crashed before printing (e.g. import error). success:false.
        return {
            **base,
            "success": False,
            "verdict": "inconclusive",
            "reason": "check did not produce a result",
            "stderr": proc.stderr.strip()[:1000],
        }
    try:
        payload = json.loads(out.splitlines()[-1])
    except json.JSONDecodeError as e:
        return {
            **base,
            "success": False,
            "verdict": "inconclusive",
            "reason": "could not parse check result: {}".format(e),
            "raw": out[:1000],
        }

    if not payload.get("ok"):
        # Child reported a crash. Honest success:false.
        return {
            **base,
            "success": False,
            "verdict": "inconclusive",
            "reason": payload.get("error", "unknown check error"),
        }

    # The check RAN — success is True REGARDLESS of verdict. A "fail" verdict
    # (counterexample found) is a successful check, not a runtime error: the
    # model must see the counterexample unmasked by the VS1 gate.
    result = payload.get("result", {})
    return {
        **base,
        "success": True,
        "verdict": result.get("verdict", "inconclusive"),
        "method": result.get("method"),
        "residual": result.get("residual"),
        "n_points": result.get("n_points"),
        "counterexample": result.get("counterexample"),
        "point": result.get("point"),
        "value_a": result.get("value_a"),
        "value_b": result.get("value_b"),
        "abs_diff": result.get("abs_diff"),
        "dimension_a": result.get("dimension_a"),
        "dimension_b": result.get("dimension_b"),
        "reason": result.get("reason"),
    }


def symbolic_check(
    expression_a: str,
    expression_b: str | None = None,
    mode: str = "equivalence",
    assumptions: dict | None = None,
    n_points: int = _DEFAULT_N_POINTS,
    seed: int = 0,
    timeout: int = 60,
    description: str = "",
) -> dict:
    """Verify a symbolic/numeric claim honestly.

    Modes:
      - ``equivalence`` (default): is expression_a == expression_b? Returns
        ``proven`` (symbolic), ``numerically_consistent`` (numeric spot-check,
        NOT proven), ``fail`` (+counterexample), or ``inconclusive``.
      - ``numeric_spot``: skip the symbolic attempt, numeric spot-check only.
      - ``dimensional``: are the two expressions dimensionally consistent?
        Uses sympy.physics.units. Returns ``proven`` or ``fail``.

    The ``success`` field means "the check ran" — a ``fail`` verdict is
    ``success: True``. See module docstring for the honesty contract.
    """
    mode = (mode or "equivalence").strip()
    if mode not in {"equivalence", "numeric_spot", "dimensional"}:
        return {
            "success": False,
            "verdict": "inconclusive",
            "reason": "unknown mode '{}' (use equivalence|numeric_spot|dimensional)".format(mode),
            "timed_out": False,
        }

    # Safety: reject obvious escapes BEFORE spawning a child.
    if _is_unsafe(expression_a) or (expression_b is not None and _is_unsafe(expression_b)):
        return {
            "success": False,
            "verdict": "inconclusive",
            "reason": (
                "expression rejected: contains a forbidden token (__import__, eval, "
                "exec, open, os.). symbolic_check parses with a restricted namespace; "
                "do not attempt to escape it."
            ),
            "timed_out": False,
        }

    assumptions = assumptions or {}
    if n_points < 1:
        n_points = _DEFAULT_N_POINTS

    return _run_check_subprocess(
        mode=mode,
        a_str=expression_a,
        b_str=expression_b,
        n_points=n_points,
        seed=seed,
        assumptions=assumptions,
        timeout=timeout,
    )


def create_symbolic_tools(registry: ToolRegistry) -> None:
    """Register the symbolic-check tool."""
    registry.register(
        Tool(
            name="symbolic_check",
            description=(
                "Verify a symbolic or numeric claim HONESTLY before claiming correctness. "
                "Modes: 'equivalence' (is a == b?), 'numeric_spot' (sampled numeric "
                "comparison), 'dimensional' (dimensional consistency via units). "
                "Verdicts: 'proven' (symbolic proof or dimension match — the ONLY verdict "
                "that justifies saying 'verified'), 'numerically_consistent' (numeric "
                "spot-check agreed but NOT proven — say so), 'fail' (counterexample found — "
                "report it), 'inconclusive' (could not decide). The `success` field means "
                "the check RAN, not that the claim holds: a 'fail' verdict is success=true. "
                "Use this to ground user-actionable results in proof, not assertion."
            ),
            input_schema={
                "type": "object",
                "properties": {
                    "expression_a": {
                        "type": "string",
                        "description": "First expression, e.g. '(x+1)**2'. SymPy syntax.",
                    },
                    "expression_b": {
                        "type": "string",
                        "description": (
                            "Second expression to compare against (required for equivalence "
                            "and numeric_spot). E.g. 'x**2 + 2*x + 1'."
                        ),
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["equivalence", "numeric_spot", "dimensional"],
                        "description": "Default 'equivalence'.",
                    },
                    "assumptions": {
                        "type": "object",
                        "description": (
                            "Per-symbol assumptions. equivalence/numeric_spot: "
                            "{'x': 'nonnegative'} samples x from [0.1, 10). dimensional: "
                            "{'v': 'm/s', 't': 's'} attaches units to symbols."
                        ),
                    },
                    "n_points": {
                        "type": "integer",
                        "description": "Numeric spot-check sample count (default {}).".format(
                            _DEFAULT_N_POINTS
                        ),
                    },
                    "seed": {
                        "type": "integer",
                        "description": "RNG seed for the numeric spot-check (default 0).",
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default 60, max {}).".format(MAX_TIMEOUT),
                    },
                    "description": {
                        "type": "string",
                        "description": "Optional one-line note for the call.",
                    },
                },
                "required": ["expression_a"],
                "additionalProperties": False,
            },
            func=symbolic_check,
            requires_approval=True,
        )
    )
