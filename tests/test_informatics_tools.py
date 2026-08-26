"""Tests for the informatics tools (E7-E12)."""
from unittest.mock import patch, MagicMock

import pytest


def test_all_informatics_tools_register():
    from app.tools.base import ToolRegistry
    from app.tools.materials.informatics import create_informatics_tools

    reg = ToolRegistry()
    create_informatics_tools(reg)
    names = {t.name for t in reg.list_tools()}
    for n in ("structure_similarity", "compute_descriptor", "predict_property",
              "pareto_screen", "suggest_next_experiments"):
        assert n in names, f"{n} must register"


def test_structure_desc_tools_register():
    from app.tools.base import ToolRegistry
    from app.tools.materials.structure_desc import create_structure_desc_tools

    reg = ToolRegistry()
    create_structure_desc_tools(reg)
    names = {t.name for t in reg.list_tools()}
    assert "describe_structure" in names
    assert "predict_synthesizability" in names


# ---- E10: pareto_screen (deterministic, no deps) ----

def test_pareto_front_simple():
    """A known 2-objective Pareto set."""
    from app.tools.base import ToolRegistry
    from app.tools.materials.informatics import create_informatics_tools

    reg = ToolRegistry()
    create_informatics_tools(reg)
    candidates = [
        {"formula": "A", "density": 1, "modulus": 100},  # Pareto (lightest)
        {"formula": "B", "density": 5, "modulus": 200},   # Pareto (stiffest)
        {"formula": "C", "density": 5, "modulus": 100},   # dominated by both
        {"formula": "D", "density": 3, "modulus": 150},   # Pareto (compromise)
    ]
    out = reg.get("pareto_screen").func(
        candidates=candidates,
        objectives=[{"property": "density", "direction": "min"},
                    {"property": "modulus", "direction": "max"}],
    )
    front_formulas = {f["candidate"]["formula"] for f in out["pareto_front"]}
    assert front_formulas == {"A", "B", "D"}, f"Pareto front wrong: {front_formulas}"
    assert out["dominated_count"] == 1  # only C is dominated
    assert out["pareto_count"] == 3


# ---- E11: suggest_next_experiments ----

def test_suggest_next_experiments_ranks_by_ei():
    """EI should favor high-uncertainty + near-best candidates."""
    from app.tools.base import ToolRegistry
    from app.tools.materials.informatics import create_informatics_tools

    reg = ToolRegistry()
    create_informatics_tools(reg)
    # Candidate with high uncertainty but below best should still rank (exploration).
    candidates = [
        {"formula": "certain_good", "predicted": 9.5, "uncertainty": 0.1},
        {"formula": "uncertain_mid", "predicted": 7.0, "uncertainty": 3.0},  # high σ → high EI
        {"formula": "certain_bad", "predicted": 1.0, "uncertainty": 0.1},
    ]
    out = reg.get("suggest_next_experiments").func(candidates=candidates, n_suggestions=2, acquisition="ei", direction="max")
    top = out["suggestions"][0]["formula"]
    # The high-uncertainty one should win on EI (exploration value).
    assert top == "uncertain_mid", f"EI should favor high-σ exploration: {top}"
    assert out["acquisition"] == "ei"


# ---- E8: compute_descriptor ----

def test_compute_descriptor_magpie():
    """matminer magpie featurization (132 features)."""
    from app.tools.base import ToolRegistry
    from app.tools.materials.informatics import create_informatics_tools

    reg = ToolRegistry()
    create_informatics_tools(reg)
    try:
        from matminer.featurizers.composition import ElementProperty  # noqa
    except ImportError:
        pytest.skip("matminer not installed")
    out = reg.get("compute_descriptor").func(formulas=["Cu2O"])
    assert len(out["descriptors"]) == 1
    assert out["descriptors"][0]["n_features"] == 132


# ---- E9: predict_property (needs ML stack — verify it degrades honestly if missing) ----

def test_predict_property_requires_formulas():
    from app.tools.base import ToolRegistry
    from app.tools.materials.informatics import create_informatics_tools

    reg = ToolRegistry()
    create_informatics_tools(reg)
    out = reg.get("predict_property").func()
    assert "error" in out


