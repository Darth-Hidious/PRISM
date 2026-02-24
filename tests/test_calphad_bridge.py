"""Tests for the CALPHAD bridge layer."""

import sys
import types
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from app.simulation.calphad_bridge import (
    CalphadBridge,
    DatabaseStore,
    _calphad_missing_error,
    _ensure_vacancy,
    check_calphad_available,
    get_calphad_bridge,
)


class TestCheckCalphadAvailable:
    def test_returns_bool(self):
        result = check_calphad_available()
        assert isinstance(result, bool)

    def test_returns_false_when_missing(self):
        # pycalphad is not installed in our test environment
        assert check_calphad_available() is False


class TestCalphadMissingError:
    def test_returns_error_dict(self):
        result = _calphad_missing_error()
        assert "error" in result
        assert "pycalphad" in result["error"]
        assert "pip install" in result["error"]


class TestEnsureVacancy:
    def test_adds_va(self):
        assert "VA" in _ensure_vacancy(["Al", "Ni"])

    def test_no_duplicate_va(self):
        result = _ensure_vacancy(["Al", "VA", "Ni"])
        assert result.count("VA") == 1

    def test_preserves_order(self):
        result = _ensure_vacancy(["Al", "Ni"])
        assert result[0] == "Al"
        assert result[1] == "Ni"
        assert result[2] == "VA"


class TestDatabaseStore:
    def test_list_empty_dir(self, tmp_path):
        store = DatabaseStore(base_dir=tmp_path)
        assert store.list_databases() == []

    def test_list_with_tdb_files(self, tmp_path):
        (tmp_path / "sgte.tdb").write_text("$ SGTE database")
        (tmp_path / "nist.tdb").write_text("$ NIST database")
        (tmp_path / "not_a_tdb.txt").write_text("ignore me")

        store = DatabaseStore(base_dir=tmp_path)
        databases = store.list_databases()
        assert len(databases) == 2
        names = {d["name"] for d in databases}
        assert "sgte" in names
        assert "nist" in names
        assert all("size_kb" in d for d in databases)

    def test_import_database_success(self, tmp_path):
        src_dir = tmp_path / "source"
        src_dir.mkdir()
        src_file = src_dir / "test.tdb"
        src_file.write_text("$ Test TDB content")

        dest_dir = tmp_path / "databases"
        store = DatabaseStore(base_dir=dest_dir)
        result = store.import_database(str(src_file))

        assert result["imported"] is True
        assert result["name"] == "test"
        assert (dest_dir / "test.tdb").exists()

    def test_import_database_custom_name(self, tmp_path):
        src_dir = tmp_path / "source"
        src_dir.mkdir()
        src_file = src_dir / "mydata.tdb"
        src_file.write_text("$ data")

        store = DatabaseStore(base_dir=tmp_path / "db")
        result = store.import_database(str(src_file), name="custom")

        assert result["name"] == "custom"
        assert (tmp_path / "db" / "custom.tdb").exists()

    def test_import_file_not_found(self, tmp_path):
        store = DatabaseStore(base_dir=tmp_path)
        result = store.import_database("/nonexistent/path.tdb")
        assert "error" in result

    def test_import_non_tdb(self, tmp_path):
        non_tdb = tmp_path / "data.csv"
        non_tdb.write_text("a,b,c")

        store = DatabaseStore(base_dir=tmp_path / "db")
        result = store.import_database(str(non_tdb))
        assert "error" in result

    def test_load_not_found(self, tmp_path):
        store = DatabaseStore(base_dir=tmp_path)
        assert store.load("nonexistent") is None

    def test_load_with_mock(self, tmp_path):
        (tmp_path / "test.tdb").write_text("$ Mock TDB")

        mock_db = MagicMock()
        mock_pycalphad = types.ModuleType("pycalphad")
        mock_pycalphad.Database = MagicMock(return_value=mock_db)

        with patch.dict(sys.modules, {"pycalphad": mock_pycalphad}):
            store = DatabaseStore(base_dir=tmp_path)
            result = store.load("test")
            assert result is mock_db

    def test_load_caches(self, tmp_path):
        (tmp_path / "test.tdb").write_text("$ Mock TDB")

        mock_db = MagicMock()
        mock_pycalphad = types.ModuleType("pycalphad")
        mock_pycalphad.Database = MagicMock(return_value=mock_db)

        with patch.dict(sys.modules, {"pycalphad": mock_pycalphad}):
            store = DatabaseStore(base_dir=tmp_path)
            r1 = store.load("test")
            r2 = store.load("test")
            assert r1 is r2
            # Only called once due to caching
            mock_pycalphad.Database.assert_called_once()

    def test_get_phases_not_found(self, tmp_path):
        store = DatabaseStore(base_dir=tmp_path)
        assert store.get_phases("nonexistent") is None

    def test_get_phases_with_mock(self, tmp_path):
        (tmp_path / "test.tdb").write_text("$ Mock")

        # Create a mock DB with phases
        mock_phase_fcc = MagicMock()
        mock_phase_fcc.constituents = [{"AL", "NI"}]
        mock_phase_bcc = MagicMock()
        mock_phase_bcc.constituents = [{"FE"}]

        mock_db = MagicMock()
        mock_db.phases = {"FCC_A1": mock_phase_fcc, "BCC_A2": mock_phase_bcc}

        mock_pycalphad = types.ModuleType("pycalphad")
        mock_pycalphad.Database = MagicMock(return_value=mock_db)

        with patch.dict(sys.modules, {"pycalphad": mock_pycalphad}):
            store = DatabaseStore(base_dir=tmp_path)
            phases = store.get_phases("test")
            assert "FCC_A1" in phases
            assert "BCC_A2" in phases


