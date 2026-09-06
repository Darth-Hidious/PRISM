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


def test_with_neither_the_engine_gate_speaks_once_and_names_both_routes(monkeypatch):
    """No local pycalphad and no sidecar: _delegate must fall through so the
    ONE canonical missing-extra shape answers — the same one the gate-order
    tests pin — naming the pip route and the sidecar provision route."""
    monkeypatch.setattr(calphad, "check_calphad_available", lambda: False)
    monkeypatch.setattr(calphad, "sidecar_available", lambda: False)
    assert calphad._delegate("calphad", {"action": "list_phases"}) is None
    out = calphad._calphad(action="list_phases", database_name="x")
    assert out.get("requires_extra") == "calphad", out
    assert "pip install" in out.get("install_hint", ""), out
    assert "provision" in out.get("provision_command", ""), out


def test_the_sidecar_never_delegates_to_itself(monkeypatch):
    """Inside the sidecar, pycalphad is present; if it were ever absent there,
    delegating would recurse forever."""
    monkeypatch.setenv("PRISM_IN_SIDECAR", "1")
    monkeypatch.setattr(calphad, "check_calphad_available", lambda: False)
    monkeypatch.setattr(calphad, "sidecar_available", lambda: True)
    monkeypatch.setattr(calphad, "sidecar_call", lambda t, a: (_ for _ in ()).throw(AssertionError("recursed")))
    out = calphad._calphad(action="list_phases", database_name="x")
    assert "error" in out, out


def test_compute_actions_delegate_too(monkeypatch):
    """The catalog tool was wired to the sidecar but calphad_compute — the one
    that does the science — was not, so equilibrium still answered "pycalphad
    is not installed" (2026-09-06)."""
    monkeypatch.setattr(calphad, "check_calphad_available", lambda: False)
    monkeypatch.setattr(calphad, "sidecar_available", lambda: True)
    seen = {}
    monkeypatch.setattr(calphad, "sidecar_call", lambda t, a: seen.update(tool=t, args=a) or {"phases": {"FCC_A1": 0.8}})
    out = calphad._calphad_compute(action="equilibrium", components=["Ni", "Cr"], conditions={"T": 773})
    assert seen["tool"] == "calphad_compute" and seen["args"]["action"] == "equilibrium", seen
    assert out.get("ran_in") == "sidecar" and "phases" in out, out


def test_components_reach_pycalphad_in_the_case_the_tdb_uses():
    """A real 773 K equilibrium refused with "X_AL refers to non-existent
    component" (2026-09-06): components arrived as 'Ni','Al' while every TDB
    species is upper case, so the mole-fraction condition matched nothing."""
    from app.tools.simulation.calphad_bridge import _normalise_components

    assert _normalise_components(["Ni", "Co", "Cr", "Al"]) == ["AL", "CO", "CR", "NI", "VA"]
    assert _normalise_components(["ni", "VA"]) == ["NI", "VA"]
