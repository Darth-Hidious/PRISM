"""Model registry: save, load, and list trained models."""
import json
from datetime import datetime
from pathlib import Path
from typing import Any, Dict, List, Optional

import joblib


class ModelRegistry:
    def __init__(self, models_dir: Optional[str] = None):
        self.models_dir = Path(models_dir) if models_dir else Path("models")
        self.models_dir.mkdir(parents=True, exist_ok=True)

    def save_model(self, model: Any, property_name: str, algorithm: str, metrics: Dict) -> Path:
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
        meta_path.write_text(json.dumps(meta, indent=2))

        return model_path

    def load_model(self, property_name: str, algorithm: str) -> Optional[Any]:
        model_path = self.models_dir / f"{property_name}_{algorithm}.joblib"
        if model_path.exists():
            return joblib.load(model_path)
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
