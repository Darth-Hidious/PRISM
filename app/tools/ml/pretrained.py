"""Pre-trained GNN model wrappers (matgl, CHGNet, MACE).

These models predict material properties from crystal structures with
zero training — weights are shipped with the package.

Only matgl is bundled; CHGNet and MACE are plugin-installable.
"""
from typing import Any, Dict, List, Optional


def check_matgl_available() -> bool:
    try:
        import matgl  # noqa: F401
        return True
    except ImportError:
        return False


# ---------------------------------------------------------------------------
# Pre-trained model catalog
# ---------------------------------------------------------------------------

PRETRAINED_MODELS: Dict[str, dict] = {
    "m3gnet-eform": {
        "package": "matgl",
        "model_id": "M3GNet-MP-2018.6.1-Eform",
        "property": "formation_energy",
        "unit": "eV/atom",
        "description": "M3GNet formation energy (Materials Project, pre-trained)",
        "requires_structure": True,
    },
    "megnet-eform": {
        "package": "matgl",
        "model_id": "MEGNet-MP-2018.6.1-Eform",
        "property": "formation_energy",
        "unit": "eV/atom",
        "description": "MEGNet formation energy (Materials Project, pre-trained)",
        "requires_structure": True,
    },
    "megnet-bandgap": {
        "package": "matgl",
        "model_id": "MEGNet-MP-2019.4.1-BandGap-mfi",
        "property": "band_gap",
        "unit": "eV",
        "description": "MEGNet multi-fidelity band gap (Materials Project, pre-trained)",
        "requires_structure": True,
    },
}


def list_pretrained_models() -> List[dict]:
    """List available pre-trained models and their install status."""
    results = []
    for model_name, info in PRETRAINED_MODELS.items():
        installed = False
        if info["package"] == "matgl":
            installed = check_matgl_available()
        results.append({
            "name": model_name,
            "property": info["property"],
            "unit": info["unit"],
            "description": info["description"],
            "installed": installed,
            "package": info["package"],
        })
    return results


def _structure_from_dict(structure_data: dict) -> Any:
    """Convert a dict with lattice/species/coords to a pymatgen Structure."""
    from pymatgen.core import Structure, Lattice

    lattice = structure_data.get("lattice")
    species = structure_data.get("species")
    coords = structure_data.get("coords")
    coords_are_cartesian = structure_data.get("cartesian", False)

    if not all([lattice, species, coords]):
        raise ValueError("structure_data must have 'lattice', 'species', and 'coords'")

    return Structure(
        Lattice(lattice),
        species,
        coords,
        coords_are_cartesian=coords_are_cartesian,
    )


def predict_with_pretrained(
    model_name: str,
    structure: Optional[Any] = None,
    structure_data: Optional[dict] = None,
) -> dict:
    """Predict a property using a pre-trained GNN model.

    Args:
        model_name: Key from PRETRAINED_MODELS (e.g. "m3gnet-eform")
        structure: A pymatgen Structure object (if already available)
        structure_data: Dict with lattice/species/coords (converted to Structure)

    Returns:
        dict with prediction, property, unit, model
    """
    if model_name not in PRETRAINED_MODELS:
        available = list(PRETRAINED_MODELS.keys())
        return {"error": f"Unknown model: {model_name}. Available: {available}"}

    info = PRETRAINED_MODELS[model_name]

    # Resolve structure
    if structure is None and structure_data is not None:
        try:
            structure = _structure_from_dict(structure_data)
        except Exception as e:
            return {"error": f"Failed to build structure: {e}"}

    if structure is None:
        return {"error": "Provide either 'structure' (pymatgen) or 'structure_data' (dict)"}

    # Load and run model
    package = info["package"]

    if package == "matgl":
        if not check_matgl_available():
            return {"error": "matgl not installed. Install with: pip install matgl"}
        try:
            import matgl
            model = matgl.load_model(info["model_id"])
            prediction = model.predict_structure(structure)
            # matgl returns a tensor or float
            value = float(prediction)
            return {
                "prediction": value,
                "property": info["property"],
                "unit": info["unit"],
                "model": model_name,
                "model_id": info["model_id"],
            }
        except Exception as e:
            return {"error": f"Prediction failed: {e}"}

    return {"error": f"Unsupported package: {package}"}
