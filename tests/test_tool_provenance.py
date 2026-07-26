"""Every PRISM tool that reports a number must say how the number was made.

The bar these tests enforce: given a value a tool returned, a reader can name
the engine and its version, the exact bytes of every input file (content
hash), the parameters, the units, and the call that reproduces it — and can
tell a real result from a failure.

Companion to tests/test_provenance.py, which covers the MACE job bundle. The
shared builder lives in app/tools/_provenance.py; both emit the same keys.
"""

from __future__ import annotations

import json
import pathlib

import numpy as np
import pytest

from app.tools import _provenance as prov


#: What every bundle must carry, whatever produced it.
REQUIRED_KEYS = {
    "schema_version",
    "tool_name",
    "created_at_iso8601",
    "input",
    "units",
    "units_policy",
    "versions",
    "host",
    "wasGeneratedBy",
    "wasDerivedFrom",
    "wasAttributedTo",
    "reproduce",
}


def assert_reconstructable(bundle: dict, *, engine: str) -> None:
    """The reconstruction test, applied to one bundle."""
    missing = REQUIRED_KEYS - set(bundle)
    assert not missing, f"provenance missing {sorted(missing)}"
    gen = bundle["wasGeneratedBy"]
    assert gen["engine"] == engine
    # An unknown engine version is allowed ("absent"), a MISSING one is not:
    # the reader has to be told either way.
    assert gen["engine_version"], "engine_version must be recorded, even as 'absent'"
    assert gen["activity"]
    assert bundle["wasAttributedTo"]["agent"] == "PRISM"
    assert bundle["wasAttributedTo"]["prism_version"]
    assert bundle["reproduce"], "a result nobody can re-run has no provenance"
    assert isinstance(bundle["wasDerivedFrom"], list)
    # Must survive the trip to the agent / to disk.
    json.dumps(bundle)


# ---------------------------------------------------------------------------
# The shared builder
# ---------------------------------------------------------------------------

class TestProvenanceBuilder:
    def test_build_has_every_required_key(self):
        b = prov.build(
            tool_name="t", engine="pycalphad", engine_version="0.11.2",
            activity="pycalphad.equilibrium", inputs={"T": 1000},
            units={"gibbs_energy": "J/mol-atom"}, reproduce="calphad_compute(...)",
        )
        assert_reconstructable(b, engine="pycalphad")

    def test_file_ref_hashes_real_bytes(self, tmp_path):
        f = tmp_path / "db.tdb"
        f.write_text("PARAMETER G(FCC_A1,AL;0) 298.15 -1000; 6000 N !")
        ref = prov.file_ref(f, role="thermodynamic_database")
        assert ref["role"] == "thermodynamic_database"
        assert len(ref["sha256"]) == 64
        assert ref["size_bytes"] == f.stat().st_size
        # Editing the database in place must change the fingerprint —
        # otherwise "same database name" would imply "same numbers".
        f.write_text("PARAMETER G(FCC_A1,AL;0) 298.15 -2000; 6000 N !")
        assert prov.file_ref(f, role="x")["sha256"] != ref["sha256"]

    def test_file_ref_records_failure_rather_than_dropping_input(self, tmp_path):
        ref = prov.file_ref(tmp_path / "gone.tdb", role="thermodynamic_database")
        assert "error" in ref and "sha256" not in ref
        assert prov.file_ref(None, role="x")["error"]

    def test_attach_never_dresses_a_failure_as_a_measurement(self):
        failed = {"error": "Equilibrium calculation failed: singular matrix"}
        prov.attach(failed, prov.build(
            tool_name="t", engine="e", engine_version="1", activity="a",
            inputs={}, units={},
        ))
        assert "provenance" not in failed

    def test_versions_reports_absent_not_silence(self):
        v = prov.versions_of("definitely_not_a_real_module_xyz")
        assert v["definitely_not_a_real_module_xyz"] == "absent"
        assert v["prism"] and v["python"]


# ---------------------------------------------------------------------------
# CALPHAD — pycalphad
# ---------------------------------------------------------------------------

