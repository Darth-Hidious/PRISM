"""Property prediction skill: predict properties for a dataset."""

import numpy as np

from app.config.preferences import UserPreferences
from app.tools.skills.base import Skill, SkillStep


def _predict_properties(**kwargs) -> dict:
    """Load dataset, train if needed, predict properties, save back."""
    dataset_name = kwargs["dataset_name"]
    properties = kwargs.get("properties")
    algorithm = kwargs.get("algorithm")
    train_if_missing = kwargs.get("train_if_missing", True)

    prefs = UserPreferences.load()
    algorithm = algorithm or prefs.default_algorithm

    from app.tools import _provenance as prov
    from app.tools.data_collectors.store import DataStore
    from app.tools.ml.features import composition_features, feature_backend_id
    from app.tools.ml.predictor import (
        UNRECORDED_BACKEND,
        backend_mismatch_error,
        property_unit,
    )
    from app.tools.ml.registry import ModelRegistry

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
            # NEVER auto-target a column this skill wrote. The result is
            # saved back over the same dataset name, so a second call with
            # properties=None (what the discovery skill passes) would fit a
            # model to a previous model's guesses and call the output a
            # prediction. Explicitly naming a predicted_* column still works
            # — that is a deliberate choice, not an accident.
            and not c.startswith("predicted_")
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
    models_used = {}
    skipped_models: dict = {}
    current_backend = feature_backend_id()

    for prop in target_cols:
        model = registry.load_model(prop, algorithm)
        meta = registry.load_meta(prop, algorithm) or {}

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

            # Intersect, don't take row 0's keys: the basic backend drops a
            # whole property block for a formula whose elements it doesn't
            # know, so row 0's key list can KeyError on a later row.
            feature_names = sorted(set.intersection(*(set(f) for f in feature_rows)))
            if not feature_names:
                continue
            X = np.array([[f[k] for k in feature_names] for f in feature_rows])
            y = np.array(targets)

            from app.tools.ml.trainer import train_model

            result = train_model(X, y, algorithm=algorithm, property_name=prop)
            model = result["model"]
            # feature_names was missing here — without it Predictor falls back
            # to sorted() at predict time, which is exactly the drift
            # model_train records it to prevent.
            registry.save_model(
                model, prop, algorithm, result["metrics"],
                feature_names=feature_names,
            )
            meta = registry.load_meta(prop, algorithm) or {}

        if model is None:
            continue

        # Same gate as the single-formula predict(): a model whose features
        # were computed by a different featurizer scores to a plausible
        # wrong number, and here it would write a whole column of them.
        trained_backend = meta.get("feature_backend_id", UNRECORDED_BACKEND)
        if trained_backend != current_backend:
            skipped_models[prop] = backend_mismatch_error(
                trained_backend, current_backend, prop, algorithm)
            continue

        # ONE feature order for the whole column. The old code recomputed
        # sorted(feats.keys()) per row, so a formula whose feature set
        # differed was scored against a differently ordered vector. Prefer
        # the order recorded at training; fall back to the first row that
        # featurizes (what pre-existing models were effectively scored with).
        feature_names = meta.get("feature_names")
        if not feature_names:
            for formula in df[formula_col]:
                feats = composition_features(str(formula))
                if feats:
                    feature_names = sorted(feats.keys())
                    break
        if not feature_names:
            continue

        pred_col = f"predicted_{prop}"
        preds = []
        n_skipped = 0
        for formula in df[formula_col]:
            feats = composition_features(str(formula))
            if feats and all(k in feats for k in feature_names):
                X = np.array([[feats[k] for k in feature_names]])
                try:
                    preds.append(float(model.predict(X)[0]))
                except Exception:
                    preds.append(None)
                    n_skipped += 1
            else:
                preds.append(None)
                n_skipped += 1

        df[pred_col] = preds
        predictions_made[prop] = pred_col
        models_used[prop] = {
            "unit": property_unit(prop),
            "holdout_metrics": meta.get("metrics"),
            "feature_backend_id": meta.get("feature_backend_id", "unrecorded"),
            "trained_at": meta.get("saved_at"),
            "n_features": len(feature_names),
            "n_rows_unpredictable": n_skipped,
            "model_file": prov.file_ref(
                registry.models_dir / f"{prop}_{algorithm}.joblib",
                role="trained_model",
            ),
        }

    if not predictions_made:
        return {
            "error": "No predictions could be made (insufficient data or features)",
            **({"skipped_models": skipped_models} if skipped_models else {}),
        }

    # Save updated dataset
    store.save(df, dataset_name)

    result = {
        "dataset_name": dataset_name,
        "predictions": predictions_made,
        "models": models_used,
        "algorithm": algorithm,
        "rows": len(df),
        # These are IN-SAMPLE for every row that already carried a measured
        # value: the model was fitted on this dataset's own rows. Say so
        # rather than let predicted_* read like independent evidence.
        "in_sample_warning": (
            "predicted_* columns cover ALL rows, including the rows used to "
            "fit the model. Judge accuracy by each model's holdout_metrics, "
            "not by agreement with the measured column."
        ),
        # A property whose model could not be used honestly is named, not
        # quietly absent from `predictions`.
        **({"skipped_models": skipped_models} if skipped_models else {}),
    }
    return prov.attach(result, prov.build(
        tool_name="predict_properties",
        engine="sklearn",
        engine_version=prov.versions_of("sklearn").get("sklearn", "absent"),
        activity=f"sklearn.{algorithm}.predict (dataset-wide)",
        inputs={
            "dataset_name": dataset_name,
            "algorithm": algorithm,
            "formula_column": formula_col,
            "properties": list(predictions_made),
            "train_if_missing": train_if_missing,
        },
        units={col: models_used[p]["unit"] for p, col in predictions_made.items()},
        derived_from=[
            {"role": "input_dataset", "dataset_name": dataset_name, "rows": len(df)},
            *(m["model_file"] for m in models_used.values()),
        ],
        reproduce=(
            f"predict_properties(dataset_name={dataset_name!r}, "
            f"properties={list(predictions_made)!r}, algorithm={algorithm!r})"
        ),
    ))


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
        "additionalProperties": False,
    },
    func=_predict_properties,
    category="prediction",
    requires_approval=True,
)
