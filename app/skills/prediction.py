"""Property prediction skill: predict properties for a dataset."""

import numpy as np

from app.config.preferences import UserPreferences
from app.skills.base import Skill, SkillStep


def _predict_properties(**kwargs) -> dict:
    """Load dataset, train if needed, predict properties, save back."""
    dataset_name = kwargs["dataset_name"]
    properties = kwargs.get("properties")
    algorithm = kwargs.get("algorithm")
    train_if_missing = kwargs.get("train_if_missing", True)

    prefs = UserPreferences.load()
    algorithm = algorithm or prefs.default_algorithm

    from app.data.store import DataStore
    from app.ml.features import composition_features
    from app.ml.registry import ModelRegistry

    store = DataStore()
    try:
        df = store.load(dataset_name)
    except FileNotFoundError:
        return {"error": f"Dataset '{dataset_name}' not found"}

    # Determine which columns to predict
    if properties:
        target_cols = [p for p in properties if p in df.columns]
    else:
        # Auto-detect numeric columns (exclude metadata-like columns)
        exclude = {"source_id", "provider", "formula", "elements", "space_group",
                   "material_id", "formula_pretty", "is_metal"}
        target_cols = [
            c for c in df.columns
            if df[c].dtype in ("float64", "float32", "int64", "int32")
            and c not in exclude
        ]

    if not target_cols:
        return {"error": "No numeric property columns found to predict"}

    # Check we have formula column
    formula_col = None
    for candidate in ("formula", "formula_pretty"):
        if candidate in df.columns:
            formula_col = candidate
            break
    if not formula_col:
        return {"error": "No formula column found in dataset"}

    registry = ModelRegistry()
    predictions_made = {}

    for prop in target_cols:
        model = registry.load_model(prop, algorithm)

        # Train if missing
        if model is None and train_if_missing:
            valid = df[[formula_col, prop]].dropna()
            if len(valid) < 5:
                continue

            feature_rows = []
            targets = []
            for _, row in valid.iterrows():
                feats = composition_features(str(row[formula_col]))
                if feats:
                    feature_rows.append(feats)
                    targets.append(row[prop])

            if len(feature_rows) < 5:
                continue

            feature_names = sorted(feature_rows[0].keys())
            X = np.array([[f[k] for k in feature_names] for f in feature_rows])
            y = np.array(targets)

            from app.ml.trainer import train_model

            result = train_model(X, y, algorithm=algorithm, property_name=prop)
            model = result["model"]
            registry.save_model(model, prop, algorithm, result["metrics"])

        if model is None:
            continue

        # Predict for all rows
        pred_col = f"predicted_{prop}"
        preds = []
        for formula in df[formula_col]:
            feats = composition_features(str(formula))
            if feats:
                feature_names = sorted(feats.keys())
                X = np.array([[feats[k] for k in feature_names]])
                try:
                    preds.append(float(model.predict(X)[0]))
                except Exception:
                    preds.append(None)
            else:
                preds.append(None)

        df[pred_col] = preds
        predictions_made[prop] = pred_col

    if not predictions_made:
        return {"error": "No predictions could be made (insufficient data or features)"}

    # Save updated dataset
    store.save(df, dataset_name)

    return {
        "dataset_name": dataset_name,
        "predictions": predictions_made,
        "algorithm": algorithm,
        "rows": len(df),
    }


PREDICT_SKILL = Skill(
    name="predict_properties",
    description=(
        "Predict material properties for an existing dataset. "
        "Automatically trains models if none exist, generates "
        "composition features, and appends predicted_<property> columns."
    ),
    steps=[
        SkillStep("load_dataset", "Load dataset from DataStore", "internal"),
        SkillStep("check_models", "Check ModelRegistry for trained models", "internal"),
        SkillStep(
            "train",
            "Train models for missing properties",
            "internal",
            optional=True,
        ),
        SkillStep("predict", "Run predictions for each formula", "internal"),
        SkillStep("save", "Save updated dataset", "internal"),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "dataset_name": {
                "type": "string",
                "description": "Name of the dataset in DataStore",
            },
            "properties": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Properties to predict (auto-detected if omitted)",
            },
            "algorithm": {
                "type": "string",
                "description": "ML algorithm (default from preferences)",
            },
            "train_if_missing": {
                "type": "boolean",
                "description": "Auto-train models if not found (default: true)",
            },
        },
        "required": ["dataset_name"],
    },
    func=_predict_properties,
    category="prediction",
)