class TestCalphadProvenance:
    def _bridge(self, tmp_path):
        from app.tools.simulation.calphad_bridge import CalphadBridge

        (tmp_path / "alfe.tdb").write_text("$ minimal fixture\n")
        return CalphadBridge(base_dir=tmp_path)

    def test_bundle_identifies_the_database_bytes(self, tmp_path):
        bridge = self._bridge(tmp_path)
        b = bridge._provenance(
            activity="pycalphad.equilibrium", database_name="alfe",
            inputs={"conditions": {"T": 1000}}, reproduce="calphad_compute(...)",
        )
        assert_reconstructable(b, engine="pycalphad")
        tdb = [d for d in b["wasDerivedFrom"] if d["role"] == "thermodynamic_database"]
        assert len(tdb) == 1 and len(tdb[0]["sha256"]) == 64
        assert b["units"]["gibbs_energy"] == "J/mol-atom"
        assert b["units"]["temperature"] == "K"

    def test_units_map_is_the_verified_one(self):
        from app.tools.simulation.calphad_bridge import CALPHAD_UNITS

        # Verified against closed-form ideal mixing and the Al melting point
        # (see the calphad_bridge module docstring). Changing these without
        # re-verifying would silently relabel every downstream number.
        assert CALPHAD_UNITS["gibbs_energy"] == "J/mol-atom"
        assert CALPHAD_UNITS["temperature"] == "K"
        assert CALPHAD_UNITS["pressure"] == "Pa"


@pytest.mark.skipif(
    not __import__("app.tools.simulation.calphad_bridge", fromlist=["x"])
    .check_calphad_available(),
    reason="pycalphad not installed",
)
class TestCalphadLive:
    def test_equilibrium_result_carries_provenance(self, tmp_path):
        """End-to-end: a real pycalphad solve emits a complete bundle."""
        from app.tools.simulation.calphad_bridge import CalphadBridge

        (tmp_path / "ideal.tdb").write_text(IDEAL_TDB)
        bridge = CalphadBridge(base_dir=tmp_path)
        res = bridge.calculate_equilibrium(
            database_name="ideal", components=["AA", "BB"], phases=["TEST"],
            conditions={"T": 1000, "P": 101325, "X(BB)": 0.5},
        )
        assert "error" not in res, res
        assert_reconstructable(res["provenance"], engine="pycalphad")
        # The auto-added vacancy is a real change to what was computed.
        assert res["provenance"]["input"]["vacancy_added"] is True
        assert "VA" in res["provenance"]["input"]["components_used"]

    def test_phase_fractions_obey_the_lever_rule(self, tmp_path):
        """Al-30Fe at 1000 K sits in the Al5Fe2 + Al2Fe two-phase field.
        The lever rule fixes the fractions exactly, so this checks phase
        identification, the X(FE) condition mapping and the NP
        serialisation in one shot — none of which a units test reaches."""
        import shutil

        import pycalphad

        from app.tools.simulation.calphad_bridge import CalphadBridge

        src = (
            pathlib.Path(pycalphad.__file__).parent
            / "tests" / "databases" / "alfe.tdb"
        )
        if not src.exists():
            pytest.skip("pycalphad test databases not installed")
        shutil.copy2(src, tmp_path / "alfe.tdb")

        res = CalphadBridge(base_dir=tmp_path).calculate_equilibrium(
            database_name="alfe", components=["AL", "FE"], phases=None,
            conditions={"T": 1000, "P": 101325, "X(FE)": 0.3},
        )
        assert "error" not in res, res
        assert set(res["phases_present"]) == {"AL5FE2", "AL2FE"}
        # x_Fe: Al5Fe2 = 2/7, Al2Fe = 1/3. Lever rule at x_Fe = 0.30.
        x, x_a, x_b = 0.30, 2 / 7, 1 / 3
        f_al2fe = (x - x_a) / (x_b - x_a)
        assert res["phase_fractions"]["AL2FE"] == pytest.approx(f_al2fe, abs=1e-3)
        assert res["phase_fractions"]["AL5FE2"] == pytest.approx(1 - f_al2fe, abs=1e-3)

    def test_gibbs_energy_is_joules_per_mole_atom(self, tmp_path):
        """Known value: an ideal binary with zero end-member energies has
        GM(x=0.5) = -R*T*ln2 exactly. Pins the unit AND the Kelvin scale."""
        import math

        from pycalphad import variables as v

        from app.tools.simulation.calphad_bridge import CalphadBridge

        (tmp_path / "ideal.tdb").write_text(IDEAL_TDB)
        bridge = CalphadBridge(base_dir=tmp_path)
        T = 1000.0
        res = bridge.calculate_gibbs_energy(
            "ideal", ["AA", "BB"], ["TEST"], temperature=T)
        flat: list[float] = []

        def walk(o):
            if isinstance(o, list):
                for x in o:
                    walk(x)
            else:
                flat.append(float(o))

        walk(res["gibbs_energies"])
        got = min(flat)
        expected = -float(v.R) * T * math.log(2.0)
        assert abs(got - expected) / abs(expected) < 1e-7, (
            f"GM={got} J/mol-atom vs -R*T*ln2={expected}; unit or "
            "temperature-scale drift"
        )


