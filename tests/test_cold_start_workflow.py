"""Tests for PRISM Addendum E cold-start mechanics and workflow wiring."""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np
import pytest
import yaml

from app.tools.ml.cold_start import (
    MACE_MP0_MODEL,
    fine_tune_linear_target_head,
    label_bcc_stability,
    run_active_learning,
    run_calphad_augmentation,
    run_foundation_bootstrap,
    sample_replay_batch_indices,
    sample_simplex,
)
from app.tools.skills.registry import load_builtin_skills


def test_simplex_sampler_is_seeded_uniform_simplex_shape():
    elements = ["Mo", "Nb", "Ta", "W", "Hf"]
    first = sample_simplex(elements, n_samples=64, seed=27)
    second = sample_simplex(elements, n_samples=64, seed=27)

    assert first == second
    assert len(first) == 64
    for composition in first:
        assert list(composition) == elements
        assert all(fraction > 0.0 for fraction in composition.values())
        assert sum(composition.values()) == pytest.approx(1.0, abs=1e-12)


def test_stability_label_uses_strict_greater_than_threshold():
    assert label_bcc_stability(0.9500001) == 1
    assert label_bcc_stability(0.95) == 0
    assert label_bcc_stability(0.9499999) == 0


def test_replay_sampler_and_training_use_exact_80_20_mix():
    rng = np.random.default_rng(5)
    target_idx, replay_idx = sample_replay_batch_indices(3, 11, 20, rng)
    assert len(target_idx) == 16
    assert len(replay_idx) == 4

    foundation_weights = np.zeros((2, 1))
    foundation_bias = np.zeros(1)
    result = fine_tune_linear_target_head(
        foundation_weights,
        foundation_bias,
        target_features=np.array([[1.0, 0.0], [0.0, 1.0]]),
        target_labels=np.array([1.0, 2.0]),
        replay_features=np.array([[1.0, 1.0], [2.0, 1.0]]),
        replay_labels=np.array([0.5, 0.75]),
        epochs=2,
        steps_per_epoch=3,
        batch_size=20,
        learning_rate=0.01,
        seed=5,
    )

    assert result["head_initialized_from_foundation"] is True
    assert result["foundation_head_unchanged"] is True
    assert result["replay_mix"]["target_fraction"] == pytest.approx(0.8)
    assert result["replay_mix"]["foundation_fraction"] == pytest.approx(0.2)
    assert np.array_equal(foundation_weights, np.zeros((2, 1)))
    assert not np.array_equal(result["target_weights"], foundation_weights)


def _write_mace_artifact(path: Path, **arrays):
    np.savez_compressed(
        path,
        **arrays,
        model_repo=MACE_MP0_MODEL["repo_id"],
        model_file=MACE_MP0_MODEL["filename"],
        model_license=MACE_MP0_MODEL["license"],
        feature_method="test frozen descriptor fixture",
    )


def test_phase0_artifact_training_copies_head_and_records_replay(tmp_path):
    head = tmp_path / "head_a.npz"
    target = tmp_path / "target.npz"
    replay = tmp_path / "replay.npz"
    output = tmp_path / "head_b.npz"
    _write_mace_artifact(head, weights=np.zeros((2, 1)), bias=np.zeros(1))
    _write_mace_artifact(
        target,
        features=np.array([[1.0, 0.0], [0.0, 1.0]]),
        targets=np.array([1.0, 2.0]),
    )
    _write_mace_artifact(
        replay,
        features=np.array([[1.0, 1.0], [2.0, 1.0]]),
        targets=np.array([0.5, 0.75]),
    )

    phase0 = run_foundation_bootstrap(
        foundation_head_path=head,
        target_data_path=target,
        foundation_replay_path=replay,
        output_path=output,
        epochs=1,
        steps_per_epoch=2,
        batch_size=10,
        learning_rate=0.01,
        seed=3,
    )["phase0"]

    assert phase0["status"] == "completed_scope_limited"
    assert phase0["stage_1_foundation_pretraining"]["status"] == "not_performed"
    assert phase0["stage_2_target_head_initialization"]["copied_from_head_A"] is True
    mix = phase0["stage_3_replay_fine_tuning"]["replay_mix"]
    assert mix["target_fraction"] == pytest.approx(0.8)
    assert mix["foundation_fraction"] == pytest.approx(0.2)
    assert output.is_file()
    assert phase0["target_head_artifact"]["sha256"]


