"""Guards that a silent `symbolic_check` child explains itself.

Measured 2026-08-25: a live run got back `reason: "check did not produce a
result"` with an EMPTY stderr, twice, at 6ms and 37ms. The agent could not tell
a broken tool from bad input from a host out of resources, burned 7 checks and
13 patch attempts on the ambiguity, and finally declined to deliver rather than
present unverified numbers. The tool itself was fine — it reproduces green both
standalone and through the MCP path. What was missing was the exit status.
"""

import subprocess
from unittest import mock

import pytest

from app.tools import symbolic


def _proc(returncode, stdout="", stderr=""):
    return subprocess.CompletedProcess(
        args=["python", "-c", "..."], returncode=returncode, stdout=stdout, stderr=stderr
    )


@pytest.mark.parametrize(
    "returncode, must_say",
    [
        (-9, "killed by signal 9"),
        (-11, "killed by signal 11"),
    ],
)
def test_a_killed_child_is_named_as_a_host_condition(returncode, must_say):
    with mock.patch.object(symbolic.subprocess, "run", return_value=_proc(returncode)):
        out = symbolic.symbolic_check(
            expression_a="1+1", expression_b="2", mode="numeric_spot"
        )
    assert out["success"] is False
    assert out["verdict"] == "inconclusive"
    assert must_say in out["reason"], out["reason"]
    # The distinction that matters: this is NOT the caller's expression's fault.
    assert "HOST condition" in out["reason"]
    assert out["exit_code"] == returncode


def test_a_nonzero_exit_reports_the_code():
    with mock.patch.object(symbolic.subprocess, "run", return_value=_proc(3)):
        out = symbolic.symbolic_check(
            expression_a="1+1", expression_b="2", mode="numeric_spot"
        )
    assert "exited 3" in out["reason"], out["reason"]
    assert out["exit_code"] == 3


def test_a_clean_exit_with_no_output_says_the_check_did_not_run():
    with mock.patch.object(symbolic.subprocess, "run", return_value=_proc(0)):
        out = symbolic.symbolic_check(
            expression_a="1+1", expression_b="2", mode="numeric_spot"
        )
    assert "did not run" in out["reason"], out["reason"]
    assert out["exit_code"] == 0


def test_the_reason_is_never_the_old_undiagnosable_string():
    for rc in (-9, 1, 0):
        with mock.patch.object(symbolic.subprocess, "run", return_value=_proc(rc)):
            out = symbolic.symbolic_check(
                expression_a="1+1", expression_b="2", mode="numeric_spot"
            )
        assert out["reason"] != "check did not produce a result"
        assert str(out.get("elapsed_ms")) in out["reason"] or "ms" in out["reason"]