IDEAL_TDB = """$ Ideal binary, both end members G=0 -> GM is pure mixing entropy.
 ELEMENT /-   ELECTRON_GAS              0.0000E+00  0.0000E+00  0.0000E+00!
 ELEMENT VA   VACUUM                    0.0000E+00  0.0000E+00  0.0000E+00!
 ELEMENT AA   TEST                      1.0000E+01  0.0000E+00  0.0000E+00!
 ELEMENT BB   TEST                      1.0000E+01  0.0000E+00  0.0000E+00!
 FUNCTION ZERO      298.15  0.0;                                6000 N !
 PHASE TEST  %  1  1.0  !
 CONSTITUENT TEST  :AA,BB : !
 PARAMETER G(TEST,AA;0)  298.15  +ZERO;                         6000 N !
 PARAMETER G(TEST,BB;0)  298.15  +ZERO;                         6000 N !
"""


# ---------------------------------------------------------------------------
# ML prediction — sklearn
# ---------------------------------------------------------------------------

@pytest.fixture
def trained_registry(tmp_path):
    """A real (tiny) trained model on disk, via the real registry."""
    from sklearn.ensemble import RandomForestRegressor

    from app.tools.ml.features import composition_features
    from app.tools.ml.registry import ModelRegistry

    formulas = ["Fe2O3", "SiO2", "Al2O3", "TiO2", "MgO", "CaO", "NaCl", "KCl"]
    rows = [composition_features(f) for f in formulas]
    names = sorted(set.intersection(*(set(r) for r in rows)))
    X = np.array([[r[k] for k in names] for r in rows])
    y = np.arange(len(formulas), dtype=float)

    model = RandomForestRegressor(n_estimators=3, random_state=0).fit(X, y)
    reg = ModelRegistry(models_dir=str(tmp_path))
    reg.save_model(model, "band_gap", "random_forest", {"mae": 0.5, "r2": 0.9},
                   feature_names=names)
    return reg


