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
# model_train — train a composition→property regressor the agent can use
# ---------------------------------------------------------------------------

# Wildcard formula families used to pull a chemically diverse training set
# from Materials Project when no local dataset is given. Each '*' is one
# element; the MP summary endpoint returns up to ~100 rows per pattern.
_DEFAULT_TRAIN_QUERIES = [
    "*O2", "*2O3", "*O", "*2O", "*3O4",
    "*N", "*3N4", "*C", "*S", "*2S3",
    "*F3", "*Cl3", "*Si", "*2Si", "*B2",
]

# Friendly aliases → Materials Project summary field names.
_PROPERTY_ALIASES = {
    "formation_energy": "formation_energy_per_atom",
    "e_form": "formation_energy_per_atom",
    "bandgap": "band_gap",
}

_KNOWN_ALGORITHMS = ["random_forest", "gradient_boosting", "linear", "xgboost", "lightgbm"]


def _gather_mp_training_rows(property_name: str, patterns: list, max_samples: int):
    """Pull (formula, value) pairs via the existing query_materials_project
    path (local MP_API_KEY → MARC27 platform proxy). Returns (rows, error)."""
    from app.tools.data import _query_materials_project

    seen: dict = {}
    last_error = None
    for pattern in patterns:
        res = _query_materials_project(
            formula=pattern,
            properties=["material_id", "formula_pretty", property_name],
        )
        if "error" in res:
            last_error = res["error"]
            continue
        for entry in res.get("results", []):
            formula = entry.get("formula_pretty")
            value = entry.get(property_name)
            if formula and isinstance(value, (int, float)) and formula not in seen:
                seen[formula] = float(value)
        if len(seen) >= max_samples:
            break
    return list(seen.items())[:max_samples], last_error


def _gather_dataset_training_rows(kw: dict, property_name: str):
    """Pull (formula, value) pairs from a DataStore dataset. Returns
    (rows, error)."""
    from app.tools.data_collectors.store import DataStore

    name = kw["dataset_name"]
    formula_col = kw.get("formula_column", "formula")
    target_col = kw.get("target_column", property_name)
    try:
        df = DataStore().load(name)
    except Exception as e:
        return [], f"could not load dataset '{name}': {e} — import it first with dataset(action='import')"
    for col in (formula_col, target_col):
        if col not in df.columns:
            return [], (
                f"dataset '{name}' has no column '{col}'. Available: "
                f"{list(df.columns)}. Pass formula_column/target_column."
            )
    sub = df[[formula_col, target_col]].dropna()
    return [(str(f), float(v)) for f, v in sub.itertuples(index=False)], None


def _model_train(**kw) -> dict:
    property_name = kw.get("property_name")
    if not property_name:
        return {"error": "model_train requires `property_name` (e.g. 'band_gap')"}
    property_name = _PROPERTY_ALIASES.get(property_name, property_name)
    algorithm = kw.get("algorithm", "random_forest")
    try:
        max_samples = max(20, min(int(kw.get("max_samples", 400)), 2000))
    except (TypeError, ValueError):
        return {"error": f"max_samples must be an integer, got {kw.get('max_samples')!r}"}

    try:
        from app.tools.ml.trainer import AVAILABLE_ALGORITHMS, train_model
        if algorithm not in AVAILABLE_ALGORITHMS:
            return {
                "error": f"Unknown algorithm '{algorithm}'. "
                f"Installed: {sorted(AVAILABLE_ALGORITHMS)}"
            }

        # 1. Training rows: local dataset takes precedence, else MP fetch.
        if kw.get("dataset_name"):
            rows, fetch_error = _gather_dataset_training_rows(kw, property_name)
            source = f"dataset:{kw['dataset_name']}"
        else:
            patterns = kw.get("formula_queries") or _DEFAULT_TRAIN_QUERIES
            rows, fetch_error = _gather_mp_training_rows(
                property_name, patterns, max_samples
            )
            source = "materials_project"
        if fetch_error and not rows:
            return {
                "error": f"No training data available: {fetch_error}",
                "hint": (
                    "Either set MP_API_KEY / run `prism login` for Materials "
                    "Project access, or import a local CSV with "
                    "dataset(action='import') and pass dataset_name."
                ),
            }
        if len(rows) < 20:
            return {
                "error": (
                    f"Only {len(rows)} usable (formula, {property_name}) rows — "
                    "need at least 20 to train anything meaningful. "
                    "Check the property name is a real MP summary field "
                    "(band_gap, formation_energy_per_atom, energy_above_hull, "
                    "density, volume) or pick a larger dataset."
                )
            }
        rows = rows[:max_samples]

        # 2. Featurize (matminer Magpie when installed, basic fallback else).
        import numpy as np
        from app.tools.ml.features import composition_features, get_feature_backend

        feature_names = None
        X_rows, y, skipped = [], [], 0
        for formula, value in rows:
            feats = composition_features(formula)
            if not feats:
                skipped += 1
                continue
            if feature_names is None:
                feature_names = sorted(feats.keys())
            try:
                X_rows.append([feats[k] for k in feature_names])
            except KeyError:
                skipped += 1
                continue
            y.append(value)
        if len(X_rows) < 20:
            return {
                "error": (
                    f"Featurization left only {len(X_rows)} rows "
                    f"({skipped} skipped) — not enough to train."
                )
            }

        # 3. Train + persist (feature order saved with the model so
        # predict-time featurization can't drift).
        result = train_model(
            np.array(X_rows), np.array(y),
            algorithm=algorithm, property_name=property_name,
        )
        from app.tools.ml.registry import ModelRegistry
        model_path = ModelRegistry().save_model(
            result["model"], property_name, algorithm,
            result["metrics"], feature_names=feature_names,
        )
        return {
            "trained": True,
            "property": property_name,
            "algorithm": algorithm,
            "metrics": result["metrics"],
            "n_samples": len(X_rows),
            "n_skipped": skipped,
            "n_features": len(feature_names or []),
            "feature_backend": get_feature_backend(),
            "source": source,
            "model_path": str(model_path),
            "next": f"predict(target='formula', formula='...', property_name='{property_name}', algorithm='{algorithm}')",
        }
    except Exception as e:
        return {"error": f"{type(e).__name__}: {e}"}


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


