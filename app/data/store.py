"""Storage layer for collected materials data (Parquet + metadata)."""
import json
from datetime import datetime
from pathlib import Path
from typing import List, Optional
import pandas as pd


class DataStore:
    def __init__(self, data_dir: Optional[str] = None):
        self.data_dir = Path(data_dir) if data_dir else Path("data")
        self.data_dir.mkdir(parents=True, exist_ok=True)

    def save(self, df: pd.DataFrame, name: str) -> Path:
        filepath = self.data_dir / f"{name}.parquet"
        df.to_parquet(filepath, index=False)
        meta = {"name": name, "rows": len(df), "columns": list(df.columns), "saved_at": datetime.now().isoformat()}
        meta_path = self.data_dir / f"{name}.meta.json"
        meta_path.write_text(json.dumps(meta, indent=2))
        return filepath

    def load(self, name: str) -> pd.DataFrame:
        filepath = self.data_dir / f"{name}.parquet"
        return pd.read_parquet(filepath)

    def list_datasets(self) -> List[dict]:
        datasets = []
        for meta_file in sorted(self.data_dir.glob("*.meta.json")):
            try:
                meta = json.loads(meta_file.read_text())
                datasets.append(meta)
            except (json.JSONDecodeError, KeyError):
                continue
        return datasets
