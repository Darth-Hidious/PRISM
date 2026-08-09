"""Capability conformance: a declared filterable field must actually filter.

THE CONTRACT: for every provider that can be built from the shipped
configuration, every field declared in ``capabilities.filterable_fields``
must be a field its translator actually emits. The engine's capability gate
(`ProviderCapabilities.can_handle`) admits queries based on these
declarations; a declared-but-dropped field means the gate passes, the
provider returns UNFILTERED results, and nothing warns -- a capability lie.

Mechanism: ``Provider.describe_query`` is contractually derived from the same
code path ``search()`` dispatches with (see providers/base.py). So a field
genuinely reaches the wire if and only if a query constraining ONLY that
field produces a different description than the empty query. A declared
field with no probe at all means the engine's query model cannot even
express it -- an advertisement nothing could ever exercise.

This test caught, at introduction time: mp_native declaring space_group,
nelements and bulk_modulus while ``to_mp_kwargs`` dropped all three, and
declaring is_metal which ``MaterialSearchQuery`` cannot express.
"""
from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import patch

import pytest

from app.tools.search_engine.providers.endpoint import ProviderEndpoint
from app.tools.search_engine.providers.registry import (
    _PROVIDER_FACTORIES,
    load_provider_plugins,
)
from app.tools.search_engine.query import MaterialSearchQuery, PropertyRange

_ROOT = Path(__file__).parent.parent
_CATALOG_PATH = _ROOT / "app" / "plugins" / "catalog.json"
_OVERRIDES_PATH = (
    _ROOT / "app" / "tools" / "search_engine" / "providers"
    / "provider_overrides.json"
)

# One query per capability field name, constraining ONLY that field. A field
# missing from this map cannot be expressed by MaterialSearchQuery at all.
_PROBES: dict[str, MaterialSearchQuery] = {
    "elements": MaterialSearchQuery(elements=["Si"]),
    "formula": MaterialSearchQuery(formula="SiO2"),
    "nelements": MaterialSearchQuery(n_elements=PropertyRange(min=2, max=3)),
    "space_group": MaterialSearchQuery(space_group="Fm-3m"),
    "crystal_system": MaterialSearchQuery(crystal_system="cubic"),
    "band_gap": MaterialSearchQuery(band_gap=PropertyRange(min=1.0, max=2.0)),
    "formation_energy": MaterialSearchQuery(
        formation_energy=PropertyRange(min=-1.0, max=0.0)
    ),
    "energy_above_hull": MaterialSearchQuery(
        energy_above_hull=PropertyRange(min=0.0, max=0.1)
    ),
    "bulk_modulus": MaterialSearchQuery(
        bulk_modulus=PropertyRange(min=100.0, max=200.0)
    ),
    "debye_temperature": MaterialSearchQuery(
        debye_temperature=PropertyRange(min=100.0, max=500.0)
    ),
}


def _shipped_provider_declarations() -> list[tuple[str, dict, str]]:
    """Every (id, endpoint_config, declaration_source) shipped in this repo.

    - catalog.json: marketplace/native providers (mp_native, ...). Entries
      whose api_type has no registered adapter factory cannot be built, so
      there is no translator to hold to the declaration yet; they are
      skipped. The moment a factory is registered they are checked.
    - provider_overrides.json ``defaults``: the capabilities applied to every
      auto-discovered OPTIMADE endpoint -- checked once through a
      representative endpoint, plus any override that redeclares its own
      filterable_fields.
    """
    load_provider_plugins()
    declarations: list[tuple[str, dict, str]] = []

    catalog = json.loads(_CATALOG_PATH.read_text())
    for pid, entry in catalog.get("plugins", {}).items():
        if entry.get("type") != "provider":
            continue
        if entry.get("api_type") not in _PROVIDER_FACTORIES:
            continue
        config = dict(entry)
        config.setdefault("id", pid)
        declarations.append((pid, config, "app/plugins/catalog.json"))

    overrides_data = json.loads(_OVERRIDES_PATH.read_text())
    defaults = overrides_data.get("defaults", {})
    base = {
        "id": "optimade_defaults_representative",
        "name": "OPTIMADE defaults (representative)",
        "base_url": "https://example.org/optimade",
        "enabled": True,
        **{k: v for k, v in defaults.items() if k != "enabled"},
    }
    declarations.append(
        (base["id"], base, "provider_overrides.json:defaults")
    )
    for pid, override in overrides_data.get("overrides", {}).items():
        declared = override.get("capabilities", {}).get("filterable_fields")
        if not declared:
            continue  # inherits the defaults, already checked above
        config = dict(base)
        config["id"] = pid
        config["capabilities"] = {
            **defaults.get("capabilities", {}),
            **override["capabilities"],
        }
        declarations.append((pid, config, "provider_overrides.json:overrides"))

    return declarations


def test_every_declared_filterable_field_is_actually_translated():
    declarations = _shipped_provider_declarations()
    # If config loading ever silently changes shape, this test must not
    # degrade into vacuously checking nothing.
    checked_ids = {pid for pid, _, _ in declarations}
    assert "mp_native" in checked_ids
    assert "optimade_defaults_representative" in checked_ids

    empty = MaterialSearchQuery()
    failures: list[str] = []
    # MP_API_KEY selects the MPRester branch of describe_query -- the branch
    # whose translation (QueryTranslator.to_mp_kwargs) the catalog capability
    # declaration describes. The keyless proxy branch narrows every query to
    # a formula pull and reports/refuses that honestly on its own.
    with patch.dict("os.environ", {"MP_API_KEY": "conformance-test-key"}):
        for pid, config, source in declarations:
            endpoint = ProviderEndpoint.model_validate(config)
            provider = _PROVIDER_FACTORIES[endpoint.api_type](endpoint)
            baseline = provider.describe_query(empty)
            for field in sorted(provider.capabilities.filterable_fields):
                probe = _PROBES.get(field)
                if probe is None:
                    failures.append(
                        f"{pid} ({source}): declares filterable field "
                        f"{field!r} that MaterialSearchQuery cannot express -- "
                        "nothing can ever exercise this capability"
                    )
                    continue
                if provider.describe_query(probe) == baseline:
                    failures.append(
                        f"{pid} ({source}): declares {field!r} filterable but "
                        "its translator drops it -- the capability gate would "
                        "pass the query and the provider would return "
                        "unfiltered results as success"
                    )
    assert not failures, "\n".join(failures)
