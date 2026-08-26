"""The marketplace catalog must not be able to lie.

Three failure modes are guarded here:

1. **Enumeration drift** — a new tool appears in the registry with no
   publish/keep-bundled verdict, or a catalog entry names a tool that no
   longer exists.
2. **Requirement drift** — a published tool grows a dependency its entry
   does not declare. That is the drift that makes a marketplace entry
   install cleanly and then fail on import.
3. **Dishonest gating** — a tool whose extra is absent raises instead of
   returning the `{error, install_hint, requires_extra}` shape.

Plus a shape check: the payload each entry publishes must match the fields
`POST /marketplace` and `PATCH /marketplace/{slug}` accept in marc27-core.
"""
from __future__ import annotations

import re
import tomllib
from pathlib import Path

import pytest

from app.tools import _extras, marketplace_catalog as catalog

REPO_ROOT = Path(__file__).resolve().parent.parent
KNOWN_EXTRAS = set(_extras.EXTRA_MODULES)

#: `resource_type` values marc27-core accepts — the enum in
#: crates/db/src/models/resource.rs and the CHECK constraint added by
#: migration 20260510000055_resource_type_text_check.sql.
RESOURCE_TYPES = {
    "plugin", "model", "mcp_server", "cli_tool", "hpc_cluster",
    "robot_lab", "test_facility", "dataset", "procedural_skill",
}

#: Licenses an entry may carry. `app/tools/**` and `app/mcp_*.py` are MIT
#: under the repo's dual license; PRISM as a whole is the dual license.
ENTRY_LICENSES = {"MIT", "LicenseRef-Mirdyne-Dual"}


@pytest.fixture(scope="module")
def registry_tool_names() -> set[str]:
    from app.plugins.bootstrap import build_full_registry

    registry, _providers, _ = build_full_registry(enable_mcp=False, enable_plugins=False)
    return {t.name for t in registry.list_tools()}


# ---------------------------------------------------------------------------
# 1. Enumeration
# ---------------------------------------------------------------------------

def test_every_registry_tool_has_a_verdict(registry_tool_names):
    """No tool may exist without a publish or keep-bundled decision."""
    judged = set(catalog.published_tools()) | set(catalog.bundled())
    assert registry_tool_names - judged == set(), (
        "tools with no marketplace verdict — add them to `entries` or to "
        "`bundled` (with a reason) in app/tools/marketplace_catalog.json"
    )


#: A tool name is declared at its registration site as `name="..."`. The
#: catalog spells the same names as `"name": "..."` in JSON, so this pattern
#: cannot match the catalog itself and accidentally vouch for a phantom.
_TOOL_NAME_DECLARATION = re.compile(r'name="([a-z0-9_]+)"')


def _tool_names_declared_in_source() -> set[str]:
    """Every tool name some `create_*_tools` function registers, extras or not."""
    declared: set[str] = set()
    for path in (REPO_ROOT / "app").rglob("*.py"):
        declared |= set(_TOOL_NAME_DECLARATION.findall(path.read_text(errors="ignore")))
    return declared


def test_catalog_names_no_phantom_tools(registry_tool_names):
    """A catalog entry must not name a tool that no longer exists.

    The registry alone cannot answer that. `bootstrap.py` registers whole
    families only when their optional extra imports — mace behind
    `check_mace_available()`, and likewise polymer and precipitation — so on a
    default install those tools are legitimately absent. Asserting against the
    registry therefore fails on provisioning, not on drift, and the fix would
    have been to DELETE truthful catalog entries.

    The question is whether the tool still exists, so the fallback oracle is
    the source: `app/tools/mace.py` imports and registers all ten of its tools
    with no mace-torch present. A name that is in neither the registry nor any
    registration site is a real phantom and still fails.
    """
    judged = set(catalog.published_tools()) | set(catalog.bundled())
    missing = judged - registry_tool_names
    phantom = missing - _tool_names_declared_in_source()
    assert phantom == set(), (
        f"catalog names tools that exist nowhere in app/: {sorted(phantom)}"
    )


def test_no_tool_is_both_published_and_bundled():
    assert set(catalog.published_tools()) & set(catalog.bundled()) == set()


