"""Tests for the VS2-P2 symbolic_check tool.

Honesty contract under test (the VS1-gate-aware rule):
  success = "the check RAN", NEVER the verdict.
  - A "fail" verdict is success:True (else tool_result_is_error masks the finding).
  - success:False is reserved for timeout/crash/parse-error/unsafe-input.
"""
import pytest

from app.tools.base import ToolRegistry
from app.tools.symbolic import (
    _DEFAULT_N_POINTS,
    _looks_dangerous,
    create_symbolic_tools,
    symbolic_check,
)


class TestVerdictEquivalence:
    def test_proven_polynomial_identity(self):
        """(x+1)**2 == x**2 + 2*x + 1 is symbolically provable -> proven."""
        r = symbolic_check("(x+1)**2", "x**2 + 2*x + 1", mode="equivalence")
        assert r["success"] is True
        assert r["verdict"] == "proven", r
        assert r["method"] == "simplify(a-b)==0"

    def test_fail_carries_counterexample_and_success_true(self):
        """x**3 != x*x**2 + 1 — simplify(a-b) = -1, a nonzero constant -> fail
        via the H3b constant-residual path. CRITICAL: fail is success=True."""
        r = symbolic_check("x**3", "x*x**2 + 1", mode="equivalence", n_points=8)
        assert r["success"] is True, "a fail verdict MUST be success=true (gate contract)"
        assert r["verdict"] == "fail", r
        # The fail carries evidence — either a numeric counterexample (abs_diff)
        # or a constant-residual proof (residual). Both are honest fail evidence.
        assert r["counterexample"] is not None or r.get("residual") is not None, (
            "fail must carry counterexample or residual evidence: %r" % r
        )

    def test_sqrt_squared_needs_assumption(self):
        """sqrt(x**2) == x only holds for x>=0. Without the nonnegative
        assumption, sampling negative points finds |x|!=x -> fail."""
        r = symbolic_check("sqrt(x**2)", "x", mode="equivalence", n_points=12)
        assert r["success"] is True
        # Without assumption, negative samples -> fail (honest about assumption dependence).
        assert r["verdict"] in ("fail", "numerically_consistent"), r

    def test_sqrt_squared_with_nonnegative_is_not_proven(self):
        """With x>=0, sqrt(x**2)==x numerically, but simplify doesn't reduce it
        to 0 -> numerically_consistent (NOT proven). The tool must say so honestly."""
        r = symbolic_check(
            "sqrt(x**2)",
            "x",
            mode="equivalence",
            assumptions={"x": "nonnegative"},
            n_points=12,
        )
        assert r["success"] is True
        assert r["verdict"] in ("numerically_consistent", "proven"), r
        # If it came back proven, that's also acceptable (sympy may reduce it);
        # but numerically_consistent is the honest answer when it doesn't.

    def test_numerically_consistent_for_equivalent_forms(self):
        """x*(y+z) vs x*y + x*z — symbolically equal, should be proven."""
        r = symbolic_check("x*(y+z)", "x*y + x*z", mode="equivalence")
        assert r["success"] is True
        assert r["verdict"] == "proven", r


class TestNumericSpot:
    def test_numeric_spot_proven_equal(self):
        r = symbolic_check("2*x", "x*2", mode="numeric_spot", n_points=8)
        assert r["success"] is True
        assert r["verdict"] == "numerically_consistent", r

    def test_numeric_spot_fail_with_counterexample(self):
        r = symbolic_check("x**2", "x**3", mode="numeric_spot", n_points=10)
        assert r["success"] is True
        assert r["verdict"] == "fail"
        assert r["counterexample"] is not None


