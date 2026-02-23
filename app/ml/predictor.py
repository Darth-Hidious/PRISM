"""Prediction engine: featurize formula and predict with trained model."""
import numpy as np
from typing import Dict, Optional

from app.ml.features import composition_features
from app.ml.registry import ModelRegistry


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
            return {"error": f"No trained model for {property_name}/{algorithm}. Run 'prism model train' first."}

        features = composition_features(formula)
        if not features:
            return {"error": f"Could not generate features for formula: {formula}"}

        feature_names = sorted(features.keys())
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
