"""Tests for AlgorithmRegistry."""
import pytest
from app.ml.algorithm_registry import AlgorithmRegistry, get_default_registry


class TestAlgorithmRegistry:
    def test_register_and_get(self):
        reg = AlgorithmRegistry()
        reg.register("dummy", "Dummy model", lambda: {"type": "dummy"})
        model = reg.get("dummy")
        assert model == {"type": "dummy"}

    def test_get_unknown_raises(self):
        reg = AlgorithmRegistry()
        with pytest.raises(ValueError, match="Unknown algorithm"):
            reg.get("nonexistent")

    def test_list_algorithms(self):
        reg = AlgorithmRegistry()
        reg.register("a", "Algorithm A", lambda: None)
        reg.register("b", "Algorithm B", lambda: None)
        algos = reg.list_algorithms()
        assert len(algos) == 2
        names = {a["name"] for a in algos}
        assert names == {"a", "b"}

    def test_has(self):
        reg = AlgorithmRegistry()
        reg.register("present", "P", lambda: None)
        assert reg.has("present") is True
        assert reg.has("absent") is False

    def test_factory_called_fresh_each_time(self):
        call_count = 0

        def factory():
            nonlocal call_count
            call_count += 1
            return call_count

        reg = AlgorithmRegistry()
        reg.register("counter", "Counter", factory)
        assert reg.get("counter") == 1
        assert reg.get("counter") == 2


class TestGetDefaultRegistry:
    def test_has_builtin_algorithms(self):
        reg = get_default_registry()
        assert reg.has("random_forest")
        assert reg.has("gradient_boosting")
        assert reg.has("linear")

    def test_list_has_at_least_three(self):
        reg = get_default_registry()
        algos = reg.list_algorithms()
        assert len(algos) >= 3

    def test_random_forest_creates_model(self):
        reg = get_default_registry()
        try:
            model = reg.get("random_forest")
            assert hasattr(model, "fit")
        except ImportError:
            pytest.skip("scikit-learn not installed")

    def test_linear_creates_model(self):
        reg = get_default_registry()
        try:
            model = reg.get("linear")
            assert hasattr(model, "fit")
        except ImportError:
            pytest.skip("scikit-learn not installed")