class TestPredictorProvenance:
    def test_prediction_is_reconstructable(self, trained_registry):
        from app.tools.ml.predictor import Predictor

        res = Predictor(registry=trained_registry).predict(
            "Fe2O3", "band_gap", "random_forest")
        assert "error" not in res, res
        assert isinstance(res["prediction"], float)
        # A bare number is not a result — the unit ships with it.
        assert res["unit"] == "eV"
        b = res["provenance"]
        assert_reconstructable(b, engine="sklearn")
        assert b["units"]["prediction"] == "eV"

        model_ref = [d for d in b["wasDerivedFrom"] if d["role"] == "trained_model"][0]
        assert len(model_ref["sha256"]) == 64
        run = [d for d in b["wasDerivedFrom"] if d["role"] == "training_run"][0]
        assert run["holdout_metrics"] == {"mae": 0.5, "r2": 0.9}
        assert run["trained_at"] != "unknown"
        assert run["feature_backend_id"] not in (None, "unrecorded")

    def test_unknown_property_says_unknown_rather_than_guessing(self, tmp_path):
        from sklearn.linear_model import LinearRegression

        from app.tools.ml.predictor import Predictor, property_unit
        from app.tools.ml.registry import ModelRegistry

        assert property_unit("some_lab_measured_column") == "unknown"
        assert property_unit("formation_energy_per_atom") == "eV/atom"

        from app.tools.ml.features import composition_features

        rows = [composition_features(f) for f in ("Fe2O3", "SiO2", "MgO")]
        names = sorted(set.intersection(*(set(r) for r in rows)))
        X = np.array([[r[k] for k in names] for r in rows])
        reg = ModelRegistry(models_dir=str(tmp_path))
        reg.save_model(LinearRegression().fit(X, np.array([1.0, 2.0, 3.0])),
                       "custom_prop", "linear", {}, feature_names=names)
        res = Predictor(registry=reg).predict("Fe2O3", "custom_prop", "linear")
        assert res["unit"] == "unknown"

    def test_refuses_to_score_across_a_featurizer_change(self, trained_registry,
                                                         monkeypatch):
        """Same feature NAMES, different NUMBERS is the invisible failure —
        it must be an error, not a plausible-looking value."""
        from app.tools.ml import predictor as predictor_mod

        monkeypatch.setattr(predictor_mod, "feature_backend_id",
                            lambda: "basic/v999-from-the-future")
        res = predictor_mod.Predictor(registry=trained_registry).predict(
            "Fe2O3", "band_gap", "random_forest")
        assert "error" in res
        assert "backend changed" in res["error"]

    def test_registry_records_the_featurizer_identity(self, trained_registry):
        from app.tools.ml.features import feature_backend_id

        meta = trained_registry.load_meta("band_gap", "random_forest")
        assert meta["feature_backend_id"] == feature_backend_id()


# ---------------------------------------------------------------------------
# Dataset-wide prediction skill
# ---------------------------------------------------------------------------

class TestPredictPropertiesProvenance:
    def test_result_carries_provenance_and_names_the_in_sample_caveat(
        self, tmp_path, monkeypatch
    ):
        import pandas as pd

        from app.tools.data_collectors.store import DataStore
        from app.tools.skills.prediction import _predict_properties

        df = pd.DataFrame({
            "formula": ["Fe2O3", "SiO2", "Al2O3", "TiO2", "MgO", "CaO", "NaCl"],
            "band_gap": [2.0, 9.0, 8.8, 3.2, 7.8, 7.0, 8.5],
        })
        monkeypatch.setattr(DataStore, "load", lambda self, name: df.copy())
        saved = {}
        monkeypatch.setattr(DataStore, "save",
                            lambda self, d, name: saved.update({name: d}))
        monkeypatch.setenv("PRISM_ML_MODELS_DIR", str(tmp_path))

        res = _predict_properties(dataset_name="d", properties=["band_gap"],
                                  algorithm="random_forest")
        assert "error" not in res, res
        assert res["predictions"] == {"band_gap": "predicted_band_gap"}
        assert "holdout_metrics" in res["in_sample_warning"]

        model_info = res["models"]["band_gap"]
        assert model_info["unit"] == "eV"
        assert model_info["holdout_metrics"] is not None
        assert len(model_info["model_file"]["sha256"]) == 64
        assert_reconstructable(res["provenance"], engine="sklearn")
        assert res["provenance"]["units"]["predicted_band_gap"] == "eV"


# ---------------------------------------------------------------------------
# pyiron simulation results
# ---------------------------------------------------------------------------

class _FakeJob:
    job_name = "prism_lammps_abc123"
    status = "finished"
    potential = "2001--Mishin-Y--Al--LAMMPS--ipr1"

    def __getitem__(self, key):
        if key == "energy_tot":
            return -3.36
        raise KeyError(key)


