"""Free first-class materials-informatics and gated domain tools."""
from __future__ import annotations

import logging

from app.tools.materials.screening import (
    create_materials_informatics_tools as _create_materials_informatics_tools,
)

logger = logging.getLogger(__name__)


def create_materials_informatics_tools(registry) -> None:
    """Register core tools and only scientifically available domain plugins."""
    _create_materials_informatics_tools(registry)

    # Polymer identities use RDKit for real SMILES validation. Mirror the QE
    # gated-registration contract: absent dependency means absent tool, never a
    # catalog entry that fails every call.
    try:
        from app.tools.materials.polymer import check_polymer_available

        if check_polymer_available():
            from app.tools.materials.polymer.tools import create_polymer_tools

            create_polymer_tools(registry)
    except Exception:
        logger.debug("polymer tools not registered", exc_info=True)


__all__ = ["create_materials_informatics_tools"]