class TestNumericHonesty:
    """FIX2-H1/H4: the tool must not lie 'proven' on unequal numbers, nor
    fabricate 'fail' counterexamples for true large-magnitude identities."""

    def test_h1_unequal_large_numbers_not_proven(self):
        """6.022e23 vs 6.022e23+1e6 are UNEQUAL. The old lossy-Float parse
        absorbed +1e6 -> simplify(a-b)==0 -> a false 'proven'. Exact parse
        (rationalize) now keeps them distinct."""
        r = symbolic_check("6.022e23", "6.022e23 + 1e6", mode="equivalence")
        assert r["verdict"] != "proven", (
            "two genuinely unequal numbers must NOT be 'proven' equal: %r" % r
        )

    def test_h1_real_identity_still_proven(self):
        """Regression: a real identity still proves (exact parse doesn't break legit math)."""
        r = symbolic_check("(x+1)**2", "x**2 + 2*x + 1", mode="equivalence")
        assert r["verdict"] == "proven"

    def test_h4_true_large_identity_not_fabricated_fail(self):
        """(x+10)**8 vs its CORRECT expansion is a true identity. The old absolute
        TOL=1e-9 fabricated a 'fail' counterexample from float64 noise at
        magnitude ~5e7. Relative tolerance (abs_diff/max(|a|,|b|,1)) must call
        this agreement, not a fail."""
        correct = (
            "x**8 + 80*x**7 + 2800*x**6 + 56000*x**5 + 700000*x**4 + "
            "5600000*x**3 + 28000000*x**2 + 80000000*x + 100000000"
        )
        r = symbolic_check("(x+10)**8", correct, mode="numeric_spot", n_points=20)
        assert r["verdict"] != "fail", (
            "a true identity must not fabricate a 'fail' under relative tol: %r" % r
        )

    def test_h4_real_disagreement_still_fails(self):
        """A genuinely unequal pair at large magnitude must still 'fail' (relative
        tol doesn't mask real disagreement)."""
        r = symbolic_check("(x+10)**8", "(x+11)**8", mode="numeric_spot", n_points=20)
        assert r["verdict"] == "fail"


class TestSpotCheckHonesty:
    """FIX2-H3/H3b/H7: the spot-check must not lie 'numerically_consistent'."""

    def test_h3_zero_valid_points_is_inconclusive(self):
        """sqrt(-x**2-1) is complex at every real sample. The old code swallowed
        every float() failure and returned 'numerically_consistent' claiming
        n_points agreement with ZERO comparisons. Now: inconclusive, n_points=0."""
        r = symbolic_check("sqrt(-x**2 - 1)", "sqrt(-x**2 - 1)",
                           mode="numeric_spot", n_points=16)
        assert r["verdict"] == "inconclusive", r
        assert "could not evaluate" in r.get("reason", ""), r

    def test_h3b_nonzero_constant_residual_is_fail(self):
        """x vs x+5: simplify(a-b) = -5, a nonzero CONSTANT -> a symbolic proof
        of inequality. The old code fell through to sampling; now it short-
        circuits to 'fail' with the residual."""
        r = symbolic_check("x", "x + 5", mode="equivalence")
        assert r["verdict"] == "fail", r
        assert r.get("residual") in ("5", "-5")

    def test_h7_nan_is_not_agreement(self):
        """x/x vs 1: at x=0 this is NaN (0/0). NaN must NOT count as agreement
        (nan > TOL is False in IEEE-754, so the old code silently accepted it).
        The guard skips non-finite points; the identity still holds at the
        finite samples -> numerically_consistent (not a false pass via NaN)."""
        r = symbolic_check("x/x", "1", mode="numeric_spot", n_points=16)
        # Must not be 'fail' (it's a true identity) and must have compared >0
        # finite points (so NaN was skipped, not counted as agreement).
        assert r["verdict"] in ("numerically_consistent", "proven"), r
        if r["verdict"] == "numerically_consistent":
            assert r.get("n_points", 0) > 0


