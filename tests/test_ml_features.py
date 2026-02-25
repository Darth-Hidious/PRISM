"""Tests for ML feature engineering."""
from unittest.mock import patch, MagicMock


class TestCompositionFeaturesBasic:
    """Test the built-in fallback feature backend."""

    def test_simple_element(self):
        from app.ml.features import _composition_features_basic
        features = _composition_features_basic("Fe")
        assert features["n_elements"] == 1
        assert features["avg_atomic_mass"] == 55.85
        assert features["std_atomic_mass"] == 0.0

    def test_binary_compound(self):
        from app.ml.features import _composition_features_basic
        features = _composition_features_basic("Fe2O3")
        assert features["n_elements"] == 2
        assert features["total_atoms_in_formula"] == 5.0
        assert features["range_atomic_mass"] > 0

    def test_unknown_element_partial(self):
        from app.ml.features import _composition_features_basic
        # Xe is not in ELEMENT_DATA
        features = _composition_features_basic("Xe")
        assert features.get("n_elements") == 1
        # No property stats since Xe not in lookup
        assert "avg_atomic_mass" not in features

    def test_empty_formula(self):
        from app.ml.features import _composition_features_basic
        assert _composition_features_basic("") == {}

    def test_feature_count(self):
        from app.ml.features import _composition_features_basic
        features = _composition_features_basic("SiO2")
        # n_elements + total_atoms + 4 props * 5 stats = 22
        assert len(features) == 22


class TestCompositionFeaturesDispatch:
    """Test the auto-dispatch between matminer and basic."""

    def test_composition_features_returns_dict(self):
        from app.ml.features import composition_features
        features = composition_features("Fe2O3")
        assert isinstance(features, dict)
        assert len(features) > 0
        assert "n_elements" in features

    def test_get_feature_backend(self):
        from app.ml.features import get_feature_backend
        backend = get_feature_backend()
        assert backend in ("matminer", "basic")

    def test_matminer_features_more_than_basic(self):
        """If matminer is available, should produce more features."""
        from app.ml.features import get_feature_backend, composition_features
        from app.ml.features import _composition_features_basic
        features = composition_features("Fe2O3")
        basic = _composition_features_basic("Fe2O3")
        if get_feature_backend() == "matminer":
            assert len(features) > len(basic)
        else:
            assert len(features) == len(basic)

    def test_fallback_on_bad_formula(self):
        """Even with matminer, bad formula should return something or empty."""
        from app.ml.features import composition_features
        # This is a weird formula — matminer might fail, fallback should handle
        result = composition_features("XYZ123NotReal")
        # Should return dict (possibly empty)
        assert isinstance(result, dict)


class TestParseFormula:
    def test_simple(self):
        from app.ml.features import _parse_formula
        assert _parse_formula("Fe2O3") == {"Fe": 2.0, "O": 3.0}

    def test_single_element(self):
        from app.ml.features import _parse_formula
        assert _parse_formula("Si") == {"Si": 1.0}

    def test_no_count(self):
        from app.ml.features import _parse_formula
        assert _parse_formula("NaCl") == {"Na": 1.0, "Cl": 1.0}


class TestPretrainedModels:
    def test_list_pretrained(self):
        from app.ml.pretrained import list_pretrained_models
        models = list_pretrained_models()
        assert len(models) >= 3
        names = [m["name"] for m in models]
        assert "m3gnet-eform" in names
        assert "megnet-eform" in names
        assert "megnet-bandgap" in names

    def test_unknown_model(self):
        from app.ml.pretrained import predict_with_pretrained
        result = predict_with_pretrained("nonexistent-model")
        assert "error" in result

    def test_no_structure(self):
        from app.ml.pretrained import predict_with_pretrained
        result = predict_with_pretrained("m3gnet-eform")
        assert "error" in result
        assert "structure" in result["error"].lower()

    def test_bad_structure_data(self):
        from app.ml.pretrained import predict_with_pretrained
        result = predict_with_pretrained(
            "m3gnet-eform",
            structure_data={"lattice": None, "species": None, "coords": None},
        )
        assert "error" in result


class TestAlgorithmRegistry:
    def test_default_has_sklearn(self):
        from app.ml.algorithm_registry import get_default_registry
        reg = get_default_registry()
        algos = reg.list_algorithms()
        names = [a["name"] for a in algos]
        assert "random_forest" in names
        assert "gradient_boosting" in names
        assert "linear" in names

    def test_pretrained_flag(self):
        from app.ml.algorithm_registry import get_default_registry
        reg = get_default_registry()
        algos = reg.list_algorithms()
        for a in algos:
            if a["name"].startswith("m3gnet") or a["name"].startswith("megnet"):
                assert a["pretrained"] is True
                assert a["requires_structure"] is True
            elif a["name"] in ("random_forest", "linear"):
                assert a.get("pretrained", False) is False


class TestPredictStructureTool:
    def test_tool_registered(self):
        from app.tools.base import ToolRegistry
        from app.tools.prediction import create_prediction_tools
        reg = ToolRegistry()
        create_prediction_tools(reg)
        tool = reg.get("predict_structure")
        assert tool.name == "predict_structure"
        assert "structure" in tool.input_schema["required"]

    def test_list_models_includes_pretrained(self):
        from app.tools.base import ToolRegistry
        from app.tools.prediction import create_prediction_tools
        reg = ToolRegistry()
        create_prediction_tools(reg)
        tool = reg.get("list_models")
        result = tool.execute()
        assert "pretrained_models" in result
        assert "feature_backend" in result

    def test_predict_structure_in_bootstrap(self):
        from app.plugins.bootstrap import build_full_registry
        tool_reg, _, _ = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in tool_reg.list_tools()}
        assert "predict_structure" in names
        assert "predict_property" in names
        assert "list_models" in names
