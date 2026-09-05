"""`prism provision qe` — Quantum ESPRESSO into ~/.prism, from source.

There is no Homebrew formula and conda-forge ships no arm64 build (checked
2026-09-05), so pw.x is built from the QE repository with the machine's own
toolchain (gfortran, clang, Open MPI, OpenBLAS, FFTW, CMake). Pseudopotentials
come from PseudoDojo (NC SR v0.4, PBE, standard accuracy; CC BY 4.0) with a
manifest recording URL, checksum and licence that every run's provenance
carries. Nothing here is shipped in the PRISM box: it is provisioned on
demand, and the QE tools report `unavailable` with this command as the remedy
until it has been.
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

QE_REPO = "https://gitlab.com/QEF/q-e.git"
QE_TAG = "qe-7.4.1"
PSEUDO_SET = "pseudo-dojo-nc-sr-04-pbe-standard"
PSEUDO_URL = "https://www.pseudo-dojo.org/pseudos/nc-sr-04_pbe_standard_upf.tgz"
PSEUDO_LICENSE = (
    "CC BY 4.0 (PseudoDojo; van Setten et al., Comput. Phys. Commun. 226, 39 (2018), "
    "doi:10.1016/j.cpc.2018.01.012)"
)
TOOLCHAIN = ("gfortran", "cmake", "mpirun", "make", "git")


def _home() -> Path:
    return Path(os.environ.get("HOME", str(Path.home())))


def install_prefix() -> Path:
    return _home() / ".prism" / "qe"


def pseudo_dir() -> Path:
    return _home() / ".prism" / "pseudopotentials" / PSEUDO_SET


def missing_toolchain() -> list[str]:
    return [tool for tool in TOOLCHAIN if shutil.which(tool) is None]


def plan() -> dict[str, Any]:
    """The recipe as data: what each step runs and why."""
    prefix = install_prefix()
    src = prefix / "src" / "q-e"
    build = src / "build-clang"
    return {
        "install_prefix": str(prefix),
        "steps": [
            {"kind": "check_toolchain", "needs": list(TOOLCHAIN), "why": "QE is built from source here; no package exists for this platform"},
            {"kind": "clone", "url": QE_REPO, "tag": QE_TAG, "into": str(src)},
            {
                "kind": "configure",
                "cwd": str(build),
                "command": [
                    "cmake", "..",
                    f"-DCMAKE_INSTALL_PREFIX={prefix}",
                    "-DQE_ENABLE_MPI=ON", "-DQE_ENABLE_OPENMP=ON",
                    "-DCMAKE_Fortran_COMPILER=gfortran", "-DCMAKE_C_COMPILER=clang",
                    "-DBLA_VENDOR=OpenBLAS",
                    "-DCMAKE_PREFIX_PATH=/opt/homebrew/opt/openblas;/opt/homebrew/opt/fftw;/opt/homebrew/opt/open-mpi",
                ],
            },
            {"kind": "build", "cwd": str(build), "command": ["make", f"-j{os.cpu_count() or 4}", "pw"]},
            {"kind": "install", "cwd": str(build), "command": ["make", "install"]},
            {"kind": "fetch_pseudopotentials", "url": PSEUDO_URL, "into": str(pseudo_dir()), "license": PSEUDO_LICENSE},
            {"kind": "write_manifest", "path": str(pseudo_dir() / "MANIFEST.json")},
            {"kind": "verify", "command": [str(prefix / "bin" / "pw.x"), "-h"]},
        ],
    }


def write_manifest(directory: Path, *, url: str, sha256: str, upf_count: int) -> Path:
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / "MANIFEST.json"
    path.write_text(json.dumps({
        "set": "PseudoDojo NC SR v0.4, PBE, standard accuracy",
        "source_url": url,
        "sha256_of_archive": sha256,
        "license": PSEUDO_LICENSE,
        "fetched_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "upf_count": upf_count,
        "note": "Norm-conserving ONCVPSP pseudopotentials. PRISM's default ecutwfc for this set is 60 Ry, overridable per run.",
    }, indent=2))
    return path


def _run(cmd: list[str], cwd: Path | None, log: list[str]) -> None:
    log.append(f"$ {' '.join(cmd)}")
    proc = subprocess.run(cmd, cwd=str(cwd) if cwd else None, capture_output=True, text=True)
    if proc.returncode != 0:
        tail = "\n".join((proc.stdout + "\n" + proc.stderr).strip().splitlines()[-25:])
        raise RuntimeError(f"{cmd[0]} failed ({proc.returncode}):\n{tail}")


def fetch_pseudopotentials(log: list[str]) -> dict[str, Any]:
    target = pseudo_dir()
    target.mkdir(parents=True, exist_ok=True)
    log.append(f"fetch {PSEUDO_URL}")
    with tempfile.NamedTemporaryFile(suffix=".tgz", delete=False) as fh:
        with urllib.request.urlopen(PSEUDO_URL, timeout=600) as resp:
            shutil.copyfileobj(resp, fh)
        archive = Path(fh.name)
    sha = hashlib.sha256(archive.read_bytes()).hexdigest()
    with tarfile.open(archive) as tf:
        tf.extractall(target, filter="data")
    archive.unlink()
    count = len(list(target.glob("*.upf")))
    manifest = write_manifest(target, url=PSEUDO_URL, sha256=sha, upf_count=count)
    return {"directory": str(target), "upf_count": count, "manifest": str(manifest)}


def run(*, dry_run: bool = False) -> dict[str, Any]:
    """Provision QE. A dry run executes nothing and reports the plan plus what
    is already present; a real run executes the missing steps."""
    prefix = install_prefix()
    pw = prefix / "bin" / "pw.x"
    present = {"pw_x": pw.is_file(), "pseudopotentials": (pseudo_dir() / "MANIFEST.json").is_file()}
    result: dict[str, Any] = {"dry_run": dry_run, "plan": plan(), "already_present": present, "log": []}
    if dry_run:
        return result
    log: list[str] = result["log"]
    missing = missing_toolchain()
    if missing and not present["pw_x"]:
        result["status"] = "blocked"
        result["reason"] = f"toolchain missing: {missing} (brew install gcc open-mpi openblas fftw cmake)"
        return result
    try:
        if not present["pw_x"]:
            src = prefix / "src" / "q-e"
            if not src.exists():
                src.parent.mkdir(parents=True, exist_ok=True)
                _run(["git", "clone", "--depth", "1", "--branch", QE_TAG, QE_REPO, str(src)], None, log)
            build = src / "build-clang"
            build.mkdir(exist_ok=True)
            steps = {s["kind"]: s for s in plan()["steps"]}
            for kind in ("configure", "build", "install"):
                _run(steps[kind]["command"], build, log)
        if not present["pseudopotentials"]:
            result["pseudopotentials"] = fetch_pseudopotentials(log)
        _run([str(pw), "-h"], None, log) if pw.is_file() else None
        result["status"] = "ok" if pw.is_file() else "failed"
        result["pw_x"] = str(pw) if pw.is_file() else None
    except Exception as exc:
        result["status"] = "failed"
        result["reason"] = str(exc)
    return result


def main(argv: list[str] | None = None) -> int:
    import argparse
    import sys

    parser = argparse.ArgumentParser(prog="prism provision qe")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)
    out = run(dry_run=args.dry_run)
    print(json.dumps(out, indent=2, default=str))
    return 0 if out.get("status", "ok") in ("ok",) or args.dry_run else 1


if __name__ == "__main__":
    raise SystemExit(main())
