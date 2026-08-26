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
