"""Tests for ML feature engineering."""
from unittest.mock import patch, MagicMock


class TestCompositionFeaturesBasic:
    """Test the built-in fallback feature backend."""

    def test_simple_element(self):
        from app.tools.ml.features import _composition_features_basic
        features = _composition_features_basic("Fe")
        assert features["n_elements"] == 1
        assert features["avg_atomic_mass"] == 55.85
        assert features["std_atomic_mass"] == 0.0

    def test_binary_compound(self):
        from app.tools.ml.features import _composition_features_basic
        features = _composition_features_basic("Fe2O3")
        assert features["n_elements"] == 2
        assert features["total_atoms_in_formula"] == 5.0
        assert features["range_atomic_mass"] > 0

    def test_unknown_element_partial(self):
        from app.tools.ml.features import _composition_features_basic
        # Xe is not in ELEMENT_DATA
        features = _composition_features_basic("Xe")
        assert features.get("n_elements") == 1
        # No property stats since Xe not in lookup
        assert "avg_atomic_mass" not in features

    def test_empty_formula(self):
        from app.tools.ml.features import _composition_features_basic
        assert _composition_features_basic("") == {}

    def test_partial_coverage_emits_no_property_statistics(self):
        """An element the table does not carry must not be silently dropped.

        BSb, BOs and TcB used to come back with the full 22-feature shape
        holding nothing but boron's numbers (avg_electronegativity 2.04,
        min == max == boron), so three different compounds were identical in
        feature space and the predictor returned one value for all of them.
        """
        from app.tools.ml.features import ELEMENT_DATA, _composition_features_basic

        assert "Sb" not in ELEMENT_DATA and "B" in ELEMENT_DATA
        f = _composition_features_basic("BSb")
        boron = ELEMENT_DATA["B"]["electronegativity"]
        for stat in ("avg", "min", "max", "range", "std"):
            assert f"{stat}_electronegativity" not in f, (
                f"{stat}_electronegativity for BSb describes boron alone"
            )
        assert boron not in f.values()
        # The parse itself is still honest, and full coverage is untouched.
        assert f["n_elements"] == 2
        assert len(_composition_features_basic("B2O3")) == 22

    def test_zero_atom_formula_returns_empty(self):
        """"Fe0" parses to a real element with zero atoms; dividing by that
        total raised ZeroDivisionError out of the featurizer instead of
        returning the documented empty dict."""
        from app.tools.ml.features import _composition_features_basic

        assert _composition_features_basic("Fe0") == {}
        assert _composition_features_basic("H0") == {}

    def test_feature_count(self):
        from app.tools.ml.features import _composition_features_basic
        features = _composition_features_basic("SiO2")
        # n_elements + total_atoms + 4 props * 5 stats = 22
        assert len(features) == 22


class TestCompositionFeaturesDispatch:
    """Test the auto-dispatch between matminer and basic."""

    def test_composition_features_returns_dict(self):
        from app.tools.ml.features import composition_features
        features = composition_features("Fe2O3")
        assert isinstance(features, dict)
        assert len(features) > 0
        assert "n_elements" in features

    def test_get_feature_backend(self):
        from app.tools.ml.features import get_feature_backend
        backend = get_feature_backend()
        assert backend in ("matminer", "basic")

    def test_matminer_features_more_than_basic(self):
        """If matminer is available, should produce more features."""
        from app.tools.ml.features import get_feature_backend, composition_features
        from app.tools.ml.features import _composition_features_basic
        features = composition_features("Fe2O3")
        basic = _composition_features_basic("Fe2O3")
        if get_feature_backend() == "matminer":
            assert len(features) > len(basic)
        else:
            assert len(features) == len(basic)

    def test_fallback_on_bad_formula(self):
        """Even with matminer, bad formula should return something or empty."""
        from app.tools.ml.features import composition_features
        # This is a weird formula — matminer might fail, fallback should handle
        result = composition_features("XYZ123NotReal")
        # Should return dict (possibly empty)
        assert isinstance(result, dict)


