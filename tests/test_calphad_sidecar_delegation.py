"""CALPHAD runs in the science sidecar when the main interpreter cannot.

2026-09-06: `calphad(action='list_phases')` answered "pycalphad is not
installed" on a machine where pycalphad 0.11.2 was installed and working — in
the sidecar venv, which exists for exactly this reason (the main venv is
Python 3.14 and pycalphad pins a dependency that has no 3.14 wheel). The
sidecar registers the CALPHAD tools; nothing routed to it. The check must ask
"can anything here run pycalphad", not "can this interpreter".
"""
import app.tools.calphad as calphad


def test_a_compute_action_delegates_to_the_sidecar_when_local_pycalphad_is_absent(monkeypatch):
    monkeypatch.setattr(calphad, "check_calphad_available", lambda: False)
    monkeypatch.setattr(calphad, "sidecar_available", lambda: True)
    seen = {}

    def fake_call(tool, args):
        seen["tool"] = tool
        seen["args"] = args
        return {"phases": ["FCC_A1", "BCC_A2"], "database_name": args.get("database_name")}

    monkeypatch.setattr(calphad, "sidecar_call", fake_call)
    out = calphad._calphad(action="list_phases", database_name="steel_odbl")
    assert out.get("phases") == ["FCC_A1", "BCC_A2"], out
    assert seen["tool"] == "calphad", seen
    assert seen["args"]["action"] == "list_phases" and seen["args"]["database_name"] == "steel_odbl", seen
    assert out.get("ran_in") == "sidecar", "the caller must be told where it ran"


def test_local_pycalphad_is_used_without_the_sidecar(monkeypatch):
    monkeypatch.setattr(calphad, "check_calphad_available", lambda: True)
    called = {"sidecar": False}
    monkeypatch.setattr(calphad, "sidecar_call", lambda t, a: called.__setitem__("sidecar", True) or {})
    monkeypatch.setattr(calphad, "_list_phases", lambda **k: {"phases": ["LIQUID"]})
    out = calphad._calphad(action="list_phases", database_name="x")
    assert out == {"phases": ["LIQUID"]}, out
    assert called["sidecar"] is False, "no sidecar hop when the local interpreter can do it"


def test_with_neither_the_error_names_both_routes(monkeypatch):
    monkeypatch.setattr(calphad, "check_calphad_available", lambda: False)
    monkeypatch.setattr(calphad, "sidecar_available", lambda: False)
    out = calphad._calphad(action="list_phases", database_name="x")
    assert "error" in out, out
    assert "sidecar" in str(out).lower(), f"say the sidecar route exists: {out}"


def test_the_sidecar_never_delegates_to_itself(monkeypatch):
    """Inside the sidecar, pycalphad is present; if it were ever absent there,
    delegating would recurse forever."""
    monkeypatch.setenv("PRISM_IN_SIDECAR", "1")
    monkeypatch.setattr(calphad, "check_calphad_available", lambda: False)
    monkeypatch.setattr(calphad, "sidecar_available", lambda: True)
    monkeypatch.setattr(calphad, "sidecar_call", lambda t, a: (_ for _ in ()).throw(AssertionError("recursed")))
    out = calphad._calphad(action="list_phases", database_name="x")
    assert "error" in out, out
