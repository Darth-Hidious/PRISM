"""Base class and registry for data collectors."""
from abc import ABC, abstractmethod
from typing import Dict, List


class DataCollector(ABC):
    """Base class for all data collectors."""

    name: str = ""

    @abstractmethod
    def collect(self, **kwargs) -> List[Dict]:
        """Collect records from this source."""
        ...

    def supported_params(self) -> List[str]:
        """Parameters this collector accepts."""
        return []


class CollectorRegistry:
    """Registry that manages DataCollector instances."""

    def __init__(self):
        self._collectors: Dict[str, DataCollector] = {}

    def register(self, collector: DataCollector) -> None:
        self._collectors[collector.name] = collector

    def get(self, name: str) -> DataCollector:
        if name not in self._collectors:
            raise KeyError(f"Unknown collector: {name}")
        return self._collectors[name]

    def list_collectors(self) -> List[DataCollector]:
        return list(self._collectors.values())

    def collect_all(self, sources: List[str], **kwargs) -> List[Dict]:
        all_records: List[Dict] = []
        for src in sources:
            if src in self._collectors:
                try:
                    records = self._collectors[src].collect(**kwargs)
                    all_records.extend(records)
                except Exception:
                    pass
        return all_records
