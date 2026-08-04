"""Tests for agent-triggered science-extra provisioning."""
from __future__ import annotations

import subprocess

from app.tools import _provision


def test_unknown_extra_is_rejected_without_running_a_process() -> None:
    result = _provision.provision_extra("not-a-science-extra")
    assert result["supported_extras"]
    assert "error" in result


def test_provision_uses_the_rust_harness(monkeypatch) -> None:
    monkeypatch.setenv("PRISM_BINARY", "/opt/prism")
    monkeypatch.setenv("PRISM_PROJECT_ROOT", "/workspace/prism")
    calls = []

    def fake_run(command, **kwargs):
        calls.append((command, kwargs))
        return subprocess.CompletedProcess(command, 0, "Provisioned", "")

    monkeypatch.setattr(_provision.subprocess, "run", fake_run)
    result = _provision.provision_extra("mace")

    assert result["provisioned"] is True
    assert calls[0][0] == [
        "/opt/prism",
        "--python",
        _provision.sys.executable,
        "--project-root",
        "/workspace/prism",
        "provision",
        "extra",
        "mace",
    ]
    assert calls[0][1]["timeout"] == 900


def test_provision_failure_is_structured(monkeypatch) -> None:
    monkeypatch.setenv("PRISM_BINARY", "/opt/prism")
    monkeypatch.setattr(
        _provision.subprocess,
        "run",
        lambda command, **kwargs: subprocess.CompletedProcess(
            command, 3, "", "offline wheelhouse missing"
        ),
    )
    result = _provision.provision_extra("calphad")
    assert result["error"].endswith("exit code 3")
    assert result["provision_command"].endswith("provision extra calphad")
    assert "wheelhouse" in result["stderr"]
