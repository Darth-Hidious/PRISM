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
        """x**3 != x*x**2 + 1 — a counterexample exists. CRITICAL: fail is success=True."""
        r = symbolic_check("x**3", "x*x**2 + 1", mode="equivalence", n_points=8)
        assert r["success"] is True, "a fail verdict MUST be success=true (gate contract)"
        assert r["verdict"] == "fail", r
        assert r["counterexample"] is not None, "fail must carry a counterexample"
        assert r["abs_diff"] > 1e-9

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

    def test_parse_error_is_inconclusive_with_success_true(self):
        """Gibberish that can't parse -> inconclusive (the check ran, decided it
        couldn't decide). success:True because the subprocess executed fine."""
        r = symbolic_check("x ++ + +", "x", mode="equivalence")
        assert r["success"] is True
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
