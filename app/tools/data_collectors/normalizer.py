"""Normalize materials data into a unified schema."""
from typing import Dict, List
import pandas as pd


def normalize_records(records: List[Dict]) -> pd.DataFrame:
    if not records:
        return pd.DataFrame()
    df = pd.DataFrame(records)
    if "elements" in df.columns:
        df["elements"] = df["elements"].apply(lambda x: ",".join(sorted(x)) if isinstance(x, list) else str(x))
    if "source_id" in df.columns:
        df = df.drop_duplicates(subset=["source_id"])
    return df.reset_index(drop=True)
