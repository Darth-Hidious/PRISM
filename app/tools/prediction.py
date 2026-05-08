"""ML prediction tools.

The unified `predict` tool replaces the prior `predict_property` (formula-
based, classical ML) and `predict_structure` (crystal-structure GNN) tools
with a single entry point dispatched by `target`. `list_models` remains a
separate tool — it's a different abstraction (registry inspection) that
doesn't fit the predict dispatcher.

The dataset-wide `predict_properties` Skill is registered through a
different code path (SkillRegistry.register_all_as_tools) and is not
collapsed here — it's a workflow-shaped abstraction that auto-trains
models on demand, distinct from the atomic predictors.
"""
from app.tools.base import Tool, ToolRegistry


# ---------------------------------------------------------------------------
# Per-target handlers
# ---------------------------------------------------------------------------

def _predict_formula(**kw) -> dict:
    """Composition-based ML prediction (matminer Magpie features + sklearn)."""
    formula = kw.get("formula")
    if not formula:
        return {"error": "Action target='formula' requires `formula`"}
    try:
        from app.tools.ml.predictor import Predictor
        predictor = Predictor()
        return predictor.predict(
            formula,
            kw.get("property_name", "band_gap"),
            kw.get("algorithm", "random_forest"),
        )
    except Exception as e:
        return {"error": str(e)}


def _predict_structure(**kw) -> dict:
    """Crystal-structure prediction via pre-trained GNN (M3GNet, MEGNet)."""
    structure = kw.get("structure")
    if not structure:
        return {"error": "Action target='structure' requires `structure` dict"}
    try:
        from app.tools.ml.pretrained import predict_with_pretrained
        return predict_with_pretrained(
            model_name=kw.get("model", "m3gnet-eform"),
            structure_data=structure,
        )
    except Exception as e:
        return {"error": str(e)}


_DISPATCH = {
    "formula":   _predict_formula,
    "structure": _predict_structure,
}


def _predict(**kwargs) -> dict:
    target = kwargs.pop("target", None)
    if not target:
        return {
            "error": f"Missing 'target'. Valid: {list(_DISPATCH.keys())}",
            "hint": (
                "predict(target='formula', formula='LiCoO2') or "
                "predict(target='structure', structure={...})"
            ),
        }
    handler = _DISPATCH.get(target)
    if not handler:
        return {"error": f"Unknown target '{target}'. Valid: {list(_DISPATCH.keys())}"}
    try:
        return handler(**kwargs)
    except Exception as e:
        return {"error": str(e), "target": target}


# ---------------------------------------------------------------------------
# list_models — separate tool (different abstraction, not part of predict)
# ---------------------------------------------------------------------------

def _list_models(**kwargs) -> dict:
    try:
        from app.tools.ml.registry import ModelRegistry
        registry = ModelRegistry()
        trained = registry.list_models()

        from app.tools.ml.pretrained import list_pretrained_models
        pretrained = list_pretrained_models()

        from app.tools.ml.features import get_feature_backend
        backend = get_feature_backend()

        return {
            "trained_models": trained,
            "pretrained_models": pretrained,
            "feature_backend": backend,
        }
    except Exception as e:
        return {"error": str(e)}


# ---------------------------------------------------------------------------
# Tool descriptions
# ---------------------------------------------------------------------------

_PREDICT_DESCRIPTION = (
    "Predict a material property using ML. ONE tool, two targets:\n"
    "  • target='formula' — composition-based ML (matminer Magpie features + "
    "trained scikit-learn model). Requires `formula` (e.g. 'LiCoO2'). "
    "Optional: `property_name` (band_gap, formation_energy, ...), "
    "`algorithm` (random_forest, xgboost, ...). Use list_models first to "
    "see what's trained.\n"
    "  • target='structure' — pre-trained GNN (M3GNet, MEGNet); no training "
    "needed. Requires `structure` dict with `lattice` (3×3), `species` "
    "(list), `coords` (list). Optional: `model` (default 'm3gnet-eform').\n"
    "Use target='formula' when you only know the chemistry; use target='structure' "
    "when you have actual atomic coordinates. NOT for batch dataset prediction "
    "(use the predict_properties skill) and NOT for property selection "
    "(use list_predictable_properties)."
)

_PREDICT_SCHEMA = {
    "type": "object",
    "properties": {
        "target": {
            "type": "string",
            "enum": list(_DISPATCH.keys()),
            "description": (
                "Whether to predict from a chemical formula or from a "
                "crystal structure."
            ),
        },
        "formula": {
            "type": "string",
            "description": (
                "Chemical formula (e.g. 'LiCoO2'). Required for "
                "target='formula'."
            ),
        },
        "property_name": {
            "type": "string",
            "description": (
                "Property to predict for target='formula' (band_gap, "
                "formation_energy, ...)."
            ),
        },
        "algorithm": {
            "type": "string",
            "description": (
                "ML algorithm for target='formula' (random_forest, "
                "xgboost, ...)."
            ),
        },
        "structure": {
            "type": "object",
            "description": (
                "Crystal structure for target='structure'. Keys: `lattice` "
                "(3×3 Angstrom matrix), `species` (element symbols), "
                "`coords` (fractional or Cartesian). Optional `cartesian: "
                "true` if coords are Cartesian."
            ),
            "properties": {
                "lattice":   {"type": "array"},
                "species":   {"type": "array", "items": {"type": "string"}},
                "coords":    {"type": "array"},
                "cartesian": {"type": "boolean"},
            },
        },
        "model": {
            "type": "string",
            "description": (
                "Pre-trained GNN model for target='structure': "
                "m3gnet-eform, megnet-eform, megnet-bandgap."
            ),
        },
    },
    "required": ["target"],
    "additionalProperties": False,
}


_LIST_MODELS_DESCRIPTION = (
    "ML property-prediction models — trained composition models + "
    "pre-trained GNN models (M3GNet, MEGNet) for materials property "
    "prediction. Use this to choose a model BEFORE calling predict. "
    "NOT for hosted chat LLMs (use models_list for those) and NOT for "
    "compute hardware (use compute(action='list_gpus') for that)."
)


# ---------------------------------------------------------------------------
# Registration
# ---------------------------------------------------------------------------

def create_prediction_tools(registry: ToolRegistry) -> None:
    """Register the unified `predict` tool and `list_models`.

    Replaces the prior `predict_property` + `predict_structure` tools
    with a single `predict(target=...)` dispatcher. `list_models` stays
    separate — it's a registry-inspection tool, not a property predictor.
    """
    registry.register(Tool(
        name="predict",
        description=_PREDICT_DESCRIPTION,
        input_schema=_PREDICT_SCHEMA,
        func=_predict,
    ))
    registry.register(Tool(
        name="list_models",
        description=_LIST_MODELS_DESCRIPTION,
        input_schema={"type": "object", "properties": {}},
        func=_list_models,
    ))
