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
            result = {
                "prediction": value,
                "property": info["property"],
                "unit": info["unit"],
                "model": model_name,
                "model_id": info["model_id"],
            }
            import hashlib
            import json as _json

            from app.tools import _provenance as prov

            # Lattice + reduced formula does NOT identify a structure —
            # polymorphs and different site orderings share both, so the
            # bundle would under-determine the very input the number came
            # from. Hash the full structure and carry the exact call.
            struct_dict = structure.as_dict()
            struct_json = _json.dumps(struct_dict, sort_keys=True, default=str)
            struct_sha = hashlib.sha256(struct_json.encode()).hexdigest()
            explicit = {
                "lattice": [list(r) for r in structure.lattice.matrix],
                "species": [str(s) for s in structure.species],
                "coords": [list(s.frac_coords) for s in structure],
            }
            return prov.attach(result, prov.build(
                tool_name="predict",
                engine="matgl",
                engine_version=prov.versions_of("matgl").get("matgl", "absent"),
                activity=f"matgl.{info['model_id']}.predict_structure",
                inputs={
                    "model": model_name,
                    "formula": structure.composition.reduced_formula,
                    "n_sites": len(structure),
                    "lattice_abc_Angstrom": list(structure.lattice.abc),
                    "lattice_angles_deg": list(structure.lattice.angles),
                    "structure_sha256": struct_sha,
                    "structure": explicit,
                },
                units={"prediction": info["unit"]},
                derived_from=[{
                    "role": "input_structure",
                    "sha256": struct_sha,
                    "formula": structure.composition.reduced_formula,
                    "n_sites": len(structure),
                }, {
                    "role": "pretrained_model",
                    "model_id": info["model_id"],
                    "training_set": "Materials Project (see matgl model card)",
                }],
                # A runnable call, not a placeholder: `structure` is carried
                # verbatim in `input.structure` above.
                reproduce=(
                    f"predict(target='structure', model={model_name!r}, "
                    f"structure=<provenance.input.structure>)  "
                    f"# structure sha256 {struct_sha[:16]}"
                ),
            ))
        except Exception as e:
            return {"error": f"Prediction failed: {e}"}

    return {"error": f"Unsupported package: {package}"}