def test_predict_property_reports_actual_feature_backend():
    """C6 honesty: the provenance must report the feature backend that ACTUALLY
    ran (matminer Magpie only when matminer is installed; otherwise the builtin
    22-feature fallback) — the old code hardcoded 'matminer magpie' even when
    the fallback ran. ML plumbing is mocked; this tests the provenance logic.
    """
    from app.tools.base import ToolRegistry
    from app.tools.materials.informatics import create_informatics_tools

    try:
        import sklearn  # noqa: F401
        import numpy  # noqa: F401
    except ImportError:
        pytest.skip("sklearn/numpy not installed")

    reg = ToolRegistry()
    create_informatics_tools(reg)

    formulas = [f"El{i}O{i % 3 + 1}" for i in range(30)]
    fake_mp = {
        "results": [
            {"formula_pretty": f, "formation_energy_per_atom": -0.05 * i}
            for i, f in enumerate(formulas)
        ]
    }

    def _fake_features(formula):
        # A NAME -> VALUE dict, which is what the real `composition_features`
        # returns. The old mock handed back a bare list; production indexes the
        # row by feature NAME, so a list mock exercised a code path that cannot
        # occur and crashed on the real one.
        return {
            "f_len": float(len(formula)),
            "f_ord": float(sum(map(ord, formula)) % 97),
            "f_bias": 1.0,
        }

    with (
        patch("app.tools.data._query_materials_project", return_value=fake_mp),
        patch("app.tools.ml.features.composition_features", side_effect=_fake_features),
    ):
        out = reg.get("predict_property").func(formulas=["Cu2O"])

    assert "predictions" in out, f"predict_property failed: {out}"
    backend = out["model_meta"]["feature_backend"]

    from app.tools.ml.features import get_feature_backend

    if get_feature_backend() == "matminer":
        assert "matminer" in backend
    else:
        # matminer absent → must NOT claim magpie; must name the real fallback
        assert "matminer magpie" != backend
        assert "builtin" in backend and "matminer not installed" in backend
        assert "matminer magpie" not in out["provenance"]
        assert "builtin" in out["provenance"]


# ---- E12: predict_synthesizability (heuristic) ----

def test_synthesizability_is_honest_heuristic():
    from app.tools.base import ToolRegistry
    from app.tools.materials.structure_desc import create_structure_desc_tools

    reg = ToolRegistry()
    create_structure_desc_tools(reg)
    # Mock the MP lookup so the hull factor returns a stable value.
    with patch("app.tools.data._query_materials_project",
               return_value={"results": [{"material_id": "mp-1", "energy_above_hull": 0.0}]}):
        out = reg.get("predict_synthesizability").func(formula="Cu2O")
    assert "score" in out
    assert out["likely_synthesizable"] is True  # on-hull → synthesizable
    assert "HEURISTIC" in out["note"]
    assert len(out["factors"]) >= 1


def test_describe_structure_degrades_without_robocrystallographer():
    """On py3.14 robocrystallographer isn't installed — honest degrade."""
    from app.tools.base import ToolRegistry
    from app.tools.materials.structure_desc import create_structure_desc_tools

    reg = ToolRegistry()
    create_structure_desc_tools(reg)
    try:
        import robocrystallographer  # noqa
        pytest.skip("robocrystallographer installed — degrade path can't be tested")
    except ImportError:
        out = reg.get("describe_structure").func(formula="Cu2O")
        assert out.get("tool_available") is False
        assert "robocrystallographer" in out["error"].lower()