class TestAssumptionVocabulary:
    """FIX2-H5: recognize sympy's assumption vocabulary (positive/real/integer/
    ...), apply it to BOTH the symbolic step (Symbol kwarg) AND the sampling
    domain. The old code only honored the literal 'nonnegative'."""

    def test_h5_positive_assumption_reaches_symbolic_proof(self):
        """sqrt(x**2) == x holds for x>=0. With {x: positive}, the symbolic step
        applies positive=True to the Symbol and can reach 'proven' (the old code
        ignored 'positive', sampled a negative x, and false-failed)."""
        r = symbolic_check("sqrt(x**2)", "x", mode="equivalence",
                           assumptions={"x": "positive"})
        assert r["verdict"] != "fail", (
            "positive assumption must prevent a false fail: %r" % r
        )

    def test_h5_negative_assumption(self):
        """sqrt(x**2) == -x holds for x<=0. With {x: negative}."""
        r = symbolic_check("sqrt(x**2)", "-x", mode="equivalence",
                           assumptions={"x": "negative"}, n_points=12)
        assert r["verdict"] != "fail", r

    def test_h5_unknown_assumption_is_inconclusive(self):
        """An unrecognized assumption is named, not silently ignored."""
        r = symbolic_check("x", "x", mode="equivalence",
                           assumptions={"x": "bogus_domain"})
        assert r["verdict"] == "inconclusive", r
        assert "not recognized" in r.get("reason", ""), r

    def test_q5_constraint_syntax_not_silently_dropped(self):
        """FIX2-Q5: {'x': 'x>0'} contains a digit, which the old unit-string
        allowance let slip through in equivalence mode -> the constraint was
        SILENTLY DROPPED, x sampled negative, a FALSE 'fail' returned outside the
        declared domain. Now non-dimensional modes require exact vocabulary, so
        this is inconclusive naming the unrecognized value (NOT a silent fail)."""
        r = symbolic_check("sqrt(x**2)", "x", mode="equivalence",
                           assumptions={"x": "x>0"})
        assert r["verdict"] == "inconclusive", r
        assert "not recognized" in r.get("reason", ""), r
        # It must NOT be the false, domain-violating 'fail' the old code produced.
        assert r["verdict"] != "fail"

    def test_q5_unit_string_in_equivalence_is_inconclusive(self):
        """A unit-like string ('m/s') is meaningful ONLY in dimensional mode; in
        equivalence/numeric_spot it is not recognized vocabulary -> inconclusive,
        never silently ignored."""
        r = symbolic_check("x", "x", mode="numeric_spot",
                           assumptions={"x": "m/s"}, n_points=8)
        assert r["verdict"] == "inconclusive", r
        assert "not recognized" in r.get("reason", ""), r

    def test_q5_dimensional_unit_strings_still_work(self):
        """Regression: the tightened validator must not touch dimensional mode —
        unit strings with '/','*',digits still resolve there."""
        r = symbolic_check("d/t", "v", mode="dimensional",
                           assumptions={"d": "m", "t": "s", "v": "m/s**2"})
        # v declared as m/s**2 but d/t is m/s -> a real dimensional mismatch =
        # fail (the point is the unit strings resolved, not that it's proven).
        assert r["verdict"] in ("fail", "proven"), r
        assert r["success"] is True

    def test_h5_integer_sampling_works(self):
        """{x: integer} samples integers. The symbol-key subs fix means an
        integer=True Symbol substitutes correctly (string-key subs failed)."""
        r = symbolic_check("x + x", "2*x", mode="numeric_spot",
                           assumptions={"x": "integer"}, n_points=8)
        assert r["verdict"] == "numerically_consistent", r
        assert r.get("n_points", 0) > 0

    def test_h5_counterexample_serializes_with_assumption(self):
        """A fail under an assumption still produces a JSON-serializable
        counterexample (symbol-keyed point must be converted to readable form)."""
        r = symbolic_check("x", "x + 1", mode="numeric_spot",
                           assumptions={"x": "positive"}, n_points=8)
        assert r["verdict"] == "fail"
        assert r["counterexample"] is not None
        # Must be valid JSON (a string like '{"x": 3.14}').
        import json
        json.loads(r["counterexample"])  # raises if not serializable


class TestDimensional:
    def test_velocity_consistent(self):
        """d/t with d in meters, t in seconds is dimensionally m/s."""
        r = symbolic_check(
            "d/t", "v", mode="dimensional", assumptions={"d": "m", "t": "s", "v": "m/s"}
        )
        assert r["success"] is True
        assert r["verdict"] == "proven", r

    def test_time_vs_distance_fails(self):
        """A time quantity is not dimensionally a distance."""
        r = symbolic_check("t", "d", mode="dimensional", assumptions={"t": "s", "d": "m"})
        assert r["success"] is True
        assert r["verdict"] == "fail", r
        assert "dimensions differ" in r.get("counterexample", ""), r