def test_slugs_are_unique():
    slugs = [e["slug"] for e in catalog.entries()]
    assert len(slugs) == len(set(slugs))


def test_no_shell_or_filesystem_tool_is_published():
    """Publishing arbitrary execution as an installable item is a defect."""
    dangerous = {"execute_bash", "bash_task", "stop_bash_task", "execute_python", "file"}
    assert dangerous & set(catalog.published_tools()) == set()
    for name in dangerous:
        assert "SECURITY" in catalog.bundled()[name]


# ---------------------------------------------------------------------------
# 2. Requirements
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("entry", catalog.entries(), ids=lambda e: e["slug"])
def test_entry_declares_every_dependency_it_imports(entry):
    """Fails when a tool gains a dependency its entry does not account for.

    Every third-party import reachable from the entry's modules must come
    from a default install, from one of the entry's declared extras, or
    from the explicit unmanaged-optional list in `app/tools/_extras.py`.
    """
    undeclared = catalog.undeclared_imports(entry)
    assert undeclared == {}, (
        f"{entry['slug']} imports {sorted(undeclared)} which no declared "
        f"extra provides — declare the extra or add the dependency"
    )


@pytest.mark.parametrize("entry", catalog.entries(), ids=lambda e: e["slug"])
def test_declared_extras_exist(entry):
    declared = set(entry["requires_extras"]) | set(entry.get("optional_extras", []))
    assert declared <= KNOWN_EXTRAS
    for tool in entry["tools"]:
        assert set(tool["requires_extras"]) <= set(entry["requires_extras"])


@pytest.mark.parametrize("entry", catalog.entries(), ids=lambda e: e["slug"])
def test_metadata_agrees_with_the_entry(entry):
    """The wire payload must not disagree with the fields the tests check."""
    meta = entry["metadata"]
    assert meta["tools"] == [t["name"] for t in entry["tools"]]
    assert meta["requires_extras"] == entry["requires_extras"]
    assert meta.get("optional_extras", []) == entry.get("optional_extras", [])
    install = meta["install"]
    assert install["method"] in {"bundled", "python_extra", "installer"}
    for extra in entry["requires_extras"]:
        # The command shown must be the one that actually resolves, not the
        # `prism-platform[extra]` form (prism-platform is not on any index).
        assert install["command"] == _extras.install_command(extra)
        assert install["extra_command"] == _extras.extra_command(extra)
    if not entry["requires_extras"]:
        assert install["method"] in {"bundled", "installer"}


def test_extras_tables_match_pyproject():
    """`_extras` must list every extra pyproject declares (bar aggregates)."""
    pyproject = tomllib.loads((REPO_ROOT / "pyproject.toml").read_text())
    declared = set(pyproject["project"]["optional-dependencies"])
    # `dev` is tooling; `all`/`full` only re-export the others.
    assert declared - {"dev", "all", "full"} == KNOWN_EXTRAS
    assert set(_extras.EXTRA_DISTRIBUTIONS) == KNOWN_EXTRAS


@pytest.mark.parametrize("extra", sorted(KNOWN_EXTRAS))
def test_extra_distributions_match_pyproject(extra):
    """The install command must name exactly what the extra installs.

    Drift here is how a user ends up running a `pip install` that leaves the
    tool still broken.
    """
    pyproject = tomllib.loads((REPO_ROOT / "pyproject.toml").read_text())
    declared = {
        spec.split(";")[0].split(">")[0].split("<")[0].split("=")[0].strip()
        for spec in pyproject["project"]["optional-dependencies"][extra]
    }
    assert set(_extras.EXTRA_DISTRIBUTIONS[extra]) == declared


def test_core_modules_cover_pyproject_dependencies():
    """A core dependency missing from CORE_MODULES would fake a drift hit."""
    pyproject = tomllib.loads((REPO_ROOT / "pyproject.toml").read_text())
    for spec in pyproject["project"]["dependencies"]:
        dist = spec.split(">")[0].split("=")[0].split("[")[0].strip()
        module = _extras.DIST_TO_IMPORT.get(dist, dist.replace("-", "_"))
        assert module in _extras.CORE_MODULES, f"{dist} -> {module} not in CORE_MODULES"


