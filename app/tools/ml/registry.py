"""Model registry: save, load, and list trained models."""
import json
import os
from datetime import datetime
from pathlib import Path
from typing import Any, Dict, List, Optional

import joblib


def _default_models_dir() -> Path:
    """Stable, cwd-independent home for trained sklearn models.

    The old default was the relative ``Path("models")`` — models trained
    in one session silently vanished in the next because the cwd changed
    (and the repo's ./models holds LLM GGUFs, not sklearn artifacts).
    """
    override = os.environ.get("PRISM_ML_MODELS_DIR")
    if override:
        return Path(override)
    return Path.home() / ".prism" / "ml_models"


class ModelRegistry:
    def __init__(self, models_dir: Optional[str] = None):
        self.models_dir = Path(models_dir) if models_dir else _default_models_dir()
        self.models_dir.mkdir(parents=True, exist_ok=True)

    def save_model(
        self,
        model: Any,
        property_name: str,
        algorithm: str,
        metrics: Dict,
        feature_names: Optional[List[str]] = None,
    ) -> Path:
        filename = f"{property_name}_{algorithm}"
        model_path = self.models_dir / f"{filename}.joblib"
        meta_path = self.models_dir / f"{filename}.meta.json"

        joblib.dump(model, model_path)

        meta = {
            "property": property_name,
            "algorithm": algorithm,
            "metrics": metrics,
            "saved_at": datetime.now().isoformat(),
        }
        if feature_names:
            # Persist the exact training feature order so predict-time
            # featurization can't silently drift (e.g. matminer vs the
            # basic fallback backend produce different feature sets).
            meta["feature_names"] = list(feature_names)
        meta_path.write_text(json.dumps(meta, indent=2))

        return model_path

    def load_model(self, property_name: str, algorithm: str) -> Optional[Any]:
        # joblib deserialization is pickle-based; safe here because this
        # directory only ever holds models the user trained locally via
        # save_model above — PRISM never downloads models into it.
        model_path = self.models_dir / f"{property_name}_{algorithm}.joblib"
        if model_path.exists():
            return joblib.load(model_path)
        return None

    def load_meta(self, property_name: str, algorithm: str) -> Optional[Dict]:
        meta_path = self.models_dir / f"{property_name}_{algorithm}.meta.json"
        if not meta_path.exists():
            return None
        try:
            return json.loads(meta_path.read_text())
        except (json.JSONDecodeError, OSError):
            return None

    def list_models(self) -> List[Dict]:
        models = []
        for meta_file in sorted(self.models_dir.glob("*.meta.json")):
            try:
                meta = json.loads(meta_file.read_text())
                models.append(meta)
            except (json.JSONDecodeError, KeyError):
                continue
        return models
