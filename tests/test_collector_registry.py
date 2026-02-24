"""Tests for DataCollector ABC and CollectorRegistry."""
import pytest
from app.data.base_collector import DataCollector, CollectorRegistry


class FakeCollector(DataCollector):
    name = "fake"

    def collect(self, **kwargs) -> list:
        return [{"source": "fake", "value": 1}]

    def supported_params(self) -> list:
        return ["param_a"]


class ErrorCollector(DataCollector):
    name = "error"

    def collect(self, **kwargs) -> list:
        raise RuntimeError("boom")


class TestDataCollectorABC:
    def test_cannot_instantiate_abc(self):
        with pytest.raises(TypeError):
            DataCollector()

    def test_subclass_collect(self):
        c = FakeCollector()
        assert c.collect() == [{"source": "fake", "value": 1}]

    def test_supported_params_default(self):
        class Bare(DataCollector):
            name = "bare"
            def collect(self, **kwargs):
                return []
        assert Bare().supported_params() == []

    def test_supported_params_override(self):
        assert FakeCollector().supported_params() == ["param_a"]


class TestCollectorRegistry:
    def test_register_and_get(self):
        reg = CollectorRegistry()
        c = FakeCollector()
        reg.register(c)
        assert reg.get("fake") is c

    def test_get_unknown_raises(self):
        reg = CollectorRegistry()
        with pytest.raises(KeyError):
            reg.get("nonexistent")

    def test_list_collectors(self):
        reg = CollectorRegistry()
        reg.register(FakeCollector())
        assert len(reg.list_collectors()) == 1
        assert reg.list_collectors()[0].name == "fake"

    def test_collect_all_single_source(self):
        reg = CollectorRegistry()
        reg.register(FakeCollector())
        records = reg.collect_all(["fake"])
        assert len(records) == 1
        assert records[0]["source"] == "fake"

    def test_collect_all_skips_unknown(self):
        reg = CollectorRegistry()
        reg.register(FakeCollector())
        records = reg.collect_all(["fake", "nonexistent"])
        assert len(records) == 1

    def test_collect_all_error_collector_skipped(self):
        reg = CollectorRegistry()
        reg.register(FakeCollector())
        reg.register(ErrorCollector())
        records = reg.collect_all(["error", "fake"])
        assert len(records) == 1
        assert records[0]["source"] == "fake"

    def test_collect_all_empty_sources(self):
        reg = CollectorRegistry()
        assert reg.collect_all([]) == []
