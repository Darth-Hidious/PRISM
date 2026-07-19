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

THREAT MODEL — ATTRIBUTE ACCESS is the real risk the whitelist alone does NOT
close. ``standard_transformations``' auto_symbol deliberately skips a NAME after
``.`` ("Don't convert attribute access"), so once any identifier resolves to a
real object (a whitelisted sympy fn OR any user Symbol — Symbols are real
objects) a ``.``/``[]`` chain runs as plain Python. That turns a math string
into ``x.__class__.__mro__[-1].__subclasses__()`` -> subprocess.Popen. The
``"__"`` substring in _DANGEROUS_TOKENS is NOT a security boundary (the code
says so). The real gate is ``_reject_attribute_access`` — a token-level
transformation PREPENDED to ``_TRANSFORMS`` that rejects the ``.`` OP token, so
attribute traversal is impossible at parse (a float literal is a single NUMBER
token, so floats still parse). This makes the whitelist load-bearing: with no
attribute access and no eval-callable in the namespace, there is no path from a
parsed expression to code execution. Applied at BOTH parse sites (expressions
and dimensional unit strings).

Execution: the check runs in a subprocess (same pattern as _execute_python)
because ``simplify()`` can hang or OOM on pathological input. The WALL-CLOCK
TIMEOUT is the cross-platform defense — it kills a hang/blowup honestly on every
OS. Defense-in-depth: the child also gets a memory + CPU ulimit (``preexec_fn``),
but that memory cap is BEST-EFFORT and NOT enforced on macOS —
``setrlimit(RLIMIT_AS)`` raises there, so on Darwin it is a no-op and the
wall-clock timeout is the working defense. A limit that can't be applied is now
LOGGED, not silently swallowed, so a limit unexpectedly broken on the Linux
deploy target raises an alarm. The child receives its config as a JSON blob on
argv[1].
"""
from __future__ import annotations

import json
import logging
import subprocess
import sys
import time
from pathlib import Path

from app.tools.base import Tool, ToolRegistry
from app.tools.code import MAX_TIMEOUT, _child_env

logger = logging.getLogger(__name__)

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


# Q2: marker the child writes to stderr when a resource limit can't be applied,
# so the parent can surface it (a failed RLIMIT_AS means the memory cap is NOT
# enforced — this must never be a silent no-op). Scanned by _run_check_subprocess.
_RLIMIT_FAIL_MARKER = "symbolic_check[preexec]: RLIMIT_"


def _child_prelimit() -> None:
    """preexec_fn (installed on Linux only — see _run_check_subprocess): cap the
    child's address space + CPU so a pathological simplify() can't OOM/segfault
    the tool server (defense-in-depth; the restricted namespace is the PRIMARY
    fix). This runs post-fork/pre-exec in the child.

    HONEST PLATFORM POSTURE (Q2): the memory cap is enforced on Linux ONLY.
    macOS ``setrlimit(RLIMIT_AS)`` raises ValueError -> it is a NO-OP there, so
    the caller does not install this on Darwin (Q3) and the wall-clock timeout is
    the working macOS defense. If a limit CANNOT be applied we write a marker to
    stderr (captured + logged by the parent) rather than swallowing it silently,
    so a limit that is unexpectedly broken on the deploy target raises an alarm.
    """
    import os
    import resource

    def _warn(msg: str) -> None:
        # Post-fork, pre-exec in a (possibly multithreaded) parent: use raw
        # os.write to stderr — do NOT touch the logging lock or stdio buffers
        # inherited across the fork.
        try:
            os.write(2, msg.encode("utf-8", "replace"))
        except OSError:
            pass

    # Address-space cap: a runaway simplify allocating GBs dies with MemoryError
    # instead of the OS killing the parent. NOT enforced on macOS (raises).
    try:
        resource.setrlimit(
            resource.RLIMIT_AS, (_CHILD_MEM_LIMIT_BYTES, _CHILD_MEM_LIMIT_BYTES)
        )
    except (ValueError, OSError) as e:
        _warn(_RLIMIT_FAIL_MARKER + "AS not applied ({}): memory cap NOT enforced\n".format(e))
    # CPU seconds: belt-and-suspenders alongside the subprocess timeout (CPU time
    # can exceed wall time under parallelism, and a tight CPU limit catches a
    # busy-spin that the timeout takes a moment to kill).
    try:
        soft, hard = resource.getrlimit(resource.RLIMIT_CPU)
        resource.setrlimit(resource.RLIMIT_CPU, (300, hard if hard != -1 else 300))
    except (ValueError, OSError) as e:
        _warn(_RLIMIT_FAIL_MARKER + "CPU not applied ({})\n".format(e))


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
def _reject_attribute_access(tokens, local_dict, global_dict):
    """C1/Q1: block attribute access at the TOKEN level so the restricted
    whitelist is actually LOAD-BEARING. Root cause: standard_transformations'
    auto_symbol deliberately skips a NAME token that follows a `.` ("Don't
    convert attribute access"), so once any identifier resolves to a real object
    (a whitelisted sympy fn OR any user Symbol — Symbols ARE real objects) a
    `.`/`[]` chain runs as normal Python. That lets a math expression traverse
    `x.__class__.__mro__[-1].__subclasses__()` to reach subprocess.Popen — the
    whitelist alone does NOT stop it; only the `"__"` substring in
    _DANGEROUS_TOKENS did, and the code itself calls that "NOT a security
    boundary". A math-expression tool never needs attribute access, so we reject
    the `.` OP token outright.

    A float literal (`1.5`, `.5`, `1.`) tokenizes as a SINGLE NUMBER token (the
    `.` is part of the number, never a standalone OP), so rejecting the OP `.`
    blocks `x.__class__` WITHOUT breaking floats. Prepended before auto_symbol so
    the `.`-chain dies before any NAME resolves to a real object.
    """
    from token import OP

    for toknum, tokval in tokens:
        if toknum == OP and tokval == ".":
            raise ValueError(
                "attribute access ('.') is not permitted in symbolic expressions"
            )
    return tokens


# H1/H4: parse numeric literals as EXACT (rationalize transform converts Float
# literals to Rational/Integer AT PARSE, before evaluation). The old path relied
# on lossy Float64 — `6.022e23 + 1e6` absorbed `+1e6` -> simplify(a-b)==0 -> a
# false "proven" for two unequal numbers. With rationalize, `6.022e23+1e6` parses
# to the exact integer ...001000000, so a-b != 0. "proven" ONLY from an exact
# symbolic zero.
# C1/Q1: _reject_attribute_access is PREPENDED (before auto_symbol) — it is now
# the REAL gate that makes the whitelist load-bearing; the `"__"` substring in
# _DANGEROUS_TOKENS is only a cheap secondary. Both parse sites
# (parse_user_expression for expr_a/b AND parse_restricted for dimensional unit
# strings) share _TRANSFORMS, so the block covers both.
_TRANSFORMS = (_reject_attribute_access,) + standard_transformations + (rationalize,)
# H4: RELATIVE tolerance. The old absolute TOL=1e-9 fabricated "fail"
# counterexamples for true large-magnitude identities (e.g. (x+10)**8 vs its
# expansion: abs_diff ~4e-8 at magnitude ~5e7 is pure float64 noise, but
# exceeded 1e-9). Compare abs_diff / max(|a|,|b|, 1) against RTOL.
RTOL = 1e-9
# H5: the assumption vocabulary sympy-literate callers actually write. The old
# code only recognized the literal "nonnegative", silently ignoring the canonical
# "positive" (so sqrt(x**2) vs x with {x:positive} sampled a negative x -> a
# false "fail"). Map each to its sympy Symbol kwarg for the symbolic step AND
# drive the sampling domain below. An assumption NOT in this map -> inconclusive
# (never silently ignored).
_ASSUMPTION_KWARGS = {
    "positive": {"positive": True},
    "negative": {"negative": True},
    "nonnegative": {"nonnegative": True},
    "nonpositive": {"nonpositive": True},
    "real": {"real": True},
    "integer": {"integer": True},
    "rational": {"rational": True},
    "complex": {"complex": True},
    "even": {"even": True},
    "odd": {"odd": True},
    "prime": {"prime": True},
    "imaginary": {"imaginary": True},
}


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


def build_local_dict(names, assumptions):
    """Map each user symbol NAME to Symbol(name, **kwargs). Names colliding with
    sympy constants (E, pi, I, S, oo) are bound to the user's Symbol so the
    constant is shadowed. H5: declared assumptions (positive/real/...) are
    applied as sympy Symbol kwargs, so the symbolic simplify() step sees them
    (e.g. sqrt(x**2)==x under x:positive can reach proven). Returns (ld, err):
    err is a string naming an unrecognized assumption, or None."""
    ld = {}
    for n in names:
        kw = {}
        if n in assumptions:
            declared = assumptions[n]
            # Dimensional mode passes unit STRINGS (not assumptions) here; only
            # apply assumption kwargs for recognized vocabulary.
            if declared in _ASSUMPTION_KWARGS:
                kw = _ASSUMPTION_KWARGS[declared]
        ld[n] = Symbol(n, **kw)
    return ld


def _validate_assumptions(assumptions):
    """H5/Q5: in equivalence/numeric_spot mode every assumption value MUST be an
    EXACT recognized-vocabulary member (positive/negative/nonnegative/real/
    integer/...). Return an error string naming the first unrecognized value, or
    None. This validator is ONLY called for non-dimensional modes (see run()).

    Q5: the old unit-string allowance (`/`, `*`, digits) leaked into
    equivalence/numeric_spot, so a real constraint like {'x': 'x>0'} (the `0`
    tripped the digit allowance) was SILENTLY DROPPED -> the tool sampled a
    negative x and returned a FALSE 'fail' with a counterexample OUTSIDE the
    declared domain (violating H5 "never silently ignored"). Unit strings are
    valid ONLY in dimensional mode, which resolves them via _resolve_unit and
    never calls this validator — so there is no unit-string allowance here."""
    for name, val in assumptions.items():
        if val in _ASSUMPTION_KWARGS:
            continue
        return "assumption '{}'='{}' not recognized (use one of: {})".format(
            name, val, ", ".join(sorted(_ASSUMPTION_KWARGS))
        )
    return None


def _symbol_names(expr_str):
    """Cheap pre-parse scan for bare identifier names, so we can build a
    local_dict BEFORE parsing (binding user symbols over reserved constants).
    This does NOT execute anything — it's a regex over the raw string."""
    import re
    return set(re.findall(r"[A-Za-z_][A-Za-z0-9_]*", expr_str or ""))


def parse_user_expression(s, assumptions):
    """Parse an expression string, binding its bare identifiers as user Symbols
    (with assumptions applied) via local_dict, so E/pi/etc. are symbols not
    constants and assumption-bearing Symbols reach simplify()."""
    names = _symbol_names(s)
    bind = {n for n in names if n in _RESERVED or n not in _SAFE_GLOBALS}
    return parse_restricted(s, build_local_dict(bind, assumptions))


def _sample_for_assumption(rng, assumption):
    """Sample a real value respecting the declared assumption domain (H5).

    The old code only honored the literal "nonnegative"; sympy's canonical
    "positive"/"real"/"integer"/... were silently ignored, so an identity true
    only on a declared domain was sampled outside it -> a false "fail".
    """
    if assumption in ("positive", "nonnegative"):
        return rng.uniform(0.1, 10.0)
    if assumption == "negative":
        return rng.uniform(-10.0, -0.1)
    if assumption == "nonpositive":
        return rng.uniform(-10.0, 0.0)
    if assumption == "integer":
        # small nonzero integers
        return float(rng.randint(-5, 5) or 1)
    # real / rational / complex / None -> the default real domain.
    val = rng.uniform(-5.0, 5.0)
    if abs(val) < 1e-6:
        val = 0.5
    return val


def numeric_spot(a, b, frees, n_points, seed, assumptions):
    rng = random.Random(seed)
    # H3: count points ACTUALLY compared. The old code returned
    # "numerically_consistent" claiming n_points agreement when ZERO points were
    # comparable (every sample complex/NaN -> float() raised -> continue). Report
    # the VALID count, and if 0 -> inconclusive.
    compared = 0
    for _ in range(n_points):
        # H5: key the point by the SYMBOL OBJECT, not its string name. With an
        # assumption applied (e.g. Symbol('x', positive=True)), string-keyed
        # subs does NOT match (the assumption-bearing Symbol is a distinct
        # object from the plain Symbol('x') sympy creates internally for a
        # string key) -> every sample failed -> 0 compared -> a false
        # "inconclusive". Symbol-keyed subs matches correctly.
        point = {}
        point_readable = {}
        for s in frees:
            name = str(s)
            val = _sample_for_assumption(rng, assumptions.get(name))
            point[s] = val
            point_readable[name] = val
        try:
            va = float(N(a.subs(point)))
            vb = float(N(b.subs(point)))
        except (TypeError, ValueError, ZeroDivisionError):
            continue
        # H7: NaN/inf is NOT agreement (`nan > TOL` is False in IEEE-754, so the
        # old code silently counted it as consistent). A non-finite point is
        # unevaluable -> skip (counts toward the zero-valid-points check).
        if not (math.isfinite(va) and math.isfinite(vb)):
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
                "counterexample": json.dumps(point_readable),
                "point": json.dumps(point_readable),
                "value_a": va,
                "value_b": vb,
                "abs_diff": diff,
            })
        compared += 1
    if compared == 0:
        # H3: could not evaluate at ANY real point -> cannot claim consistency.
        return ("inconclusive", {
            "reason": "could not evaluate at any real point in the sampled domain",
            "n_points": 0,
        })
    return ("numerically_consistent", {"n_points": compared})


