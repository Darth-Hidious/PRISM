"""Optional polymer electrical-insulation evaluator.

Registration is gated on :func:`check_polymer_available`: RDKit validates
SMILES and is deliberately mandatory for the plugin as a whole. If it is not
importable, bootstrap leaves ``polymer_insulation_properties`` out of the tool
catalog instead of exposing a registered-but-broken tool.
"""
from __future__ import annotations

RDKIT_INSTALL_HINT = (
    "Install RDKit in the PRISM Python environment with "
    "`python -m pip install rdkit`, then restart the PRISM node."
)


def check_polymer_available() -> bool:
    """Return True only when the chemistry dependency can really run."""
    try:
        from rdkit import Chem

        return Chem.MolFromSmiles("CC") is not None
    except Exception:
        return False


__all__ = ["RDKIT_INSTALL_HINT", "check_polymer_available"]
