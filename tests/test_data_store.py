"""Tests for data normalizer and store."""
import pytest
import tempfile
from app.data.normalizer import normalize_records
from app.data.store import DataStore


class TestNormalizer:
    def test_normalize_basic(self):
        records = [
            {"source_id": "mp:1", "formula": "Si", "elements": ["Si"], "provider": "mp"},
            {"source_id": "oqmd:2", "formula": "Si", "elements": ["Si"], "provider": "oqmd"},
        ]
        df = normalize_records(records)
        assert len(df) == 2
        assert "formula" in df.columns

    def test_normalize_deduplicates(self):
        records = [
            {"source_id": "mp:1", "formula": "Si", "elements": ["Si"], "provider": "mp"},
            {"source_id": "mp:1", "formula": "Si", "elements": ["Si"], "provider": "mp"},
        ]
        df = normalize_records(records)
        assert len(df) == 1


class TestDataStore:
    def test_save_and_load(self):
        records = [{"source_id": "mp:1", "formula": "Si", "elements": ["Si"], "provider": "mp"}]
        with tempfile.TemporaryDirectory() as tmpdir:
            store = DataStore(data_dir=tmpdir)
            df = normalize_records(records)
            store.save(df, "test_collection")
            loaded = store.load("test_collection")
            assert len(loaded) == 1

    def test_list_datasets(self):
        records = [{"source_id": "x", "formula": "X", "elements": [], "provider": "p"}]
        with tempfile.TemporaryDirectory() as tmpdir:
            store = DataStore(data_dir=tmpdir)
            df = normalize_records(records)
            store.save(df, "dataset_a")
            datasets = store.list_datasets()
            assert len(datasets) >= 1
