"""The science sidecar installs per capability, and says which it has.

Both capabilities used to be one pip run. On macOS arm64 that made them share
a fate they do not share: pyiron_atomistics 0.5.x pins mpi4py<=3.1.6, which
ships no wheel for this platform and needs an MPI compiler, so an
unsatisfiable pyiron requirement took CALPHAD down with it — and CALPHAD alone
resolves in about a tenth of a second. These tests pin the two properties that
fix bought: a capability that installed reports ready, and one that did not
says so by name instead of letting the caller find out as an ImportError.
"""

import pytest

from app.tools import _sidecar


@pytest.fixture()
def sidecar_venv(tmp_path, monkeypatch):
    venv = tmp_path / "venv-sci"
    venv.mkdir()
    monkeypatch.setattr(_sidecar, "SIDECAR_VENV", venv)
    return venv


def test_a_capability_that_installed_reports_ready(sidecar_venv):
    (sidecar_venv / ".provisioned").write_text("pycalphad\n")
    assert _sidecar.ensure_sidecar(install=False, capability="calphad") is None


def test_a_capability_that_did_not_install_is_named_not_silently_ready(sidecar_venv):
    # The marker exists — the venv IS provisioned, just not for pyiron. Before
    # this, marker-exists meant ready for everything, so a pyiron tool was told
    # to go ahead and failed later on an import it could not explain.
    (sidecar_venv / ".provisioned").write_text("pycalphad\n")

    error = _sidecar.ensure_sidecar(install=False, capability="pyiron")

    assert error is not None, "a missing capability must not report ready"
    assert "pyiron" in error
    assert "pyiron_atomistics" in error, f"name what is missing: {error}"
    assert "prism pyiron install" in error, f"say how to fix it: {error}"


def test_asking_for_nothing_in_particular_still_works(sidecar_venv):
    # Callers that do not name a capability keep the old meaning, so adding the
    # parameter cannot change behaviour underneath them.
    (sidecar_venv / ".provisioned").write_text("pycalphad\n")
    assert _sidecar.ensure_sidecar(install=False) is None


def test_an_unprovisioned_venv_is_never_ready(sidecar_venv):
    for capability in (None, "calphad", "pyiron"):
        assert _sidecar.ensure_sidecar(install=False, capability=capability) is not None


def test_the_capability_groups_are_separable(sidecar_venv):
    # The whole point: they are installed independently, so neither may appear
    # in the other's package list.
    calphad = set(_sidecar.SIDECAR_CAPABILITIES["calphad"])
    pyiron = set(_sidecar.SIDECAR_CAPABILITIES["pyiron"])
    assert calphad and pyiron
    assert not calphad & pyiron
    assert set(_sidecar.SIDECAR_PACKAGES) == calphad | pyiron


# ── A crashing sidecar is reported as a crash, with its traceback ───────────
#
# Its stderr went to DEVNULL, so a sidecar that died on its first request
# (a TypeError in the handler, a missing import) came back as "timed out":
# the one diagnosis that sends a person looking in the wrong place.


def _popen_that_crashes(argv, **kwargs):
    """Stand in for the sidecar: write a traceback to stderr and exit 3."""
    import subprocess as _sp
    import sys as _sys

    kwargs.pop("cwd", None)
    return _sp.Popen(
        [
            _sys.executable,
            "-c",
            "import sys; sys.stdin.readline(); "
            "sys.stderr.write('Traceback (most recent call last):\\n  File x\\n"
            "TypeError: ToolRegistry object is not subscriptable\\n'); sys.exit(3)",
        ],
        **kwargs,
    )


def test_a_sidecar_that_crashes_is_reported_with_its_traceback_not_as_a_timeout(monkeypatch):
    monkeypatch.setattr(_sidecar, "ensure_sidecar", lambda: None)
    monkeypatch.setattr(_sidecar.spawn, "popen", _popen_that_crashes)
    proc = _sidecar._SidecarProcess()
    response = proc.call("calphad_compute", {"x": 1})
    error = response.get("error", "")
    assert "exited (code 3)" in error, error
    assert "TypeError: ToolRegistry object is not subscriptable" in error, error
    assert "timed out" not in error, error
