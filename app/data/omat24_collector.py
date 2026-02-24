"""OMAT24 collector — Meta's Open Materials 2024 dataset via HuggingFace."""
from typing import Dict, List

from app.data.base_collector import DataCollector


class OMAT24Collector(DataCollector):
    name = "omat24"

    def collect(self, elements: List[str] = None, max_results: int = 100,
                formula: str = None, **kwargs) -> List[Dict]:
        """Stream OMAT24 from HuggingFace, filter by elements/formula."""
        try:
            from datasets import load_dataset
        except ImportError:
            return []

        ds = load_dataset("fairchem/OMAT24", split="train", streaming=True)

        results = []
        for row in ds:
            if len(results) >= max_results:
                break
            record = self._parse_row(row)
            if elements and not self._matches_elements(record, elements):
                continue
            if formula and record.get("formula", "") != formula:
                continue
            results.append(record)

        return results

    def _parse_row(self, row: dict) -> dict:
        """Convert a HuggingFace dataset row to a PRISM record."""
        return {
            "source": "omat24",
            "source_id": f"omat24:{row.get('id', '')}",
            "formula": row.get("formula", row.get("composition", "")),
            "elements": row.get("elements", []),
            "energy": row.get("energy"),
            "energy_per_atom": row.get("energy_per_atom"),
            "forces": row.get("forces"),
            "stress": row.get("stress"),
            "positions": row.get("positions"),
            "cell": row.get("cell"),
            "pbc": row.get("pbc"),
            "natoms": row.get("natoms"),
        }

    def _matches_elements(self, record: dict, elements: List[str]) -> bool:
        rec_elements = record.get("elements", [])
        if not rec_elements:
            return True  # Can't filter, include it
        return all(e in rec_elements for e in elements)

    def supported_params(self) -> List[str]:
        return ["elements", "max_results", "formula"]
