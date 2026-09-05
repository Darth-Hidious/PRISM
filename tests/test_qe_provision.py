"""`prism provision qe`: build pw.x from source and fetch a pseudopotential set.

Not shipped in the box — provisioned into ~/.prism by the user or the model
through PRISM's own mechanism, like the science extras. The plan is data
first: every step names what it runs and why, so a dry run is a readable
recipe and a real run is that recipe executed.
"""

import json

from app.tools.simulation.qe import provision


def test_the_plan_names_every_step_and_its_source(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    plan = provision.plan()
    kinds = [step["kind"] for step in plan["steps"]]
    assert kinds == ["check_toolchain", "clone", "configure", "build", "install", "fetch_pseudopotentials", "write_manifest", "verify"]
    clone = next(s for s in plan["steps"] if s["kind"] == "clone")
    assert clone["url"].startswith("https://gitlab.com/QEF/q-e") and clone["tag"].startswith("qe-7.")
    fetch = next(s for s in plan["steps"] if s["kind"] == "fetch_pseudopotentials")
    assert fetch["url"].startswith("https://www.pseudo-dojo.org/") and "CC BY 4.0" in fetch["license"]
    assert plan["install_prefix"] == str(tmp_path / ".prism" / "qe")


def test_a_dry_run_executes_nothing_and_reports_what_is_already_there(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    result = provision.run(dry_run=True)
    assert result["dry_run"] is True
    assert result["already_present"] == {"pw_x": False, "pseudopotentials": False}
    assert not (tmp_path / ".prism" / "qe").exists()


def test_toolchain_check_names_what_is_missing(monkeypatch):
    monkeypatch.setattr(provision.shutil, "which", lambda name: None)
    missing = provision.missing_toolchain()
    assert set(missing) >= {"gfortran", "cmake", "mpirun"}


def test_manifest_records_url_checksum_and_license(tmp_path):
    path = provision.write_manifest(tmp_path, url="https://x/set.tgz", sha256="abc", upf_count=3)
    data = json.loads(path.read_text())
    assert data["source_url"] == "https://x/set.tgz" and data["sha256_of_archive"] == "abc"
    assert data["upf_count"] == 3 and "CC BY 4.0" in data["license"]