# ---------------------------------------------------------------------------
# 3. Honest gating
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("extra", sorted(KNOWN_EXTRAS))
def test_missing_extra_error_shape(extra):
    err = _extras.missing_extra_error(extra, "boom")
    assert err["error"] == "boom"
    assert err["requires_extra"] == extra
    # The primary hint must not send anyone to a package index that 404s.
    assert err["install_hint"] == _extras.install_command(extra)
    assert "prism-platform[" not in err["install_hint"]
    assert err["install_extra_hint"] == f"pip install 'prism-platform[{extra}]'"


def test_absent_extra_reports_the_hint_instead_of_raising(registry_tool_names, monkeypatch):
    """For every published tool whose extra is absent here, calling it must
    return the honest install hint — never an ImportError.

    Only tools whose extra really is missing on this machine are exercised;
    the assertion is on behaviour, not on the environment.
    """
    from app.plugins.bootstrap import build_full_registry

    registry, _providers, _ = build_full_registry(enable_mcp=False, enable_plugins=False)
    tools = {t.name: t for t in registry.list_tools()}
    # The science tools proxy into a sidecar venv that would pip-install the
    # dependency; exercise their in-process implementations directly so the
    # test asserts the gate, not the provisioner.
    from app.tools.base import ToolRegistry
    from app.tools.calphad import create_calphad_tools
    from app.tools.mace import create_mace_tools
    from app.tools.sim_tools import create_simulation_tools

    local = ToolRegistry()
    create_calphad_tools(local)
    create_simulation_tools(local)
    # bootstrap registers the mace family only when the extra imports, so the
    # gated tools are absent from `registry` exactly when this test has
    # something to check. They define fine without mace-torch present.
    create_mace_tools(local)
    tools.update({t.name: t for t in local.list_tools()})

    # Probe a tool that needs the WHOLE extra. `structure_import` was the wrong
    # choice for mace: it is published under that extra but only ever imports
    # `ase`, which several extras provide, so on a machine with ase and no
    # mace-torch it correctly does its job — and the probe read that success as
    # a missing gate, then blamed a deliberately unparseable CIF.
    probes = {
        "calphad": ("calphad", {"action": "list_phases", "database_name": "x"}),
        "simulation": ("list_potentials", {}),
        "mace": ("mace_relax_structure", {"structure_ref": "cache://nonexistent"}),
        "ml": ("model_train", {"property_name": "band_gap"}),
    }
    checked = 0
    for extra, (tool_name, kwargs) in probes.items():
        if _extras.extra_available(extra) or tool_name not in tools:
            continue
        result = tools[tool_name].func(**kwargs)  # must not raise
        assert result.get("requires_extra") == extra, (tool_name, result)
        assert result["install_hint"] == _extras.install_command(extra)
        checked += 1
    if checked == 0:
        pytest.skip("every published extra is installed here — nothing to gate")


# ---------------------------------------------------------------------------
# 4. Publish payload shape
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("entry", catalog.entries(), ids=lambda e: e["slug"])
def test_publish_payload_matches_the_platform_contract(entry):
    """Fields `POST /marketplace` + `PATCH /marketplace/{slug}` accept.

    resource_type must be a value of marc27-core's `resource_type` enum;
    `cli_tool` is the one that PRISM's own `list_installable_tools` filter
    admits.
    """
    assert entry["resource_type"] in RESOURCE_TYPES, (
        "resource_type must be a value marc27-core's enum accepts, and one the "
        "hub already renders a tab for — do not invent a kind the UI cannot show"
    )
    assert entry["slug"] and entry["slug"] == entry["slug"].lower()
    assert "/" not in entry["slug"] and ".." not in entry["slug"]
    assert entry["name"] and entry["description"]
    assert entry["license"] in ENTRY_LICENSES, (
        "app/tools/** is MIT under the repo's dual license — see LICENSE"
    )
    assert entry["tags"] and all(isinstance(t, str) for t in entry["tags"])
    assert "prism" in entry["tags"] or "prism-tool" in entry["tags"]
    verification = entry["metadata"]["verification"]
    assert verification["level"] in {"end_to_end", "gate_only"}
    assert verification["note"]
    assert entry["metadata"]["dependency_licenses"]