_MODEL_TRAIN_DESCRIPTION = (
    "Train a composition→property ML regressor so predict(target='formula') "
    "works. Use this when predict says 'No trained model'. Fetches a "
    "chemically diverse training set from Materials Project (needs MP_API_KEY "
    "or `prism login`; a few hundred samples, ~1 min) OR trains from a local "
    "DataStore dataset when `dataset_name` is given. Features: matminer "
    "Magpie (132) when installed. Saves the model to ~/.prism/ml_models for "
    "all future sessions; returns holdout MAE/RMSE/R². Local CPU only — no "
    "compute cost. NOT for GNN/structure models (those are pre-trained; see "
    "list_models) and NOT for dataset-wide prediction (predict_properties)."
)

_MODEL_TRAIN_SCHEMA = {
    "type": "object",
    "properties": {
        "property_name": {
            "type": "string",
            "description": (
                "Target property. For Materials Project sourcing use a real "
                "MP summary field: band_gap, formation_energy_per_atom, "
                "energy_above_hull, density, volume (aliases: "
                "formation_energy, bandgap). For dataset sourcing, any "
                "numeric column name."
            ),
        },
        "algorithm": {
            "type": "string",
            "enum": _KNOWN_ALGORITHMS,
            "default": "random_forest",
            "description": (
                "Regressor to train. xgboost/lightgbm only work if those "
                "packages are installed; random_forest always works."
            ),
        },
        "dataset_name": {
            "type": "string",
            "description": (
                "Train from this DataStore dataset instead of Materials "
                "Project. Import first via dataset(action='import')."
            ),
        },
        "formula_column": {
            "type": "string",
            "default": "formula",
            "description": "Column holding chemical formulas (dataset mode).",
        },
        "target_column": {
            "type": "string",
            "description": (
                "Column holding the target values (dataset mode). "
                "Defaults to property_name."
            ),
        },
        "formula_queries": {
            "type": "array",
            "items": {"type": "string"},
            "description": (
                "Optional MP wildcard formula patterns to build the training "
                "set from (e.g. ['*2O3', '*N']). Each '*' is one element. "
                "Defaults to a diverse 15-family mix."
            ),
        },
        "max_samples": {
            "type": "integer",
            "minimum": 20,
            "maximum": 2000,
            "default": 400,
            "description": "Cap on training samples (larger = slower, better).",
        },
    },
    "required": ["property_name"],
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
        name="model_train",
        description=_MODEL_TRAIN_DESCRIPTION,
        input_schema=_MODEL_TRAIN_SCHEMA,
        func=_model_train,
    ))
    registry.register(Tool(
        name="list_models",
        description=_LIST_MODELS_DESCRIPTION,
        input_schema={"type": "object", "properties": {}},
        func=_list_models,
    ))