class TestParseFormula:
    def test_simple(self):
        from app.tools.ml.features import _parse_formula
        assert _parse_formula("Fe2O3") == {"Fe": 2.0, "O": 3.0}

    def test_single_element(self):
        from app.tools.ml.features import _parse_formula
        assert _parse_formula("Si") == {"Si": 1.0}

    def test_no_count(self):
        from app.tools.ml.features import _parse_formula
        assert _parse_formula("NaCl") == {"Na": 1.0, "Cl": 1.0}

    def test_ascii_oxide_dot_is_an_adduct_separator(self):
        """A "." not followed by a digit cannot be a decimal point.

        Oxide/cement notation put one straight into the count group, where
        float(".") raised ValueError out of the tokenizer and took the whole
        predict_property batch — valid formulas included — down with it.
        """
        from app.tools.ml.features import _parse_formula, composition_features

        # MgO.Al2O3 is spinel MgAl2O4; 3CaO.SiO2 is alite Ca3SiO5 (the
        # leading 3 belongs to its own segment, not to the whole string).
        assert _parse_formula("MgO.Al2O3") == {"Mg": 1.0, "Al": 2.0, "O": 4.0}
        assert _parse_formula("3CaO.SiO2") == {"Ca": 3.0, "Si": 1.0, "O": 5.0}
        assert len(composition_features("MgO.Al2O3")) == 22

    def test_decimal_stoichiometry_is_not_split(self):
        """A dot BETWEEN DIGITS stays with the numeric parser — splitting it
        would corrupt every Mg1.5Si0.5O4-style composition."""
        from app.tools.ml.features import _parse_formula

        assert _parse_formula("Mg1.5Si0.5O4") == {"Mg": 1.5, "Si": 0.5, "O": 4.0}
        assert _parse_formula("CuSO4.5H2O") == {
            "Cu": 1.0, "S": 1.0, "O": 5.5, "H": 2.0
        }


class TestPretrainedModels:
    def test_list_pretrained(self):
        from app.tools.ml.pretrained import list_pretrained_models
        models = list_pretrained_models()
        assert len(models) >= 3
        names = [m["name"] for m in models]
        assert "m3gnet-eform" in names
        assert "megnet-eform" in names
        assert "megnet-bandgap" in names

    def test_unknown_model(self):
        from app.tools.ml.pretrained import predict_with_pretrained
        result = predict_with_pretrained("nonexistent-model")
        assert "error" in result

    def test_no_structure(self):
        from app.tools.ml.pretrained import predict_with_pretrained
        result = predict_with_pretrained("m3gnet-eform")
        assert "error" in result
        assert "structure" in result["error"].lower()

    def test_bad_structure_data(self):
        from app.tools.ml.pretrained import predict_with_pretrained
        result = predict_with_pretrained(
            "m3gnet-eform",
            structure_data={"lattice": None, "species": None, "coords": None},
        )
        assert "error" in result


class TestAlgorithmRegistry:
    def test_default_has_sklearn(self):
        from app.tools.ml.algorithm_registry import get_default_registry
        reg = get_default_registry()
        algos = reg.list_algorithms()
        names = [a["name"] for a in algos]
        assert "random_forest" in names
        assert "gradient_boosting" in names
        assert "linear" in names

    def test_pretrained_flag(self):
        from app.tools.ml.algorithm_registry import get_default_registry
        reg = get_default_registry()
        algos = reg.list_algorithms()
        for a in algos:
            if a["name"].startswith("m3gnet") or a["name"].startswith("megnet"):
                assert a["pretrained"] is True
                assert a["requires_structure"] is True
            elif a["name"] in ("random_forest", "linear"):
                assert a.get("pretrained", False) is False


