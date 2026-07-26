"""Prediction engine: featurize formula and predict with trained model."""
import numpy as np
from typing import Dict, Optional

from app.tools import _provenance as prov
from app.tools.ml.features import composition_features, feature_backend_id
from app.tools.ml.registry import ModelRegistry

#: Units for the Materials Project summary fields model_train can source.
#: A property NOT in this map gets "unknown" — a locally trained model's
#: target unit is whatever the training column was, and guessing it would be
#: worse than saying so.
MP_PROPERTY_UNITS = {
    "band_gap": "eV",
    "formation_energy_per_atom": "eV/atom",
    "energy_above_hull": "eV/atom",
    "density": "g/cm^3",
    "volume": "Angstrom^3",
    "efermi": "eV",
    "total_magnetization": "muB",
}


def property_unit(property_name: str) -> str:
    """Unit for a predicted property, or an explicit 'unknown'."""
    return MP_PROPERTY_UNITS.get(property_name, "unknown")


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
        # Same feature NAMES can carry different NUMBERS after a featurizer
        # change. Refuse rather than return a plausible-looking wrong value.
        trained_backend = meta.get("feature_backend_id")
        current_backend = feature_backend_id()
        if trained_backend and trained_backend != current_backend:
            return {
                "error": (
                    f"Feature backend changed since training: model was built "
                    f"with {trained_backend!r}, this process computes "
                    f"{current_backend!r}. The feature names match but the "
                    "values do not mean the same thing. Retrain with "
                    f"model_train(property_name={property_name!r}, "
                    f"algorithm={algorithm!r})."
                )
            }
        X = np.array([[features[k] for k in feature_names]])

        try:
            prediction = float(model.predict(X)[0])
        except Exception as e:
            return {"error": f"Prediction failed: {e}"}

        model_path = self.registry.models_dir / f"{property_name}_{algorithm}.joblib"
        unit = property_unit(property_name)
        result = {
            "prediction": prediction,
            "unit": unit,
            "formula": formula,
            "property": property_name,
            "algorithm": algorithm,
            "n_features": len(feature_names),
        }
        return prov.attach(result, prov.build(
            tool_name="predict",
            engine="sklearn",
            engine_version=prov.versions_of("sklearn").get("sklearn", "absent"),
            activity=f"sklearn.{algorithm}.predict",
            inputs={
                "formula": formula,
                "property_name": property_name,
                "algorithm": algorithm,
                "feature_backend_id": current_backend,
                "feature_names": feature_names,
            },
            units={"prediction": unit},
            derived_from=[
                prov.file_ref(model_path, role="trained_model"),
                {
                    "role": "training_run",
                    "trained_at": meta.get("saved_at", "unknown"),
                    "holdout_metrics": meta.get("metrics", "unknown"),
                    "feature_backend_id": trained_backend or "unrecorded",
                },
            ],
            reproduce=(
                f"predict(target='formula', formula={formula!r}, "
                f"property_name={property_name!r}, algorithm={algorithm!r})"
            ),
        ))