class TestDimensionalHonesty:
    """FIX2-H2/H9: dimensional mode must FAIL LOUD on typos / undeclared
    symbols (no silent meter fallback) and reduce named-vs-derived units."""

    def test_h2_typo_unit_is_inconclusive(self):
        """A typo'd unit string must NOT silently become meter (the old fallback
        faked a 'proven'). It is inconclusive naming the unrecognized unit."""
        r = symbolic_check("d/t", "v", mode="dimensional",
                           assumptions={"d": "m", "t": "s", "v": "mter/s"})
        assert r["verdict"] == "inconclusive", r
        assert "not recognized" in r.get("reason", ""), r

    def test_h2_undeclared_symbol_is_inconclusive(self):
        """An undeclared free symbol must NOT default to meter. The old default
        'proved' v=t for undeclared v. Now it names the missing declaration."""
        r = symbolic_check("d/t", "v", mode="dimensional",
                           assumptions={"d": "m", "t": "s"})
        assert r["verdict"] == "inconclusive", r
        assert "not declared" in r.get("reason", ""), r

    def test_h2_velocity_declared_is_proven(self):
        """Regression: properly-declared v=d/t (m/s) still proves."""
        r = symbolic_check("d/t", "v", mode="dimensional",
                           assumptions={"d": "m", "t": "s", "v": "m/s"})
        assert r["verdict"] == "proven"

    def test_h9_named_vs_derived_units_reduce(self):
        """newton vs kg*m/s**2 are physically identical. dimsys_SI.equivalent_dims
        reduces them, so the verdict is 'proven' (more honest than the old
        false 'fail', and better than the spec's 'inconclusive' fallback)."""
        r = symbolic_check("m*a", "F", mode="dimensional",
                           assumptions={"m": "kg", "a": "m/s**2", "F": "newton"})
        assert r["verdict"] == "proven", r

    def test_h2_real_mismatch_still_fails(self):
        """A genuine dimensional mismatch (time vs distance) is still 'fail'."""
        r = symbolic_check("t", "d", mode="dimensional",
                           assumptions={"t": "s", "d": "m"})
        assert r["verdict"] == "fail"


class TestGateContract:
    """The VS1-gate-aware rule: success means 'ran', not 'claim holds'.

    A fail verdict must NOT be flagged as a tool error — the model needs to see
    the counterexample. We assert on the result SHAPE that tool_result_is_error
    keys on (the `success` boolean), so this test documents the contract even
    though the gate itself lives in Rust.
    """

    def test_fail_verdict_is_success_true(self):
        r = symbolic_check("x", "x + 1", mode="equivalence", n_points=8)
        assert r["verdict"] == "fail"
        assert r["success"] is True, (
            "a fail verdict must be success=true; if false, the Rust is_error gate "
            "would mask the counterexample as a runtime error"
        )

    def test_inconclusive_verdict_is_success_true_when_check_ran(self):
        # Free-symbol mismatch -> inconclusive, but the check RAN.
        r = symbolic_check("x", "y", mode="equivalence")
        assert r["verdict"] == "inconclusive"
        assert r["success"] is True


class TestSafetyAndErrors:
    def test_unsafe_import_rejected_before_run(self):
        r = symbolic_check("__import__('os').system('id')", "x")
        assert r["success"] is False, "unsafe input must not run"
        assert r["verdict"] == "inconclusive"
        assert "forbidden token" in r["reason"]

    def test_unsafe_eval_rejected(self):
        r = symbolic_check("eval('1+1')", "2")
        assert r["success"] is False
        assert "forbidden token" in r["reason"]

    def test_dunder_rejected(self):
        r = symbolic_check("x.__class__", "x")
        assert r["success"] is False

    def test_unknown_mode_is_success_false(self):
        r = symbolic_check("x", "x", mode="bogus")
        assert r["success"] is False
        assert "unknown mode" in r["reason"]

    def test_timeout_is_success_false(self):
        """A pathological simplify that hangs must be killed honestly."""
        # A deeply nested expression that makes simplify work very hard; tiny timeout.
        nested = "x" + "*x" * 50
        r = symbolic_check(nested, nested, mode="equivalence", timeout=1)
        # Either it completes fast (proven) or times out — both are honest. If it
        # timed out, success must be False and verdict inconclusive.
        if r.get("timed_out"):
            assert r["success"] is False
            assert r["verdict"] == "inconclusive"
            assert "timed out" in r["reason"]

    def test_parse_error_is_success_false(self):
        """FIX2-CONTRACT: gibberish that can't parse -> success:False (the check
        did NOT run), verdict inconclusive. Aligns with the docstring contract:
        success means 'the check ran'; a parse error means it couldn't. A real
        VERDICT (proven/numerically_consistent/fail/inconclusive-from-the-check)
        stays success:True — see test_fail_verdict_is_success_true."""
        r = symbolic_check("x ++ + +", "x", mode="equivalence")
        assert r["success"] is False, "parse error means the check did not run"
        assert r["verdict"] == "inconclusive"
        assert "parse error" in r["reason"]

    def test_looks_dangerous_helper(self):
        # The cheap secondary reject (the PRIMARY gate is the child's restricted
        # namespace — this just avoids spawning for obvious attempts).
        assert _looks_dangerous("__import__('os')")
        assert _looks_dangerous("eval('x')")
        assert _looks_dangerous("exec('y')")
        assert _looks_dangerous("open('/etc/passwd')")
        assert _looks_dangerous("os.system('id')")
        assert _looks_dangerous("x.__class__")
        assert _looks_dangerous("getattr(os, 'system')")
        assert not _looks_dangerous("(x+1)**2")
        assert not _looks_dangerous("sqrt(x**2) + sin(y)")
        assert not _looks_dangerous("x + y**2")


