"""Structured query model for materials search."""
from __future__ import annotations

import hashlib
import json
from typing import Literal

from pydantic import BaseModel, Field, field_validator, model_validator

# Periodic table symbols — all 118 elements
VALID_ELEMENTS = {
    "H", "He", "Li", "Be", "B", "C", "N", "O", "F", "Ne",
    "Na", "Mg", "Al", "Si", "P", "S", "Cl", "Ar", "K", "Ca",
    "Sc", "Ti", "V", "Cr", "Mn", "Fe", "Co", "Ni", "Cu", "Zn",
    "Ga", "Ge", "As", "Se", "Br", "Kr", "Rb", "Sr", "Y", "Zr",
    "Nb", "Mo", "Tc", "Ru", "Rh", "Pd", "Ag", "Cd", "In", "Sn",
    "Sb", "Te", "I", "Xe", "Cs", "Ba", "La", "Ce", "Pr", "Nd",
    "Pm", "Sm", "Eu", "Gd", "Tb", "Dy", "Ho", "Er", "Tm", "Yb",
    "Lu", "Hf", "Ta", "W", "Re", "Os", "Ir", "Pt", "Au", "Hg",
    "Tl", "Pb", "Bi", "Po", "At", "Rn", "Fr", "Ra", "Ac", "Th",
    "Pa", "U", "Np", "Pu", "Am", "Cm", "Bk", "Cf", "Es", "Fm",
    "Md", "No", "Lr", "Rf", "Db", "Sg", "Bh", "Hs", "Mt", "Ds",
    "Rg", "Cn", "Nh", "Fl", "Mc", "Lv", "Ts", "Og",
}


class PropertyRange(BaseModel):
    """Numeric range for property filtering."""
    min: float | None = None
    max: float | None = None

    @model_validator(mode="after")
    def min_lte_max(self):
        if self.min is not None and self.max is not None and self.min > self.max:
            raise ValueError(f"min ({self.min}) must be <= max ({self.max})")
        return self


class MaterialSearchQuery(BaseModel):
    """What to search for. Domain terms only — no provider-specific syntax."""

    # Composition
    elements: list[str] | None = None
    elements_any: list[str] | None = None
    exclude_elements: list[str] | None = None
    formula: str | None = None
    n_elements: PropertyRange | None = None

    # Properties
    band_gap: PropertyRange | None = None
    formation_energy: PropertyRange | None = None
    energy_above_hull: PropertyRange | None = None
    bulk_modulus: PropertyRange | None = None
    debye_temperature: PropertyRange | None = None

    # Structural
    space_group: str | int | None = None
    crystal_system: Literal[
        "cubic", "hexagonal", "tetragonal",
        "orthorhombic", "monoclinic", "triclinic", "trigonal",
    ] | None = None

    # Control
    providers: list[str] | None = None
    limit: int = Field(default=100, ge=1, le=10000)

    @field_validator("elements", "elements_any", "exclude_elements")
    @classmethod
    def validate_elements(cls, v):
        if v is None:
            return v
        for el in v:
            if el not in VALID_ELEMENTS:
                raise ValueError(f"Invalid element symbol: {el!r}")
        return v

    def query_hash(self) -> str:
        """Stable hash for cache keying."""
        data = self.model_dump(exclude_none=True, mode="json")
        raw = json.dumps(data, sort_keys=True)
        return hashlib.sha256(raw.encode()).hexdigest()[:16]
