"""Cross-provider material fusion -- merge, dedup, rank."""
from __future__ import annotations

from collections import defaultdict

from app.search.result import Material, PropertyValue


def _fusion_key(m: Material) -> str:
    """Identity key for grouping: normalized formula + space group."""
    sg = m.space_group.value if m.space_group else "unknown"
    return f"{m.formula}::{sg}"


def _merge_property(
    existing: PropertyValue | None,
    incoming: PropertyValue | None,
    property_name: str,
    extra: dict[str, PropertyValue],
    incoming_provider: str,
) -> PropertyValue | None:
    """Merge a single property. First value wins primary slot; conflicts go to extra."""
    if incoming is None:
        return existing
    if existing is None:
        return incoming
    # Conflict -- existing wins primary, incoming goes to extra
    extra[f"{property_name}:{incoming_provider}"] = incoming
    return existing


def fuse_materials(materials: list[Material]) -> list[Material]:
    """Group by identity, merge properties across providers."""
    if not materials:
        return []

    groups: dict[str, list[Material]] = defaultdict(list)
    for m in materials:
        groups[_fusion_key(m)].append(m)

    fused = []
    for key, group in groups.items():
        if len(group) == 1:
            fused.append(group[0])
            continue

        # Merge: first material is the base
        base = group[0].model_copy(deep=True)
        merged_sources = list(base.sources)
        merged_extra = dict(base.extra_properties)

        for other in group[1:]:
            for src in other.sources:
                if src not in merged_sources:
                    merged_sources.append(src)

            # Merge standard properties
            for prop_name in ("space_group", "band_gap", "formation_energy",
                              "energy_above_hull", "bulk_modulus", "debye_temperature",
                              "lattice_vectors", "crystal_system"):
                existing_val = getattr(base, prop_name)
                incoming_val = getattr(other, prop_name)
                merged_val = _merge_property(
                    existing_val, incoming_val, prop_name, merged_extra,
                    other.sources[0] if other.sources else "unknown",
                )
                setattr(base, prop_name, merged_val)

            # Merge extra_properties
            for k, v in other.extra_properties.items():
                if k not in merged_extra:
                    merged_extra[k] = v

        base.sources = merged_sources
        base.extra_properties = merged_extra
        fused.append(base)

    return fused
