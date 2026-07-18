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

SECURITY (C1, the PRIMARY gate): every caller-controlled string (expression_a,
expression_b, assumption unit strings) is parsed with a RESTRICTED namespace —
an explicit whitelist of safe symbolic names, NOT ``sympy.__dict__``. The old
``_GLOBALS = sympy.__dict__`` exposed ``sympify`` (a callable eval sink), and
``parse_expr(evaluate=True)`` invoked it, giving arbitrary code execution via
string-splitting past the substring blocklist. The whitelist contains NO
callable that evals/imports (no sympify, no Function, no lambdify, no eval);
with ``sympify`` absent, ``sympify('...')`` in a parsed expression auto-
symbolizes to ``Symbol('sympify')`` applied as a symbolic function — no real
call, no execution. User symbols are bound via ``local_dict`` so a symbol named
``E``/``pi``/``I``/``S``/``oo`` is a SYMBOL, not the sympy constant.

Execution: the check runs in a subprocess (same pattern as _execute_python)
because ``simplify()`` can hang or OOM on pathological input — the timeout
kills it honestly. Defense-in-depth: the child gets a memory + CPU ulimit
(``preexec_fn``) so a pathological input can't crash the tool server via OOM.
The child receives its config as a JSON blob on argv[1].
"""
from __future__ import annotations

import json
import subprocess
import sys
import time
from pathlib import Path

from app.tools.base import Tool, ToolRegistry
from app.tools.code import MAX_TIMEOUT, _child_env

_NUMERIC_TOL = 1e-9
_DEFAULT_N_POINTS = 16
# Memory cap (bytes) for the child subprocess — defense-in-depth against an
# OOM/SIGSEGV from a pathological simplify() input. ~1 GB.
_CHILD_MEM_LIMIT_BYTES = 1024 * 1024 * 1024


def _monotonic_ms() -> int:
    return int(time.monotonic() * 1000)


# C1: a cheap SECONDARY pre-spawn reject. The PRIMARY gate is the child's
# restricted namespace (no sympify/Function/eval reachable) — that is what makes
# the RCE impossible. This scan just avoids spawning a child for inputs that are
# obviously trying to escape (substring match across EVERY caller string,
# including assumptions values which the old _is_unsafe never checked). It is
# NOT a security boundary on its own: string-splitting defeats it, which is why
# the namespace restriction is the real fix.
_DANGEROUS_TOKENS = ("__import__", "__", "eval(", "exec(", "open(", "os.", "getattr")


def _looks_dangerous(*strings: str) -> bool:
    """Cheap secondary reject across all caller-controlled strings."""
    for s in strings:
        if not s:
            continue
        low = s.lower()
        if any(tok.lower() in low for tok in _DANGEROUS_TOKENS):
            return True
    return False


def _child_prelimit() -> None:
    """preexec_fn: cap the child's memory + CPU so a pathological simplify()
    input can't crash the tool server via OOM (C1.4 defense-in-depth). The
    restricted namespace is the PRIMARY fix; this is the secondary boundary.

    POSIX-only — on platforms without ``resource`` the limits are silently
    skipped (the timeout still bounds wall time).
    """
    try:
        import resource

        # Address-space cap: a runaway simplify allocating GBs dies with
        # MemoryError instead of the OS killing the parent.
        resource.setrlimit(resource.RLIMIT_AS, (_CHILD_MEM_LIMIT_BYTES, _CHILD_MEM_LIMIT_BYTES))
        # CPU seconds: belt-and-suspenders alongside the subprocess timeout
        # (CPU time can exceed wall time under parallelism, and a tight CPU
        # limit catches a busy-spin that the timeout takes a moment to kill).
        soft, hard = resource.getrlimit(resource.RLIMIT_CPU)
        resource.setrlimit(resource.RLIMIT_CPU, (300, hard if hard != -1 else 300))
    except (ImportError, ValueError, OSError):
        # Non-POSIX or limit already lower — the namespace restriction + the
        # subprocess timeout remain in force.
        pass


# The generated child script. It receives its config as JSON on argv[1] — there
# is NO string templating on this template, so every brace/format call below is
# literal Python the child executes. Keeping the logic in the child means a
# hang/OOM in simplify() is bounded by the subprocess timeout + the preexec
# ulimit, not the tool server. The child prints exactly one JSON line on stdout:
#   {"ok": true, "result": {...verdict...}}  -> check ran (any verdict)
#   {"ok": false, "error": "..."}            -> crash/parse-error (could not run)
_CHILD_SCRIPT = '''
import json, sys, random, math

from sympy.parsing.sympy_parser import parse_expr, standard_transformations, rationalize
from sympy import (
    simplify, N, Symbol, Integer, Float, Rational, Pow,
    sin, cos, tan, asin, acos, atan, atan2, sinh, cosh, tanh,
    exp, log, sqrt, Abs, Min, Max, floor, ceiling, sign, gamma,
    pi, oo, zoo, E,
)

# C1 SECURITY: an explicit whitelist of ONLY safe symbolic names. NO callable
# that evals/imports — no sympify, no Function, no lambdify, no eval/exec. The
# old `_GLOBALS = sympy.__dict__` exposed sympify (an eval sink reachable as an
# expression name), which parse_expr(evaluate=True) INVOKED -> arbitrary code
# execution via string-splitting past the substring blocklist. With sympify
# ABSENT from this namespace, `sympify('...')` in a parsed expression
# auto-symbolizes to Symbol('sympify') applied as a symbolic function — no real
# call, no execution.
_SAFE_GLOBALS = {
    "Symbol": Symbol, "Integer": Integer, "Float": Float, "Rational": Rational,
    "Pow": Pow,
    "sin": sin, "cos": cos, "tan": tan, "asin": asin, "acos": acos, "atan": atan,
    "atan2": atan2, "sinh": sinh, "cosh": cosh, "tanh": tanh,
    "exp": exp, "log": log, "sqrt": sqrt, "Abs": Abs, "Min": Min, "Max": Max,
    "floor": floor, "ceiling": ceiling, "sign": sign, "gamma": gamma,
    "pi": pi, "oo": oo, "zoo": zoo, "E": E,
}
# Symbols that sympy would otherwise resolve to constants via _SAFE_GLOBALS.
# User-declared symbols override these via local_dict (see parse_restricted), so
# a user symbol named E/pi/I/S is THEIR symbol, not the constant.
_RESERVED = {"E", "pi", "I", "S", "oo", "zoo"}
# H1/H4: parse numeric literals as EXACT (rationalize transform converts Float
# literals to Rational/Integer AT PARSE, before evaluation). The old path relied
# on lossy Float64 — `6.022e23 + 1e6` absorbed `+1e6` -> simplify(a-b)==0 -> a
# false "proven" for two unequal numbers. With rationalize, `6.022e23+1e6` parses
# to the exact integer ...001000000, so a-b != 0. "proven" ONLY from an exact
# symbolic zero.
_TRANSFORMS = standard_transformations + (rationalize,)
# H4: RELATIVE tolerance. The old absolute TOL=1e-9 fabricated "fail"
# counterexamples for true large-magnitude identities (e.g. (x+10)**8 vs its
# expansion: abs_diff ~4e-8 at magnitude ~5e7 is pure float64 noise, but
# exceeded 1e-9). Compare abs_diff / max(|a|,|b|, 1) against RTOL.
RTOL = 1e-9


def emit(obj):
    print(json.dumps(obj))


def parse_restricted(s, local_dict=None):
    """Parse a caller-controlled string against the SAFE whitelist ONLY.

    Every untrusted string (expression_a/b, assumption unit strings) MUST go
    through here. local_dict binds user symbol names -> Symbol(name) so a user
    symbol named E/pi/I/S is THEIR symbol, not the sympy constant (C1.2 / H6).
    Numeric literals parse EXACT (rationalize) so lossy Floats can't fake a
    "proven" (H1).
    """
    return parse_expr(
        s,
        local_dict=local_dict or {},
        global_dict=_SAFE_GLOBALS,
        transformations=_TRANSFORMS,
        evaluate=True,
    )


def build_local_dict(names):
    """Map each user symbol NAME to Symbol(name). Names that collide with sympy
    constants (E, pi, I, S, oo) get bound to the user's Symbol so the constant
    is shadowed. (Assumption kwargs are added in H5.)"""
    ld = {}
    for n in names:
        ld[n] = Symbol(n)
    return ld


def _symbol_names(expr_str):
    """Cheap pre-parse scan for bare identifier names, so we can build a
    local_dict BEFORE parsing (binding user symbols over reserved constants).
    This does NOT execute anything — it's a regex over the raw string."""
    import re
    return set(re.findall(r"[A-Za-z_][A-Za-z0-9_]*", expr_str or ""))