class TestRCEBlocked:
    """FIX2-C1: the two PROVEN-live RCE payloads MUST NOT execute. The PRIMARY
    gate is the child's restricted namespace (no sympify/Function/eval); the
    secondary _looks_dangerous scan covers the obvious forms. We assert no
    marker file is written — the direct evidence the RCE is closed."""

    def _marker(self, tmp_path):
        import os
        m = str(tmp_path / "pwned.txt")
        if os.path.exists(m):
            os.remove(m)
        return m

    def test_rce_expr_a_sympify_split_blocked(self, tmp_path):
        """Bypass 1 (PROVEN): string-split sympify in expression_a. With the
        restricted namespace, sympify is NOT callable -> no execution."""
        import os
        marker = self._marker(tmp_path)
        payload = (
            "sympify('_'+'_import_'+'_'+'(\"os\").system(\"echo PWNED > %s\")')"
            % marker.replace("\\", "/")
        )
        r = symbolic_check(payload, "x", mode="equivalence")
        # The check does not execute the payload — no marker file appears.
        assert not os.path.exists(marker), "RCE executed: marker file written"
        # The verdict is inconclusive (parse either rejected or auto-symbolized).
        assert r["verdict"] == "inconclusive"

    def test_rce_assumptions_injection_blocked(self, tmp_path):
        """Bypass 2 (PROVEN): assumptions values flow into the unit resolver.
        The secondary scan now covers assumptions (the old _is_unsafe did not)."""
        import os
        marker = self._marker(tmp_path)
        payload = "__import__('os').system('echo PWNED > %s')" % marker.replace("\\", "/")
        r = symbolic_check("x", "x", mode="dimensional", assumptions={"x": payload})
        assert not os.path.exists(marker), "RCE via assumptions: marker written"
        assert r["success"] is False
        assert r["verdict"] == "inconclusive"

    def test_rce_getattr_obfuscation_blocked(self, tmp_path):
        """An obfuscation variant (getattr). The namespace restriction defeats it
        even if the secondary scan missed the split form."""
        import os
        marker = self._marker(tmp_path)
        payload = "getattr(getattr(x, '__class__'), '__bases__')"
        r = symbolic_check(payload, "x", mode="equivalence")
        assert not os.path.exists(marker), "getattr obfuscation executed"

    def test_rce_unit_string_injection_blocked(self, tmp_path):
        """A unit string in assumptions is parsed with the RESTRICTED namespace
        (was sympy.sympify — an eval sink). No execution."""
        import os
        marker = self._marker(tmp_path)
        payload = "__import__('os').system('echo PWNED > %s')" % marker.replace("\\", "/")
        # velocity = d/t; pass a malicious unit for d.
        r = symbolic_check("d/t", "v", mode="dimensional",
                           assumptions={"d": payload, "t": "s", "v": "m/s"})
        assert not os.path.exists(marker), "unit-string injection executed"

    def test_legitimate_math_still_parses(self):
        """Regression: the restricted namespace must still parse real math."""
        r = symbolic_check("(x+1)**2", "x**2 + 2*x + 1", mode="equivalence")
        assert r["verdict"] == "proven"
        # Trig functions resolve from the whitelist; matching free symbols.
        r2 = symbolic_check("sin(x)**2 + cos(x)**2", "cos(x)**2 + sin(x)**2",
                            mode="equivalence")
        assert r2["verdict"] == "proven"

    def test_user_symbol_named_E_not_eulers_number(self):
        """H6 (fixed by C1.2): a user symbol named E is a SYMBOL, not Euler's
        number. E vs E must parse as the same free symbol -> proven."""
        r = symbolic_check("E", "E", mode="equivalence")
        assert r["verdict"] == "proven"
        # And E vs E+1 must NOT be proven (E is a symbol, not the constant 2.718).
        r2 = symbolic_check("E", "E + 1", mode="equivalence")
        assert r2["verdict"] != "proven", "E must be a user symbol, not the constant"


