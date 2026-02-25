"""Prediction tools for the agent."""
from app.tools.base import Tool, ToolRegistry


def _predict_property(**kwargs) -> dict:
    formula = kwargs["formula"]
    property_name = kwargs.get("property_name", "band_gap")
    algorithm = kwargs.get("algorithm", "random_forest")

    try:
        from app.ml.predictor import Predictor
        predictor = Predictor()
        return predictor.predict(formula, property_name, algorithm)
    except Exception as e:
        return {"error": str(e)}


def _predict_structure(**kwargs) -> dict:
    """Predict a property from a crystal structure using a pre-trained GNN."""
    model_name = kwargs.get("model", "m3gnet-eform")
    structure_data = kwargs.get("structure")

    try:
        from app.ml.pretrained import predict_with_pretrained
        return predict_with_pretrained(
            model_name=model_name,
            structure_data=structure_data,
        )
    except Exception as e:
        return {"error": str(e)}


def _list_models(**kwargs) -> dict:
    try:
        from app.ml.registry import ModelRegistry
        registry = ModelRegistry()
        trained = registry.list_models()

        from app.ml.pretrained import list_pretrained_models
        pretrained = list_pretrained_models()

        from app.ml.features import get_feature_backend
        backend = get_feature_backend()

        return {
            "trained_models": trained,
            "pretrained_models": pretrained,
            "feature_backend": backend,
        }
    except Exception as e:
        return {"error": str(e)}


def create_prediction_tools(registry: ToolRegistry) -> None:
    """Register prediction tools."""
    registry.register(Tool(
        name="predict_property",
        description=(
            "Predict a material property from its chemical formula using trained "
            "ML models (composition-based). Available properties depend on trained "
            "models — check with list_models first. Uses matminer Magpie features "
            "if installed, otherwise built-in features."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "formula": {"type": "string", "description": "Chemical formula, e.g. 'LiCoO2'"},
                "property_name": {"type": "string", "description": "Property to predict (band_gap, formation_energy, etc.)"},
                "algorithm": {"type": "string", "description": "Algorithm to use (random_forest, xgboost, etc.)"},
            },
            "required": ["formula"],
        },
        func=_predict_property,
    ))

    registry.register(Tool(
        name="predict_structure",
        description=(
            "Predict a material property from its crystal structure using a "
            "pre-trained GNN model (M3GNet, MEGNet). No training needed — "
            "models ship with the package. Requires lattice vectors, species, "
            "and atomic coordinates."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "model": {
                    "type": "string",
                    "description": "Pre-trained model: m3gnet-eform, megnet-eform, megnet-bandgap",
                },
                "structure": {
                    "type": "object",
                    "description": (
                        "Crystal structure as dict with keys: "
                        "'lattice' (3x3 matrix), 'species' (list of element symbols), "
                        "'coords' (list of [x,y,z] fractional coordinates). "
                        "Set 'cartesian': true if coords are Cartesian (Angstroms)."
                    ),
                    "properties": {
                        "lattice": {"type": "array", "description": "3x3 lattice matrix in Angstroms"},
                        "species": {"type": "array", "items": {"type": "string"}, "description": "Element symbols"},
                        "coords": {"type": "array", "description": "Atomic coordinates (fractional by default)"},
                        "cartesian": {"type": "boolean", "description": "True if coords are Cartesian"},
                    },
                    "required": ["lattice", "species", "coords"],
                },
            },
            "required": ["structure"],
        },
        func=_predict_structure,
    ))

    registry.register(Tool(
        name="list_models",
        description=(
            "List all available ML models: trained composition models AND "
            "pre-trained GNN models (M3GNet, MEGNet). Also shows which feature "
            "backend is active (matminer or basic)."
        ),
        input_schema={"type": "object", "properties": {}},
        func=_list_models,
    ))