def parse_user_expression(s):
    """Parse an expression string, binding its bare identifiers as user Symbols
    via local_dict (so E/pi/etc. are symbols, not constants)."""
    names = _symbol_names(s)
    # Only bind names that are NOT whitelisted math functions (sin, exp, ...) —
    # those should resolve to the function. Reserved constants ARE bound (so a
    # user symbol named E wins over Euler's number).
    bind = {n for n in names if n in _RESERVED or n not in _SAFE_GLOBALS}
    return parse_restricted(s, build_local_dict(bind))


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
        # H4: RELATIVE tolerance. The old absolute `diff > TOL` fabricated "fail"
        # counterexamples for true large-magnitude identities (float64 noise at
        # magnitude ~5e7 exceeded 1e-9). Normalize by the value scale so a "fail"
        # is a REAL disagreement, not rounding noise. A small absolute floor (1)
        # keeps tiny-magnitude comparisons meaningful.
        scale = max(abs(va), abs(vb), 1.0)
        if diff > RTOL * scale:
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
        a = parse_user_expression(a_str)
        b = parse_user_expression(b_str) if b_str is not None else None
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
        # Dimensional consistency via sympy.physics.units. Substitute each free
        # symbol with the unit declared in `assumptions` and compare the resulting
        # DIMENSIONS via the SI dimension system. C1: unit strings parsed with
        # parse_restricted (the SAME safe namespace), NOT sympy.sympify.
        # H2: FAIL LOUD — the old code silently substituted units.meter on any
        # unrecognized unit string AND defaulted undeclared symbols to meter ->
        # a typo or missing declaration "proved" dimensional consistency.
        # H9: named-vs-derived units (newton vs kg*m/s**2) ARE physically
        # identical; dimsys_SI.equivalent_dims reduces them correctly (a plain
        # ratio==1 could not, which produced a false "fail").
        from sympy.physics import units
        from sympy.physics.units import Quantity
        from sympy.physics.units.systems.si import dimsys_SI

        def _resolve_unit(uname):
            """Resolve a unit string to a unit expr, or None if unrecognized.
            NO silent meter fallback (H2)."""
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
            try:
                parsed = parse_restricted(uname, base)
            except Exception:
                return None
            # Reject if the parse produced free symbols that are NOT recognized
            # Quantities (a typo like "mter" becomes a free Symbol, not a unit).
            if parsed.free_symbols:
                return None
            return parsed

        def _resolve_side(expr):
            """Resolve every free symbol's unit. FAIL LOUD on:
            - an undeclared symbol (no unit in assumptions) -> inconclusive
            - an unrecognized unit string -> inconclusive
            Returns (subs_dict, None) or (None, reason_str)."""
            subs = {}
            for s in expr.free_symbols:
                name = str(s)
                if name not in assumptions:
                    return None, "dimension of symbol '{}' not declared (pass it in assumptions, e.g. {{'{}': 'm'}})".format(name, name)
                uname = assumptions[name]
                unit = _resolve_unit(uname)
                if unit is None:
                    return None, "unit '{}' not recognized".format(uname)
                subs[s] = unit
            return subs, None

        def _dim_of(expr):
            """Reduce an expression of Quantities to its Dimension."""
            subs = {q: q.dimension for q in expr.atoms(Quantity)}
            return simplify(expr.subs(subs))

        try:
            subs_a, err = _resolve_side(a)
            if err:
                emit({"ok": True, "result": {
                    "verdict": "inconclusive", "reason": err,
                }})
                return
            dim_a = _dim_of(a.subs(subs_a))
            if b is not None:
                subs_b, err = _resolve_side(b)
                if err:
                    emit({"ok": True, "result": {
                        "verdict": "inconclusive", "reason": err,
                    }})
                    return
                dim_b = _dim_of(b.subs(subs_b))
            else:
                dim_b = dim_a
            consistent = dimsys_SI.equivalent_dims(dim_a, dim_b)
            if consistent:
                emit({"ok": True, "result": {
                    "verdict": "proven", "method": "dimensional consistency",
                    "dimension_a": str(dim_a), "dimension_b": str(dim_b),
                }})
            else:
                emit({"ok": True, "result": {
                    "verdict": "fail", "method": "dimensional",
                    "dimension_a": str(dim_a), "dimension_b": str(dim_b),
                    "counterexample": "dimensions differ",
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
            preexec_fn=_child_prelimit,
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

    assumptions = assumptions or {}
    if n_points < 1:
        n_points = _DEFAULT_N_POINTS

    # C1: cheap SECONDARY reject across EVERY caller string (expressions AND
    # assumption values — the old _is_unsafe never scanned assumptions, which
    # was a live RCE vector). The PRIMARY gate is the child's restricted
    # namespace; this just avoids spawning for obvious escape attempts.
    assumption_values = [str(v) for v in assumptions.values()]
    if _looks_dangerous(expression_a or "", expression_b or "", *assumption_values):
        return {
            "success": False,
            "verdict": "inconclusive",
            "reason": (
                "input rejected: contains a forbidden token. symbolic_check parses "
                "with a restricted namespace (no sympify/Function/eval); do not "
                "attempt to escape it."
            ),
            "timed_out": False,
        }

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