def test_a_nan_objective_cannot_sit_on_the_pareto_front():
    """NaN neither dominates nor is dominated, so it used to survive screening.

    Every comparison with NaN is False, so the dominance loop marked a NaN
    candidate neither dominated nor dominating and it landed on the front
    unconditionally. Measured before the fix: a candidate with density=NaN and
    modulus=1.0 sat on the front beside the true optimum (density=1.0,
    modulus=300) while the strictly-worse real candidate was correctly
    dominated. NaN arrives easily — any pandas column, any upstream tool
    emitting float("nan").

    The drop is also REPORTED rather than silent: a candidate that vanished for
    want of a finite objective is a data gap the caller needs to see.
    """
    from app.tools.materials.informatics import _pareto_screen_tool

    out = _pareto_screen_tool().func(
        candidates=[
            {"id": "BEST", "density": 1.0, "modulus": 300.0},
            {"id": "JUNK_NAN", "density": float("nan"), "modulus": 1.0},
            {"id": "WORSE", "density": 9.0, "modulus": 10.0},
        ],
        objectives=[
            {"property": "density", "direction": "min"},
            {"property": "modulus", "direction": "max"},
        ],
    )

    ids = [c["candidate"]["id"] for c in out["pareto_front"]]
    assert ids == ["BEST"], f"NaN must not survive screening: {ids}"
    assert out["excluded_non_finite_or_missing"] == 1, (
        "the dropped candidate must be counted, not silently discarded"
    )
    assert out["total_evaluated"] == 2, "only finite candidates are evaluated"


def test_expected_improvement_uses_the_observed_incumbent_when_given():
    """EI's f* must be the best MEASURED value, not the pool's best prediction.

    Before the fix the incumbent was `max(predictions)` over the UNLABELLED
    pool, so the top-predicted candidate always got z=0 and therefore
    EI = 0.3989*sigma — the SMALLEST EI in the pool whenever its sigma was
    small. Measured: A(mu=10.0, sigma=0.01) ranked LAST at EI=0.004 behind
    C(mu=5.0). The tool could never recommend exploitation.

    The formula itself was always correct; only the incumbent was wrong.
    """
    from app.tools.materials.informatics import _suggest_next_experiments_tool

    tool = _suggest_next_experiments_tool()
    candidates = [
        {"id": "A", "predicted": 10.0, "uncertainty": 0.01},
        {"id": "B", "predicted": 9.9, "uncertainty": 3.0},
        {"id": "C", "predicted": 5.0, "uncertainty": 5.0},
    ]

    # With a real measurement, everything that beats it scores well and the
    # near-optimal candidates rise above the far one.
    out = tool.func(
        candidates=candidates, acquisition="ei", direction="max", best_observed=6.0
    )
    assert out["incumbent"] == 6.0
    assert out["incumbent_source"] == "observed"
    scores = {s["predicted"]: s["acquisition_score"] for s in out["suggestions"]}
    assert scores[10.0] > scores[5.0], (
        "a candidate predicted well above the measured incumbent must not rank "
        f"below one predicted below it: {scores}"
    )

    # Without one, the old fallback stands — but it must SAY it is a model
    # output rather than presenting it as a measured incumbent.
    out = tool.func(candidates=candidates, acquisition="ei", direction="max")
    assert out["incumbent_source"] == "pool_max_predicted"
    assert "not an observation" in out["provenance"], (
        "the fallback incumbent must be labelled as a prediction"
    )


def test_pareto_screen_refuses_non_canonical_direction():
    """`direction` decides the SIGN of an objective and nothing validates it.

    Tool.execute does not check arguments against input_schema, so the
    dispatch `obj["direction"] == "min"` treated "minimize"/"minimise"/"MIN"
    as MAXIMISE and returned the HEAVIEST candidate as the Pareto-optimal
    minimum-density pick — a wrong answer in a success shape. A missing
    `direction` (the objective item declared no `required`) raised a bare
    KeyError. Both must be refused by name.
    """
    from app.tools.base import ToolRegistry
    from app.tools.materials.informatics import create_informatics_tools

    reg = ToolRegistry()
    create_informatics_tools(reg)
    tool = reg.get("pareto_screen")
    candidates = [
        {"formula": "LIGHT", "density": 1.0},
        {"formula": "HEAVY", "density": 9.0},
    ]

    ok = tool.func(candidates=candidates,
                   objectives=[{"property": "density", "direction": "min"}])
    assert [c["candidate"]["formula"] for c in ok["pareto_front"]] == ["LIGHT"]

    for bad in ("minimize", "minimise", "MIN", "Min"):
        out = tool.func(candidates=candidates,
                        objectives=[{"property": "density", "direction": bad}])
        assert "error" in out, (
            f"direction={bad!r} must be refused, not silently maximised; got {out}"
        )
        assert "direction" in out["error"]
        assert "pareto_front" not in out

    missing = tool.func(candidates=candidates, objectives=[{"property": "density"}])
    assert "error" in missing and "direction" in missing["error"], missing
    assert "KeyError" not in missing["error"], (
        f"a missing direction must be a named refusal, not a raw KeyError: {missing}"
    )