class _FakeDatabase:
    elements = {"AL", "ZR", "VA"}
    phases = {"FCC_A1": object()}


class _FakeStore:
    def __init__(self, base_dir: Path):
        self.base_dir = base_dir

    def list_databases(self):
        return [{"name": "alzr_probe", "path": str(self.base_dir / "alzr_probe.tdb")}]

    def load(self, name):
        return _FakeDatabase() if name == "alzr_probe" else None

    def get_phases(self, name, components=None):
        return ["FCC_A1"] if name == "alzr_probe" else None


class _FakeBridge:
    def __init__(self, base_dir: Path):
        self.databases = _FakeStore(base_dir)


def test_missing_database_path_fails_honestly_without_sampling(tmp_path, monkeypatch):
    bridge = _FakeBridge(tmp_path / "databases")
    monkeypatch.setattr(
        "app.tools.simulation.calphad_bridge.check_calphad_available", lambda: True
    )
    monkeypatch.setattr(
        "app.tools.simulation.calphad_bridge.get_calphad_bridge", lambda: bridge
    )
    monkeypatch.setattr(
        "app.tools.ml.cold_start.sample_simplex",
        lambda *args, **kwargs: pytest.fail("coverage preflight must happen before sampling"),
    )

    phase1 = run_calphad_augmentation(
        elements=["Mo", "Nb", "Ta", "W", "Hf"],
        temperature_K=1773,
        n_samples=10000,
        seed=0,
    )["phase1"]

    expected = (
        "no thermodynamic database covering element set {Mo, Nb, Ta, W, Hf} found in "
        f"{bridge.databases.base_dir} (available: alzr_probe). A CALPHAD result "
        "requires a TDB for this system — none is synthesized."
    )
    assert phase1["status"] == "unavailable"
    assert phase1["reason"] == expected
    assert phase1["dataset"] == []
    assert phase1["tier"] == 2
    assert phase1["provenance"]["wasGeneratedBy"]["engine"] == "pycalphad"


def test_active_learning_formula_and_exact_d_optimal_selection():
    candidates = [
        {
            "id": "a",
            "composition": {"Mo": 0.5, "Nb": 0.5},
            "y_stability": 1,
            "sigma": 1.0,
            "mu": 2.0,
            "d_pareto": 3.0,
            "rho": 0.25,
            "descriptor": [1.0, 0.0],
            "provenance": {"source": "test fixture"},
        },
        {
            "id": "b",
            "composition": {"Mo": 0.4, "Nb": 0.6},
            "y_stability": 1,
            "sigma": 0.5,
            "mu": 1.0,
            "d_pareto": 0.5,
            "rho": 0.5,
            "descriptor": [0.0, 1.0],
            "provenance": {"source": "test fixture"},
        },
        {
            "id": "c",
            "composition": {"Mo": 0.6, "Nb": 0.4},
            "y_stability": 1,
            "sigma": 0.1,
            "mu": 0.1,
            "d_pareto": 0.1,
            "rho": 0.9,
            "descriptor": [1.0, 1.0],
            "provenance": {"source": "test fixture"},
        },
    ]
    phase2 = run_active_learning(candidates=candidates, batch_size=2)["phase2"]

    assert phase2["status"] == "completed"
    assert phase2["scores"][0]["alpha"] == pytest.approx(
        0.4 * 1.0 + 0.2 * 2.0 + 0.3 * 3.0 + 0.1 * (1.0 - 0.25)
    )
    assert phase2["d_optimality"]["determinant"] == pytest.approx(1.0)
    assert len(phase2["selected"]) == 2


def test_workflow_manifest_binds_only_registered_skills():
    manifest = yaml.safe_load(Path(".prism/workflows/cold_start.yaml").read_text())
    registered = {skill.name for skill in load_builtin_skills().list_skills()}

    assert manifest["kind"] == "skill_workflow"
    assert manifest["version"] == "1.0.0"
    assert "MACE-MP-0 replaces" in manifest["description"]
    assert [step["name"] for step in manifest["steps"]] == [
        "phase0_foundation_bootstrap",
        "phase1_calphad_augmentation",
        "phase2_active_learning",
        "phase3_campaign_handoff",
    ]
    assert all(step["skill"] in registered for step in manifest["steps"])
    assert json.dumps(manifest)