def run(cfg):
    mode = cfg["mode"]
    a_str = cfg["expression_a"]
    b_str = cfg.get("expression_b")
    n_points = cfg.get("n_points", 16)
    seed = cfg.get("seed", 0)
    assumptions = cfg.get("assumptions") or {}

    # H5: validate assumption vocabulary up front. An unrecognized assumption
    # (that isn't a unit string for dimensional mode) -> inconclusive naming it,
    # never silently ignored.
    if mode != "dimensional":
        bad = _validate_assumptions(assumptions)
        if bad:
            emit({"ok": True, "result": {
                "verdict": "inconclusive", "reason": bad,
            }})
            return

    try:
        a = parse_user_expression(a_str, assumptions)
        b = parse_user_expression(b_str, assumptions) if b_str is not None else None
    except Exception as e:
        # CONTRACT: a parse error means the check could NOT run -> ok:false ->
        # success:false (a real VERDICT stays ok:true/success:true; a fail
        # verdict is success:true). Aligns code with the module docstring.
        emit({"ok": False, "error": "parse error: " + type(e).__name__})
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
        # H3b: a NONZERO CONSTANT residual (no free symbols) is a symbolic proof
        # of INEQUALITY -> verdict fail, with the residual as evidence. The old
        # code always fell through to numeric sampling, which could coincidentally
        # miss and report "numerically_consistent" for expressions that provably
        # differ by a constant.
        try:
            residual_is_const = not diff.free_symbols
        except Exception:
            residual_is_const = False
        if residual_is_const and diff != 0:
            emit({"ok": True, "result": {
                "verdict": "fail",
                "method": "nonzero constant residual (proof of inequality)",
                "residual": str(diff),
                "counterexample": "a-b = {} (constant, nonzero)".format(str(diff)),
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
    # Q2: surface a failed child resource limit even on the success path (the
    # child's stderr is otherwise discarded when it produced a verdict). A
    # RLIMIT that couldn't be applied on the deploy target must not be a silent
    # no-op — log it so a prod misconfiguration is visible.
    if proc.stderr and _RLIMIT_FAIL_MARKER in proc.stderr:
        for line in proc.stderr.splitlines():
            if _RLIMIT_FAIL_MARKER in line:
                logger.warning("symbolic_check child resource limit: %s", line.strip())
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