def test_suggest_next_experiments_refuses_non_canonical_enums():
    """`acquisition` and `direction` were dispatched as `== literal else other`.

    Any other spelling silently ran the OPPOSITE branch while the response
    echoed the caller's word back: direction="maximize" ranked the pool
    backwards under `"direction": "maximize"`, and acquisition="EI" ran UCB
    under `"acquisition": "EI"` with a provenance line claiming EI.
    """
    from app.tools.base import ToolRegistry
    from app.tools.materials.informatics import create_informatics_tools

    reg = ToolRegistry()
    create_informatics_tools(reg)
    tool = reg.get("suggest_next_experiments")
    pool = [
        {"formula": "A", "predicted": 10.0, "uncertainty": 0.5},
        {"formula": "B", "predicted": 1.0, "uncertainty": 0.5},
    ]

    good = tool.func(candidates=pool, direction="max", acquisition="ucb",
                     n_suggestions=1)
    assert good["suggestions"][0]["formula"] == "A"

    for bad in ("maximize", "MAX", "max "):
        out = tool.func(candidates=pool, direction=bad, acquisition="ucb")
        assert "error" in out, (
            f"direction={bad!r} must be refused, not silently minimised; got {out}"
        )
        assert "suggestions" not in out

    for bad in ("EI", "expected_improvement", "Ei"):
        out = tool.func(candidates=pool, acquisition=bad, best_observed=5.0)
        assert "error" in out, (
            f"acquisition={bad!r} must be refused, not silently run as UCB; got {out}"
        )
        assert "suggestions" not in out


def test_predict_property_keeps_the_full_descriptor_set():
    """A sparse MINORITY of training rows must not delete COLUMNS for everyone.

    The basic feature backend drops a whole property block for a formula whose
    elements it does not know. Intersecting every row's keys therefore
    collapsed the design matrix to the two keys the sparse rows do carry —
    measured live as (364, 2) out of 22 features, after which Cu2O, Fe2O3 and
    NaCl all predicted the identical 0.14046 eV/atom while `feature_backend`
    and `provenance` both still advertised the 22-feature set.

    Drop the sparse ROWS instead, and say how many.
    """
    from app.tools.base import ToolRegistry
    from app.tools.materials.informatics import create_informatics_tools

    try:
        import sklearn  # noqa: F401
        import numpy  # noqa: F401
    except ImportError:
        pytest.skip("sklearn/numpy not installed")

    reg = ToolRegistry()
    create_informatics_tools(reg)

    # 300 rows in one response so the tool's bounded pull stops after the
    # first pattern and the row counts below are exact.
    rich = [f"Rich{i}" for i in range(250)]
    sparse = [f"Sparse{i}" for i in range(50)]
    fake_mp = {
        "results": [
            {"formula_pretty": f, "formation_energy_per_atom": -0.1 * i}
            for i, f in enumerate(rich + sparse)
        ]
    }

    def _fake_features(formula):
        # `always_*` are the two keys every formula carries, and they are
        # CONSTANT — exactly the position n_elements/total_atoms_in_formula
        # occupy for the elemental rows in the live pull. All the signal lives
        # in the three keys the sparse rows lack.
        base = {"always_a": 1.0, "always_b": 2.0}
        if formula.startswith("Sparse"):
            return base
        idx = float(formula.removeprefix("Rich"))
        return {**base, "sig_1": idx, "sig_2": idx * 2.0, "sig_3": -idx}

    with (
        patch("app.tools.data._query_materials_project", return_value=fake_mp),
        patch("app.tools.ml.features.composition_features", side_effect=_fake_features),
    ):
        out = reg.get("predict_property").func(formulas=["Rich0", "Rich249"])

    assert "predictions" in out, f"predict_property failed: {out}"
    values = [p.get("value") for p in out["predictions"]]
    assert all(v is not None for v in values), out["predictions"]
    assert values[0] != values[1], (
        "two training formulas with different descriptors must not predict the "
        f"identical value — the design matrix collapsed to its constant columns: {out}"
    )

    meta = out["model_meta"]
    assert meta["n_features_used"] == 5, (
        f"the 5-feature descriptor set must survive the 5 sparse rows: {meta}"
    )
    assert meta["training_rows_dropped_incomplete_features"] == 50, meta
    assert "5 feature(s) actually used" in out["provenance"], out["provenance"]