class TestCalphadBridge:
    def test_database_not_found(self, tmp_path):
        bridge = CalphadBridge(base_dir=tmp_path)
        result = bridge.calculate_equilibrium(
            database_name="nonexistent",
            components=["Al", "Ni"],
            phases=None,
            conditions={"T": 1000, "P": 101325},
        )
        assert "error" in result

    def test_calculate_equilibrium_with_mock(self, tmp_path):
        import numpy as np

        (tmp_path / "test.tdb").write_text("$ Mock")

        # Mock the equilibrium result (xarray-like)
        mock_eq = MagicMock()
        mock_eq.Phase.values.squeeze.return_value = np.array(["FCC_A1", "BCC_A2"])
        mock_eq.NP.values.squeeze.return_value = np.array([0.6, 0.4])
        mock_eq.GM.values.squeeze.return_value = np.float64(-50000.0)

        mock_db = MagicMock()
        mock_db.phases = {"FCC_A1": MagicMock(), "BCC_A2": MagicMock()}

        mock_v = MagicMock()
        mock_v.T = "T"
        mock_v.P = "P"
        mock_v.X = MagicMock(side_effect=lambda e: f"X({e})")

        mock_pycalphad = types.ModuleType("pycalphad")
        mock_pycalphad.Database = MagicMock(return_value=mock_db)
        mock_pycalphad.equilibrium = MagicMock(return_value=mock_eq)
        mock_pycalphad.variables = mock_v

        with patch.dict(sys.modules, {"pycalphad": mock_pycalphad}):
            bridge = CalphadBridge(base_dir=tmp_path)
            result = bridge.calculate_equilibrium(
                database_name="test",
                components=["Al", "Ni"],
                phases=None,
                conditions={"T": 1000, "P": 101325},
            )
            assert "phases_present" in result
            assert result["database"] == "test"

    def test_calculate_gibbs_energy_with_mock(self, tmp_path):
        import numpy as np

        (tmp_path / "test.tdb").write_text("$ Mock")

        mock_calc = MagicMock()
        mock_calc.GM.values.squeeze.return_value = np.array([-40000.0, -35000.0])

        mock_db = MagicMock()
        mock_v = MagicMock()

        mock_pycalphad = types.ModuleType("pycalphad")
        mock_pycalphad.Database = MagicMock(return_value=mock_db)
        mock_pycalphad.calculate = MagicMock(return_value=mock_calc)
        mock_pycalphad.variables = mock_v

        with patch.dict(sys.modules, {"pycalphad": mock_pycalphad}):
            bridge = CalphadBridge(base_dir=tmp_path)
            result = bridge.calculate_gibbs_energy(
                database_name="test",
                components=["Al", "Ni"],
                phases=["FCC_A1"],
                temperature=1000,
            )
            assert "gibbs_energies" in result
            assert result["temperature"] == 1000

    def test_calculate_phase_diagram_db_not_found(self, tmp_path):
        bridge = CalphadBridge(base_dir=tmp_path)
        result = bridge.calculate_phase_diagram(
            database_name="nonexistent",
            components=["Al", "Ni"],
        )
        assert "error" in result


class TestGetCalphadBridge:
    def test_returns_singleton(self):
        import app.simulation.calphad_bridge as mod

        # Reset singleton
        mod._bridge = None
        b1 = get_calphad_bridge()
        b2 = get_calphad_bridge()
        assert b1 is b2
        # Clean up
        mod._bridge = None