class TestSimJobProvenance:
    def test_results_name_the_engine_potential_and_what_failed(self, monkeypatch):
        from app.tools import sim_tools
        from app.tools.simulation import bridge as bridge_mod

        monkeypatch.setattr(sim_tools, "_guard", lambda: None)

        class _Jobs:
            def get(self, jid):
                return _FakeJob()

        class _Bridge:
            jobs = _Jobs()

        monkeypatch.setattr(bridge_mod, "get_bridge", lambda: _Bridge())

        res = sim_tools._get_job_results(
            job_id="j1", properties=["energy_tot", "forces"])
        assert res["energy_tot"] == -3.36
        # A property the code never wrote must be distinguishable from one
        # that failed to read — `None` alone said neither.
        assert res["forces"] is None
        assert res["unreadable_properties"][0]["property"] == "forces"

        b = res["provenance"]
        assert_reconstructable(b, engine="pyiron")
        assert b["input"]["potential"] == _FakeJob.potential
        assert b["input"]["job_class"] == "_FakeJob"
        # PRISM must not invent a unit it did not verify.
        assert "as reported by pyiron" in b["units"]["energy_tot"]
        assert "no unit conversion" in b["units_policy"]


# ---------------------------------------------------------------------------
# Physics-correctness fixes that provenance alone would not catch
# ---------------------------------------------------------------------------

class TestCompositionFeatureCorrectness:
    def test_nested_groups_are_parsed(self):
        from app.tools.ml.features import _parse_formula

        # The old flat regex read this as Ca1 O1 H2.
        assert _parse_formula("Ca(OH)2") == {"Ca": 1.0, "O": 2.0, "H": 2.0}
        assert _parse_formula("Al2(SO4)3") == {"Al": 2.0, "S": 3.0, "O": 12.0}
        # Existing behaviour preserved.
        assert _parse_formula("Fe2O3") == {"Fe": 2.0, "O": 3.0}

    def test_weighted_average_is_an_average(self):
        """La is not in the 44-element table. The mean over the elements that
        ARE covered must be a real mean, not one scaled down by the missing
        fraction."""
        from app.tools.ml.features import ELEMENT_DATA, _composition_features_basic

        assert "La" not in ELEMENT_DATA
        f = _composition_features_basic("LaFeO3")
        covered = {"Fe": 1.0, "O": 3.0}
        total = sum(covered.values())
        expected = sum(
            ELEMENT_DATA[el]["electronegativity"] * n / total
            for el, n in covered.items()
        )
        assert f["avg_electronegativity"] == pytest.approx(expected)
        # min/max/range are over covered elements and unaffected.
        assert f["max_electronegativity"] == ELEMENT_DATA["O"]["electronegativity"]

    def test_backend_id_carries_a_version(self):
        from app.tools.ml.features import feature_backend_id, get_feature_backend

        bid = feature_backend_id()
        assert bid.startswith(get_feature_backend() + "/")