def test_synthesizability_scores_an_absent_database_entry():
    """"Not in Materials Project" must be visible, and must cost something.

    The existence factor was guarded by `if res.get("results")`, so its
    `-0.1` "absent from MP" branch could never fire, and the hull factor
    simply vanished from `factors` — a formula MP had never heard of was
    scored identically to one whose hull the tool never looked up, while the
    note still claimed the score combines hull distance.
    """
    from app.tools.base import ToolRegistry
    from app.tools.materials.structure_desc import create_structure_desc_tools

    reg = ToolRegistry()
    create_structure_desc_tools(reg)
    tool = reg.get("predict_synthesizability")

    with patch("app.tools.data._query_materials_project",
               return_value={"results": [], "count": 0}):
        absent = tool.func(formula="Xe3Kr2Rn")

    by_name = {f["factor"]: f for f in absent["factors"]}
    hull = by_name.get("convex_hull_distance")
    assert hull is not None, (
        f"the dominant signal must be reported as unavailable, not omitted: {absent}"
    )
    assert hull.get("available") is False and "no Materials Project entry" in hull["reason"]

    existence = by_name.get("database_existence")
    assert existence is not None, f"database_existence must be scored: {absent}"
    assert existence["mp_hits"] == 0
    assert existence["contribution"] == -0.1, (
        f"absence from MP must carry its written penalty: {existence}"
    )

    with patch("app.tools.data._query_materials_project",
               return_value={"results": [{"material_id": "mp-1",
                                          "energy_above_hull": 0.0}]}):
        present = tool.func(formula="Cu2O")
    present_by_name = {f["factor"]: f for f in present["factors"]}
    assert present_by_name["database_existence"]["contribution"] == 0.1
    assert present["score"] > absent["score"], (
        "a known on-hull material must outscore one MP has no entry for: "
        f"{present['score']} vs {absent['score']}"
    )


def test_materials_dependency_gates_carry_the_one_extras_shape():
    """A gate must hand back a command that WORKS.

    `prism-platform` is on no index (see `app.tools._extras.install_command`),
    so `pip install prism-platform[ml]` 404s — yet three gates in this area
    printed exactly that, and `structure_similarity` printed no install path
    at all. All four must go through `missing_extra_error`, which names the
    distributions directly and adds `requires_extra` for a caller to branch on.
    """
    import sys

    from app.tools.base import ToolRegistry
    from app.tools.materials.calculations import create_calphad_tools
    from app.tools.materials.informatics import create_informatics_tools
    from app.tools.materials.structure_desc import create_structure_desc_tools

    reg = ToolRegistry()
    create_informatics_tools(reg)
    create_structure_desc_tools(reg)
    create_calphad_tools(reg)

    # Setting a module to None in sys.modules makes `import` raise ImportError,
    # which is exactly the state each gate exists to handle.
    cases = [
        ("compute_descriptor", {"formulas": ["Cu2O"]},
         ["matminer.featurizers.composition"], "ml"),
        ("structure_similarity", {"query_formula": "Cu2O"},
         ["pymatgen.analysis.structure_matcher"], "ml"),
        ("describe_structure", {"cif": "irrelevant"},
         ["robocrystallographer.structure"], "ml"),
        ("hea_phase_stability", {"composition": "Fe0.7Cr0.2Ni0.1"},
         ["pycalphad"], "calphad"),
    ]
    for name, args, blocked, extra in cases:
        with patch.dict(sys.modules, {mod: None for mod in blocked}):
            out = reg.get(name).func(**args)
        assert "error" in out, f"{name} must degrade, not succeed: {out}"
        assert out.get("requires_extra") == extra, (
            f"{name} must name the extra it needs: {out}"
        )
        hint = out.get("install_hint", "")
        assert hint.startswith("pip install "), f"{name}: {out}"
        assert "prism-platform[" not in hint, (
            f"{name} hands the user a command that 404s: {hint!r}"
        )
