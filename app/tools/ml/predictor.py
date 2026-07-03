"""Prediction engine: featurize formula and predict with trained model."""
import numpy as np
from typing import Dict, Optional

from app.tools.ml.features import composition_features
from app.tools.ml.registry import ModelRegistry


class Predictor:
    def __init__(self, registry: Optional[ModelRegistry] = None):
        self.registry = registry or ModelRegistry()

    def predict(
        self,
        formula: str,
        property_name: str,
        algorithm: str = "random_forest",
    ) -> Dict:
        model = self.registry.load_model(property_name, algorithm)
        if model is None:
            return {
                "error": (
                    f"No trained model for {property_name}/{algorithm}. "
                    "Train one with the model_train tool, e.g. "
                    f"model_train(property_name='{property_name}'), "
                    "or use predict(target='structure') for pre-trained GNN "
                    "predictions that need no training."
                )
            }

        features = composition_features(formula)
        if not features:
            return {"error": f"Could not generate features for formula: {formula}"}

        # Use the exact feature order the model was trained with when the
        # meta records it (models saved via model_train do). Falling back
        # to sorted() keeps pre-existing models working.
        meta = self.registry.load_meta(property_name, algorithm) or {}
        feature_names = meta.get("feature_names") or sorted(features.keys())
        missing = [k for k in feature_names if k not in features]
        if missing:
            return {
                "error": (
                    f"Feature backend mismatch: {len(missing)} training features "
                    f"missing at predict time (e.g. {missing[:3]}). The model was "
                    "likely trained with the matminer backend — install matminer "
                    "or retrain with model_train."
                )
            }
        X = np.array([[features[k] for k in feature_names]])

        try:
            prediction = float(model.predict(X)[0])
            return {
                "prediction": prediction,
                "formula": formula,
                "property": property_name,
                "algorithm": algorithm,
                "n_features": len(feature_names),
            }
        except Exception as e:
            return {"error": f"Prediction failed: {e}"}