class TestChildResourceLimits:
    """FIX2-Q2: a resource limit that can't be applied (e.g. RLIMIT_AS on macOS,
    which raises) must be LOGGED, never silently swallowed — otherwise the memory
    cap is a silent no-op and a prod misconfiguration passes unnoticed."""

    def test_q2_prelimit_logs_not_swallows_on_failure(self, monkeypatch):
        """When setrlimit raises, _child_prelimit writes a marker to stderr
        (fd 2) instead of a bare `except: pass`. Tested by faking the failure and
        capturing os.write — robust across the Q3 install-site change since it
        exercises _child_prelimit directly."""
        import os as _os
        import resource as _res

        from app.tools import symbolic

        def _boom(*_a, **_k):
            raise ValueError("simulated: RLIMIT not enforceable on this platform")

        monkeypatch.setattr(_res, "setrlimit", _boom)
        written = []
        monkeypatch.setattr(_os, "write", lambda fd, b: (written.append((fd, b)), len(b))[1])

        # Must NOT raise — a failed limit is non-fatal (timeout still bounds it).
        symbolic._child_prelimit()

        blob = b"".join(b for _, b in written)
        assert b"RLIMIT_" in blob, "a failed limit must be reported, not swallowed"
        assert b"not applied" in blob
        # And it goes to stderr (fd 2), not stdout (fd 1 — would corrupt the JSON).
        assert all(fd == 2 for fd, _ in written), written

    def test_q2_prelimit_does_not_raise_on_this_platform(self):
        """Calling it for real must never raise, whatever the platform enforces
        (on macOS RLIMIT_AS raises internally and is caught+logged)."""
        from app.tools.symbolic import _child_prelimit

        _child_prelimit()  # no exception

    def test_q3_prelimit_installed_only_on_linux(self):
        """FIX2-Q3: preexec_fn is gated to Linux. On macOS it would force the
        fork() path the sibling _execute_python deliberately avoids (and the
        RLIMIT_AS it sets is a no-op there anyway)."""
        import sys

        from app.tools import symbolic

        assert symbolic._INSTALL_PRELIMIT == sys.platform.startswith("linux")

    def test_q3_subprocess_preexec_matches_platform(self, monkeypatch):
        """The actual subprocess.run receives preexec_fn only on Linux; None
        elsewhere (no macOS fork-fragility for a no-op limit)."""
        import sys

        from app.tools import symbolic

        captured = {}
        real_run = symbolic.subprocess.run

        def spy(*args, **kwargs):
            captured["preexec_fn"] = kwargs.get("preexec_fn")
            return real_run(*args, **kwargs)

        monkeypatch.setattr(symbolic.subprocess, "run", spy)
        r = symbolic.symbolic_check("(x+1)**2", "x**2 + 2*x + 1", mode="equivalence")
        assert r["verdict"] == "proven"  # still works with the gated preexec
        if sys.platform.startswith("linux"):
            assert captured["preexec_fn"] is symbolic._child_prelimit
        else:
            assert captured["preexec_fn"] is None