class TestPredictStructureTool:
    """After Round 4 batch 2: predict_property + predict_structure were
    collapsed into a unified `predict(target='formula'|'structure')` tool.
    """

    def test_unified_predict_tool_registered(self):
        from app.tools.base import ToolRegistry
        from app.tools.prediction import create_prediction_tools
        reg = ToolRegistry()
        create_prediction_tools(reg)
        tool = reg.get("predict")
        assert tool.name == "predict"
        # target is required; both formula + structure modes are advertised
        assert "target" in tool.input_schema["required"]
        targets = tool.input_schema["properties"]["target"]["enum"]
        assert "structure" in targets
        assert "formula" in targets

    def test_list_models_includes_pretrained(self):
        from app.tools.base import ToolRegistry
        from app.tools.prediction import create_prediction_tools
        reg = ToolRegistry()
        create_prediction_tools(reg)
        tool = reg.get("list_models")
        result = tool.execute()
        assert "pretrained_models" in result
        assert "feature_backend" in result

    def test_predict_in_bootstrap(self):
        from app.plugins.bootstrap import build_full_registry
        tool_reg, _, _ = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in tool_reg.list_tools()}
        # Unified `predict` replaces predict_property + predict_structure
        assert "predict" in names
        assert "list_models" in names
        # `predict_structure` is gone and must stay gone — that half of the
        # Round-4 collapse still holds.
        assert "predict_structure" not in names

        # `predict_property` is NOT asserted absent any more, and that is not a
        # relaxation — the name was legitimately reused.
        #
        # This assertion was written 2026-07-03 (a56d8229) to pin the collapse
        # of predict_property + predict_structure into `predict`. On 2026-07-22
        # (80da217b) the E9 informatics batch registered a NEW and unrelated
        # `predict_property` — matminer+sklearn with uncertainty, trained on MP
        # via the proxy (`app/tools/materials/informatics.py:237`) — which
        # `pareto_screen` and `hea_dataset` both consume.
        #
        # So the assertion has been false since 19 days after it was written.
        # Nothing caught it because no workflow ran pytest until the
        # `python-suite` job was added; its own comment says so.
        #
        # What still matters is that the E9 tool is the informatics one and not
        # a resurrected copy of the collapsed tool, so pin its provenance
        # rather than its absence.
        if "predict_property" in names:
            tool = tool_reg.get("predict_property")
            assert tool.source_detail == "materials.informatics", (
                "`predict_property` is registered but is not the E9 informatics "
                f"tool (source_detail={tool.source_detail!r}) — the collapsed "
                "pre-1.0 tool may have been resurrected"
            )


def test_gradient_boosting_predicts_without_crashing_and_reports_no_uncertainty():
    """`gradient_boosting` is in the tool's enum; picking it used to crash the call.

    `hasattr(model, "estimators_")` is true for BOTH regressors, but
    GradientBoostingRegressor's is a 2-D ndarray — iterating it yields
    sub-arrays, so `t.predict(x)` raised AttributeError. `Tool.execute` caught
    that generically, so an agent choosing a documented enum value got
    `{"error": "AttributeError: ..."}` and zero predictions.

    Flattening would have made it "work" and the number wrong: boosting trees
    are sequential residual fitters, so their spread is not an uncertainty.
    Predictions, `uncertainty is None`, is the honest result.

    Asserted at the sklearn level the handler uses, so the test needs no MP
    network pull.
    """
    import numpy as np
    from sklearn.ensemble import GradientBoostingRegressor, RandomForestRegressor

    X = np.random.RandomState(0).rand(24, 3)
    y = np.random.RandomState(1).rand(24)
    x = X[:1]

    rf = RandomForestRegressor(n_estimators=4, random_state=0).fit(X, y)
    gb = GradientBoostingRegressor(n_estimators=4, random_state=0).fit(X, y)

    # The trap: the attribute exists on both, so `hasattr` cannot discriminate.
    assert hasattr(rf, "estimators_") and hasattr(gb, "estimators_")

    # The handler's rule — gate on the ensemble KIND.
    for model, expects_uncertainty in ((rf, True), (gb, False)):
        unc = None
        if isinstance(model, RandomForestRegressor):
            unc = float(np.std([float(t.predict(x)[0]) for t in model.estimators_]))
        assert (unc is not None) is expects_uncertainty, type(model).__name__
        # Whichever branch, a prediction is always produced.
        assert isinstance(float(model.predict(x)[0]), float)
