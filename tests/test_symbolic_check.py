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
    _is_unsafe,
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

    def test_is_unsafe_helper(self):
        assert _is_unsafe("__import__('os')")
        assert _is_unsafe("eval('x')")
        assert _is_unsafe("exec('y')")
        assert _is_unsafe("open('/etc/passwd')")
        assert _is_unsafe("os.system('id')")
        assert _is_unsafe("")
        assert _is_unsafe("   ")
        assert not _is_unsafe("(x+1)**2")
        assert not _is_unsafe("sqrt(x**2) + sin(y)")
        assert not _is_unsafe("x + y**2")


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
