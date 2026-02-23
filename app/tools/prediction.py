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


def _list_models(**kwargs) -> dict:
    try:
        from app.ml.registry import ModelRegistry
        registry = ModelRegistry()
        return {"models": registry.list_models()}
    except Exception as e:
        return {"error": str(e)}


def create_prediction_tools(registry: ToolRegistry) -> None:
    """Register prediction tools."""
    registry.register(Tool(
        name="predict_property",
        description="Predict a material property from its chemical formula using trained ML models. Available properties depend on trained models (check with list_models first).",
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
        name="list_models",
        description="List all trained ML models and their metrics.",
        input_schema={"type": "object", "properties": {}},
        func=_list_models,
    ))