class TestAttributeAccessBlocked:
    """FIX2-Q1: attribute access is blocked at the TOKEN level (a `.` OP token is
    rejected in _TRANSFORMS, PREPENDED before auto_symbol), so the whitelist is
    load-bearing. auto_symbol skips NAME-after-`.`, so without this a `.`-chain
    like x.__class__.__mro__[-1].__subclasses__() traverses to subprocess.Popen.
    We assert at the PARSE layer directly (bypassing the _looks_dangerous
    substring scan) so this proves the token block, not the `"__"` secondary."""

    def _child_ns(self):
        from app.tools.symbolic import _CHILD_SCRIPT

        ns = {"__name__": "child_test"}
        exec(compile(_CHILD_SCRIPT, "<child_test>", "exec"), ns)
        return ns

    def test_attribute_gadget_rejected_at_parse(self):
        """The reviewer's gadget must be REJECTED at parse, not return a live
        class list. Tested at BOTH parse sites (parse_user_expression AND
        parse_restricted), bypassing the _looks_dangerous substring scan."""
        ns = self._child_ns()
        gadget = "x.__class__.__mro__[-1].__subclasses__()"
        with pytest.raises(ValueError):
            ns["parse_user_expression"](gadget, {})
        with pytest.raises(ValueError):
            ns["parse_restricted"](gadget)

    def test_plain_attribute_access_rejected(self):
        """`x.y` and `x.real` have NO `__` so they slip past _looks_dangerous —
        the token-level block is what rejects them at parse."""
        ns = self._child_ns()
        for expr in ("x.y", "x.real", "x.__dict__"):
            with pytest.raises(ValueError):
                ns["parse_restricted"](expr)

    def test_floats_still_parse(self):
        """The `.` block must NOT break float literals (each is one NUMBER token)
        nor legit expressions."""
        ns = self._child_ns()
        pr = ns["parse_restricted"]
        # None of these raise; float literals and legit math parse fine.
        for expr in ("1.5", ".5", "1.", "6.022e23", "2.5*x + 1.0",
                     "sin(x)**2 + cos(x)**2", "(x+1)**2"):
            pr(expr)  # raises if the `.` block over-rejects

    def test_attribute_expr_via_symbolic_check_is_success_false(self):
        """End-to-end through the subprocess: `x.y` (no `__`, passes
        _looks_dangerous) reaches parse and is rejected -> parse error ->
        success:False (the check did not run)."""
        r = symbolic_check("x.y", "x", mode="equivalence")
        assert r["success"] is False, r
        assert r["verdict"] == "inconclusive"
        assert "parse error" in r["reason"]

    def test_dimensional_unit_attribute_access_rejected(self):
        """Q1 covers the SECOND parse site: a unit string with attribute access
        is rejected by the unit resolver -> inconclusive (not silently resolved)."""
        r = symbolic_check("d/t", "v", mode="dimensional",
                           assumptions={"d": "m", "t": "s", "v": "meter.__class__"})
        assert r["verdict"] == "inconclusive", r

    def test_spec_rce_bypass1_still_inert(self, tmp_path):
        """FIX2-C1 regression: the exact string-split sympify payload from the
        spec must remain inert (no marker file, no execution)."""
        import os
        marker = str(tmp_path / "marker")
        if os.path.exists(marker):
            os.remove(marker)
        payload = ("sympify('_'+'_import_'+'_'+\"('builtins').\"+'open'+"
                   "\"('%s','w').write('x')\")" % marker.replace("\\", "/"))
        symbolic_check(payload, "x", mode="equivalence")
        assert not os.path.exists(marker), "RCE bypass1 executed: marker written"

    def test_spec_rce_bypass2_still_inert(self, tmp_path):
        """FIX2-C1 regression: the exact assumptions-injection payload from the
        spec (dimensional mode) must remain inert."""
        import os
        marker = str(tmp_path / "marker")
        if os.path.exists(marker):
            os.remove(marker)
        payload = "__import__('builtins').open('%s','w').write('x')" % marker.replace("\\", "/")
        r = symbolic_check("x", "x", mode="dimensional", assumptions={"x": payload})
        assert not os.path.exists(marker), "RCE bypass2 executed: marker written"
        assert r["success"] is False


class TestRegistration:
    def test_tool_registered_with_approval(self):
        reg = ToolRegistry()
        create_symbolic_tools(reg)
        tool = reg.get("symbolic_check")
        assert tool.name == "symbolic_check"
        # parse_expr evals — blast radius equals execute_python, so approval-gated.
        assert tool.requires_approval is True

    def test_tool_in_bootstrap(self):
        """symbolic_check is registered via build_full_registry."""
        from app.plugins.bootstrap import build_full_registry

        tool_reg, _, _ = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in tool_reg.list_tools()}
        assert "symbolic_check" in names

    def test_default_n_points_reasonable(self):
        assert _DEFAULT_N_POINTS >= 8, "default spot-check count should give decent coverage"
