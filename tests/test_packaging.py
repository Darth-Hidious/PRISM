"""What actually reaches a user's venv.

The defect these tests exist for: the published wheel was built from a
hand-maintained package list in `pyproject.toml` that had gone stale, and
declared no package data at all. So the wheel shipped without
`app.tools.memory`, without `app.tools.materials`, and without
`app/tools/search_engine/providers/provider_overrides.json`.

`load_overrides()` reads that JSON unguarded, so `build_registry()` raised
FileNotFoundError on every installed copy; `app/plugins/bootstrap.py`
catches that and falls back to an empty `ProviderRegistry()`. The result
was `materials_search` returning `{"materials": [], "count": 0}` with exit
0 — a wrong answer that looked like a correct one.

Every existing test imports `app` straight from the checkout, where all of
this is present, so the whole suite stayed green while the artifact was
broken. These tests look at the artifact instead.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
APP = ROOT / "app"

# Files under app/ that are not product content: editor state, local tool
# indexes, bytecode. Everything else must ship.
_NOT_PRODUCT = ("__pycache__",)


def _is_product_file(path: Path) -> bool:
    rel = path.relative_to(APP)
    if any(part.startswith(".") for part in rel.parts):
        return False  # .claude/, .srclight/, …
    if any(part in _NOT_PRODUCT for part in rel.parts):
        return False
    return path.suffix not in {".pyc", ".pyo"}


def _clean_checkout(dest: Path) -> Path:
    """A build tree that looks like a fresh `git clone`, not this working copy.

    Building in place proves nothing. `include-package-data = true` makes
    setuptools honour `prism_platform.egg-info/SOURCES.txt`, and that file
    is left behind by any earlier editable/sdist build — listing files the
    CURRENT config would not select. So a developer's local build looked
    complete while release CI, building from a clean checkout with no
    egg-info, produced the wheel that shipped without
    `app.tools.memory`. Both mutation-checked: with the stale packaging
    config restored, an in-place build stays green and this one goes red.
    """
    src = dest / "src"
    src.mkdir(parents=True)
    shutil.copytree(
        APP,
        src / "app",
        ignore=shutil.ignore_patterns("__pycache__", "*.pyc", "*.pyo"),
    )
    for name in ("pyproject.toml", "MANIFEST.in", "README.md"):
        if (ROOT / name).exists():
            shutil.copy2(ROOT / name, src / name)
    for licence in ROOT.glob("LICENSE*"):
        if licence.is_file():
            shutil.copy2(licence, src / licence.name)
    if (ROOT / "NOTICE").exists():
        shutil.copy2(ROOT / "NOTICE", src / "NOTICE")
    return src


@pytest.fixture(scope="module")
def wheel_names(tmp_path_factory) -> list[str]:
    """Build the wheel this repo would publish; return its member names."""
    work = tmp_path_factory.mktemp("wheel")
    src = _clean_checkout(work)
    out = work / "dist"
    proc = subprocess.run(
        # --no-cache-dir is load-bearing, not hygiene: pip's wheel cache
        # otherwise returns a wheel built from an earlier state of the tree.
        [
            sys.executable,
            "-m", "pip", "wheel",
            "--no-deps", "--no-cache-dir",
            "-w", str(out),
            str(src),
        ],
        capture_output=True,
        text=True,
        timeout=900,
    )
    if proc.returncode != 0:
        pytest.fail(
            "could not build the wheel — this test cannot verify what ships:\n"
            f"{proc.stdout[-4000:]}\n{proc.stderr[-4000:]}"
        )
    wheels = list(out.glob("prism_platform-*.whl"))
    assert len(wheels) == 1, f"expected exactly one wheel, got {wheels}"
    return zipfile.ZipFile(wheels[0]).namelist()


def test_every_python_subpackage_under_app_ships(wheel_names):
    """A package missing from the wheel is an ImportError on a user's box."""
    expected = sorted(
        str(p.relative_to(ROOT))
        for p in APP.rglob("__init__.py")
        if _is_product_file(p)
    )
    missing = [p for p in expected if p not in wheel_names]
    assert not missing, (
        f"{len(missing)} subpackage(s) never reach an installed venv: {missing}"
    )
    # The two the stale list actually dropped, named so a regression is
    # readable without decoding a diff.
    for pkg in ("app/tools/memory/__init__.py", "app/tools/materials/__init__.py"):
        assert pkg in wheel_names


def test_every_non_python_asset_under_app_ships(wheel_names):
    """Data files are code as far as a user is concerned."""
    expected = sorted(
        str(p.relative_to(ROOT))
        for p in APP.rglob("*")
        if p.is_file() and p.suffix != ".py" and _is_product_file(p)
    )
    assert expected, "no non-.py assets found under app/ — the test is looking wrong"
    missing = [p for p in expected if p not in wheel_names]
    assert not missing, (
        f"{len(missing)} asset(s) never reach an installed venv: {missing}"
    )


def test_the_file_whose_absence_silenced_materials_search_ships(wheel_names):
    """`load_overrides()` reads this unguarded; without it the whole
    provider federation collapses to an empty registry."""
    assert (
        "app/tools/search_engine/providers/provider_overrides.json" in wheel_names
    )