class TestElasticPhysics:
    """The one place under app/tools that implements physics itself rather
    than delegating: Voigt-Reuss-Hill averaging of a stiffness tensor.
    Verified against published single-crystal elastic constants."""

    @staticmethod
    def _cubic(c11, c12, c44):
        C = np.zeros((6, 6))
        C[0, 0] = C[1, 1] = C[2, 2] = c11
        C[0, 1] = C[1, 0] = C[0, 2] = C[2, 0] = C[1, 2] = C[2, 1] = c12
        C[3, 3] = C[4, 4] = C[5, 5] = c44
        return C

    @pytest.mark.parametrize(
        "name,c11,c12,c44,lit_K,lit_G,lit_E,lit_nu",
        [
            # Single-crystal C_ij (Simmons & Wang) -> polycrystalline handbook
            # values. 3% covers the spread between reported measurements.
            ("Al", 107.3, 60.9, 28.3, 76.0, 26.0, 70.0, 0.35),
            ("Cu", 168.4, 121.4, 75.4, 137.0, 48.0, 130.0, 0.34),
            ("W", 522.4, 204.4, 160.8, 310.0, 161.0, 411.0, 0.28),
        ],
    )
    def test_matches_published_moduli(self, name, c11, c12, c44,
                                      lit_K, lit_G, lit_E, lit_nu):
        from app.tools.simulation.mace.core.elastic import voigt_reuss_hill

        K, G, E, nu, _ = voigt_reuss_hill(self._cubic(c11, c12, c44))
        assert K == pytest.approx(lit_K, rel=0.03), f"{name} bulk modulus"
        assert G == pytest.approx(lit_G, rel=0.03), f"{name} shear modulus"
        assert E == pytest.approx(lit_E, rel=0.03), f"{name} Young's modulus"
        assert nu == pytest.approx(lit_nu, abs=0.02), f"{name} Poisson ratio"

    def test_isotropic_input_round_trips_exactly(self):
        """For an isotropic C the Voigt and Reuss bounds coincide, so VRH must
        return the K and G that built it — a units or factor error anywhere in
        the averaging breaks this."""
        from app.tools.simulation.mace.core.elastic import voigt_reuss_hill

        K0, G0 = 100.0, 40.0
        lam = K0 - 2 * G0 / 3
        C = np.zeros((6, 6))
        for i in range(3):
            for j in range(3):
                C[i, j] = lam + (2 * G0 if i == j else 0)
        for i in range(3, 6):
            C[i, i] = G0
        K, G, E, nu, _ = voigt_reuss_hill(C)
        assert K == pytest.approx(K0, rel=1e-12)
        assert G == pytest.approx(G0, rel=1e-12)
        assert E == pytest.approx(9 * K0 * G0 / (3 * K0 + G0), rel=1e-12)
        assert nu == pytest.approx((3 * K0 - 2 * G0) / (2 * (3 * K0 + G0)), rel=1e-12)

    def test_stress_unit_constant(self):
        from app.tools.simulation.mace.core.elastic import EV_PER_A3_TO_GPA

        # 1 eV/A^3 = e[C] / 1e-30 m^3 -> Pa, /1e9 -> GPa, with the SI-2019
        # exact elementary charge.
        exact = 1.602176634e-19 / 1e-30 / 1e9
        assert EV_PER_A3_TO_GPA == pytest.approx(exact, rel=1e-8)

    def test_failed_averaging_is_indeterminate_not_brittle(self):
        """NaN < 0.57 is False, so a singular stiffness tensor used to come
        back as a confident 'brittle' that also failed the AM
        manufacturability gate. Neither claim had anything behind it."""
        from app.tools.simulation.mace.core.elastic import summarize_elastic

        r = summarize_elastic(np.zeros((6, 6)))
        assert r.pugh_verdict == "indeterminate"
        assert r.am_manufacturability_passed is None
        assert "error" in r.extras

    def test_real_tensor_still_gets_a_verdict(self):
        from app.tools.simulation.mace.core.elastic import summarize_elastic

        r = summarize_elastic(self._cubic(107.3, 60.9, 28.3))  # Al
        assert r.pugh_verdict == "ductile"  # G/B ~ 0.34 < 0.57
        assert r.am_manufacturability_passed is True
        assert "error" not in r.extras


class TestSelectionHonesty:
    def _store(self, monkeypatch):
        import pandas as pd

        from app.tools.data_collectors.store import DataStore

        df = pd.DataFrame({"formula": ["A", "B", "C"], "band_gap": [1.0, 3.0, 2.0]})
        monkeypatch.setattr(DataStore, "load", lambda self, name: df.copy())
        monkeypatch.setattr(DataStore, "save", lambda self, d, name: None)
        return df

    def test_unknown_sort_column_is_an_error_not_file_order(self, monkeypatch):
        from app.tools.skills.selection import _select_materials

        self._store(monkeypatch)
        res = _select_materials(dataset_name="d", sort_by="bulk_modulus", top_n=2)
        assert "error" in res and "Cannot sort by" in res["error"]

    def test_unknown_criterion_is_an_error_not_an_unfiltered_result(self, monkeypatch):
        from app.tools.skills.selection import _select_materials

        self._store(monkeypatch)
        res = _select_materials(dataset_name="d", criteria={"bulk_modulus_min": 100})
        assert "error" in res and "bulk_modulus_min" in res["error"]

    def test_ranking_direction_is_explicit(self, monkeypatch):
        from app.tools.skills.selection import _select_materials

        self._store(monkeypatch)
        asc = _select_materials(dataset_name="d", sort_by="band_gap", top_n=1)
        desc = _select_materials(dataset_name="d", sort_by="band_gap", top_n=1,
                                 descending=True)
        assert asc["sort_order"] == "ascending"
        assert desc["sort_order"] == "descending"
        assert asc["ranked"] is True
