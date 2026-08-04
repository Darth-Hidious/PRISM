# Copyright (c) 2025-2026 Mirdyne. Licensed under MIT License.
"""Scientific kernels for Addendum E's cold-start workflow.

This module contains the deterministic mechanics used by the workflow skills;
the YAML manifest contains orchestration only. No routine invents scientific
measurements. Missing models, datasets, thermodynamic databases, or descriptor
signals produce explicit ``unavailable`` results.

MACE-MH-1 is intentionally not used. Its weights are ASL-licensed for academic
non-commercial use, while PRISM bills for predictions. Phase 0 therefore uses
MIT-licensed MACE-MP-0 descriptor/head artifacts and never weakens the
``MACE_ACCEPT_ASL_LICENSE`` gate in the simulation calculator.
"""

from __future__ import annotations

from itertools import combinations
import json
import math
from pathlib import Path
import re
from typing import Any

import numpy as np

from app.tools import _provenance as prov

COLD_START_VERSION = "1.0.0"
MACE_MP0_MODEL = {
    "repo_id": "mace-foundations/mace-mp-0",
    "filename": "mace-mp-0b3-medium.model",
    "license": "MIT",
}
MACE_SUBSTITUTION_REASON = (
    "Addendum E specifies MACE-MH-1, but its ASL weights do not permit this "
    "commercial prediction service. PRISM uses MIT-licensed MACE-MP-0 instead."
)
DEFAULT_ACQUISITION_WEIGHTS = {
    "uncertainty": 0.4,
    "exploitation": 0.2,
    "improvement": 0.3,
    "diversity": 0.1,
}
_ELEMENT_RE = re.compile(r"^[A-Z][a-z]?$", re.ASCII)


def _json_value(value: Any, field: str) -> Any:
    """Decode a JSON-shaped CLI string while preserving native values."""
    if not isinstance(value, str):
        return value
    stripped = value.strip()
    if stripped.lower() in {"", "null", "none"}:
        return None
    try:
        return json.loads(stripped)
    except json.JSONDecodeError as exc:
        raise ValueError(f"{field} must be valid JSON: {exc.msg}") from exc


def normalize_elements(value: Any) -> list[str]:
    """Normalize a JSON list or comma-delimited element set."""
    if isinstance(value, str):
        stripped = value.strip()
        if stripped.startswith("["):
            value = _json_value(value, "elements")
        else:
            value = [part.strip() for part in stripped.split(",") if part.strip()]
    if not isinstance(value, (list, tuple)) or len(value) < 2:
        raise ValueError("elements must contain at least two element symbols")
    elements = [str(element).strip() for element in value]
    invalid = [element for element in elements if not _ELEMENT_RE.fullmatch(element)]
    if invalid:
        raise ValueError(f"invalid element symbols: {invalid}")
    if len(set(elements)) != len(elements):
        raise ValueError("elements must not contain duplicates")
    return elements


def sample_simplex(elements: list[str], n_samples: int, seed: int) -> list[dict[str, float]]:
    """Uniformly sample the interior of an element simplex.

    Dirichlet(alpha=1) is the uniform distribution over the simplex. The
    seeded NumPy PCG64 generator makes the workflow reproducible.
    """
    elements = normalize_elements(elements)
    if isinstance(n_samples, bool) or int(n_samples) != n_samples or n_samples <= 0:
        raise ValueError("n_samples must be a positive integer")
    rng = np.random.default_rng(int(seed))
    samples = rng.dirichlet(np.ones(len(elements)), size=int(n_samples))
    return [
        {element: float(fraction) for element, fraction in zip(elements, row)}
        for row in samples
    ]


def label_bcc_stability(phi_bcc: float, threshold: float = 0.95) -> int:
    """Algorithm 2's strict single-phase BCC threshold."""
    value = float(phi_bcc)
    if not math.isfinite(value) or not 0.0 <= value <= 1.0:
        raise ValueError(f"phi_bcc must be finite and in [0, 1], got {phi_bcc!r}")
    return int(value > float(threshold))


def initialize_target_head(
    foundation_weights: np.ndarray,
    foundation_bias: np.ndarray,
) -> tuple[np.ndarray, np.ndarray]:
    """Stage 2: copy Head-A parameters into an independent Head-B."""
    return np.array(foundation_weights, dtype=float, copy=True), np.array(
        foundation_bias, dtype=float, copy=True
    )


def sample_replay_batch_indices(
    n_target: int,
    n_foundation: int,
    batch_size: int,
    rng: np.random.Generator,
) -> tuple[np.ndarray, np.ndarray]:
    """Sample one exact 80/20 target/foundation replay batch."""
    if n_target <= 0 or n_foundation <= 0:
        raise ValueError("target and foundation replay datasets must be non-empty")
    if batch_size <= 0 or batch_size % 5:
        raise ValueError("batch_size must be a positive multiple of 5 for an exact 80/20 mix")
    n_replay = batch_size // 5
    n_sparse = batch_size - n_replay
    return (
        rng.integers(0, n_target, size=n_sparse),
        rng.integers(0, n_foundation, size=n_replay),
    )


def fine_tune_linear_target_head(
    foundation_weights: np.ndarray,
    foundation_bias: np.ndarray,
    target_features: np.ndarray,
    target_labels: np.ndarray,
    replay_features: np.ndarray,
    replay_labels: np.ndarray,
    *,
    epochs: int,
    steps_per_epoch: int,
    batch_size: int,
    learning_rate: float,
    seed: int,
) -> dict[str, Any]:
    """Fine-tune only a cloned dense target head with foundation replay.

    Features are frozen MACE-MP-0 descriptors prepared outside this routine.
    The supplied Head-A arrays are never mutated; only the cloned Head-B
    arrays receive gradient updates. This is the small-scale, testable scope of
    Phase 0. Full MACE foundation pre-training remains an HPC workload.
    """

    def matrix(value: Any, name: str) -> np.ndarray:
        array = np.asarray(value, dtype=float)
        if array.ndim == 1:
            array = array.reshape(-1, 1)
        if array.ndim != 2 or not array.size or not np.isfinite(array).all():
            raise ValueError(f"{name} must be a non-empty finite 2D array")
        return array

    weights = matrix(foundation_weights, "foundation_weights")
    bias = np.asarray(foundation_bias, dtype=float).reshape(-1)
    target_x = matrix(target_features, "target_features")
    target_y = matrix(target_labels, "target_labels")
    replay_x = matrix(replay_features, "replay_features")
    replay_y = matrix(replay_labels, "replay_labels")
    if weights.shape != (target_x.shape[1], target_y.shape[1]):
        raise ValueError(
            "foundation head shape does not match target features/labels: "
            f"{weights.shape} != {(target_x.shape[1], target_y.shape[1])}"
        )
    if replay_x.shape[1] != weights.shape[0] or replay_y.shape[1] != weights.shape[1]:
        raise ValueError("foundation replay dimensions do not match Head-A")
    if len(target_x) != len(target_y) or len(replay_x) != len(replay_y):
        raise ValueError("each feature matrix must have the same rows as its labels")
    if bias.shape != (weights.shape[1],):
        raise ValueError(f"foundation_bias must have shape {(weights.shape[1],)}")
    if epochs <= 0 or steps_per_epoch <= 0 or learning_rate <= 0:
        raise ValueError("epochs, steps_per_epoch, and learning_rate must be positive")

    foundation_weights_snapshot = weights.copy()
    foundation_bias_snapshot = bias.copy()
    target_weights, target_bias = initialize_target_head(weights, bias)
    initialized_exactly = bool(
        np.array_equal(target_weights, weights) and np.array_equal(target_bias, bias)
    )
    rng = np.random.default_rng(int(seed))
    target_seen = replay_seen = 0

    def mse(x: np.ndarray, y: np.ndarray) -> float:
        return float(np.mean(np.square(x @ target_weights + target_bias - y)))

    initial_losses = {"target_mse": mse(target_x, target_y), "replay_mse": mse(replay_x, replay_y)}
    for _epoch in range(int(epochs)):
        for _step in range(int(steps_per_epoch)):
            target_idx, replay_idx = sample_replay_batch_indices(
                len(target_x), len(replay_x), int(batch_size), rng
            )
            batch_x = np.concatenate((target_x[target_idx], replay_x[replay_idx]), axis=0)
            batch_y = np.concatenate((target_y[target_idx], replay_y[replay_idx]), axis=0)
            order = rng.permutation(len(batch_x))
            batch_x = batch_x[order]
            batch_y = batch_y[order]
            error = batch_x @ target_weights + target_bias - batch_y
            scale = 2.0 / len(batch_x)
            target_weights -= float(learning_rate) * (scale * batch_x.T @ error)
            target_bias -= float(learning_rate) * (scale * error.sum(axis=0))
            target_seen += len(target_idx)
            replay_seen += len(replay_idx)

    final_losses = {"target_mse": mse(target_x, target_y), "replay_mse": mse(replay_x, replay_y)}
    foundation_unchanged = bool(
        np.array_equal(weights, foundation_weights_snapshot)
        and np.array_equal(bias, foundation_bias_snapshot)
    )
    return {
        "target_weights": target_weights,
        "target_bias": target_bias,
        "head_initialized_from_foundation": initialized_exactly,
        "foundation_head_unchanged": foundation_unchanged,
        "replay_mix": {
            "target_examples": target_seen,
            "foundation_examples": replay_seen,
            "target_fraction": target_seen / (target_seen + replay_seen),
            "foundation_fraction": replay_seen / (target_seen + replay_seen),
        },
        "initial_losses": initial_losses,
        "final_losses": final_losses,
        "optimizer": {
            "method": "dense-head full-gradient SGD per sampled mini-batch",
            "epochs": int(epochs),
            "steps_per_epoch": int(steps_per_epoch),
            "batch_size": int(batch_size),
            "learning_rate": float(learning_rate),
            "seed": int(seed),
        },
    }


def _npz_scalar(bundle: Any, key: str) -> str:
    if key not in bundle:
        raise ValueError(f"artifact is missing metadata field {key!r}")
    value = np.asarray(bundle[key])
    if value.size != 1:
        raise ValueError(f"artifact metadata field {key!r} must be scalar")
    return str(value.reshape(-1)[0])


def _validate_mace_mp0_metadata(bundle: Any, path: Path) -> dict[str, str]:
    metadata = {
        "repo_id": _npz_scalar(bundle, "model_repo"),
        "filename": _npz_scalar(bundle, "model_file"),
        "license": _npz_scalar(bundle, "model_license"),
        "feature_method": _npz_scalar(bundle, "feature_method"),
    }
    expected = MACE_MP0_MODEL
    if any(metadata[key] != expected[key] for key in ("repo_id", "filename", "license")):
        raise ValueError(
            f"{path} is not a commercial-safe MACE-MP-0 artifact; expected "
            f"{expected}, got {metadata}"
        )
    return metadata


def run_foundation_bootstrap(
    *,
    foundation_head_path: Any = None,
    target_data_path: Any = None,
    foundation_replay_path: Any = None,
    output_path: Any = None,
    epochs: Any = 5,
    steps_per_epoch: Any = 10,
    batch_size: Any = 20,
    learning_rate: Any = 0.001,
    seed: Any = 0,
) -> dict[str, Any]:
    """Run Addendum E Phase 0 stages 2-3 from local artifacts."""
    paths = {
        "foundation_head_path": foundation_head_path,
        "target_data_path": target_data_path,
        "foundation_replay_path": foundation_replay_path,
        "output_path": output_path,
    }
    missing = [name for name, value in paths.items() if value in (None, "")]
    base = {
        "execution_summary": "Phase 0 Stage 1 not performed; Stages 2-3 pending artifact preflight",
        "spec_version": COLD_START_VERSION,
        "model_substitution": {**MACE_MP0_MODEL, "reason": MACE_SUBSTITUTION_REASON},
        "stage_1_foundation_pretraining": {
            "status": "not_performed",
            "required_targets": ["formation_energy", "forces", "stress"],
            "required_scale": "approximately 150,000 OMAT24 and Materials Project structures",
            "reason": "full foundation pre-training requires a prepared corpus and GPU/HPC compute",
        },
    }
    if missing:
        base.update(
            {
                "status": "unavailable",
                "execution_summary": (
                    "UNAVAILABLE — Stage 1 requires GPU/HPC and Stages 2-3 need local "
                    "MACE-MP-0 head/target/replay artifacts"
                ),
                "stage_2_3": {
                    "status": "unavailable",
                    "reason": f"missing local artifacts: {', '.join(missing)}",
                    "acquisition_hint": (
                        "Provide a MACE-MP-0 Head-A .npz, sparse target descriptor .npz, "
                        "foundation replay descriptor .npz, and an output .npz path. "
                        "All artifacts must identify the MIT MACE-MP-0 model and feature method."
                    ),
                },
            }
        )
        return {"phase0": base}

    head_path = Path(str(foundation_head_path)).expanduser()
    target_path = Path(str(target_data_path)).expanduser()
    replay_path = Path(str(foundation_replay_path)).expanduser()
    out_path = Path(str(output_path)).expanduser()
    absent = [str(path) for path in (head_path, target_path, replay_path) if not path.is_file()]
    if absent:
        base.update(
            {
                "status": "unavailable",
                "execution_summary": "UNAVAILABLE — one or more local Phase 0 artifacts do not exist",
                "stage_2_3": {
                    "status": "unavailable",
                    "reason": f"local artifact files not found: {absent}",
                    "acquisition_hint": "Generate the missing artifacts on the approved GPU/HPC training system.",
                },
            }
        )
        return {"phase0": base}
    if out_path.suffix.lower() != ".npz":
        raise ValueError("output_path must end in .npz")

    with np.load(head_path, allow_pickle=False) as head, np.load(
        target_path, allow_pickle=False
    ) as target, np.load(replay_path, allow_pickle=False) as replay:
        head_meta = _validate_mace_mp0_metadata(head, head_path)
        target_meta = _validate_mace_mp0_metadata(target, target_path)
        replay_meta = _validate_mace_mp0_metadata(replay, replay_path)
        trained = fine_tune_linear_target_head(
            head["weights"],
            head["bias"],
            target["features"],
            target["targets"],
            replay["features"],
            replay["targets"],
            epochs=int(epochs),
            steps_per_epoch=int(steps_per_epoch),
            batch_size=int(batch_size),
            learning_rate=float(learning_rate),
            seed=int(seed),
        )

    provenance = prov.build(
        tool_name="cold_start_foundation_bootstrap",
        engine="numpy-sgd-on-MACE-MP-0-descriptors",
        engine_version=np.__version__,
        activity="Head-B initialization from Head-A and 80/20 replay fine-tuning",
        inputs={
            "model": MACE_MP0_MODEL,
            "head_metadata": head_meta,
            "target_metadata": target_meta,
            "replay_metadata": replay_meta,
            **trained["optimizer"],
        },
        units={"target_mse": "squared target-label units", "replay_mse": "squared replay-label units"},
        derived_from=[
            prov.file_ref(head_path, "foundation_head_A"),
            prov.file_ref(target_path, "sparse_target_data"),
            prov.file_ref(replay_path, "foundation_replay_data"),
        ],
        reproduce=(
            "prism workflow run cold-start --execute --set "
            f"foundation_head_path={head_path} --set target_data_path={target_path} "
            f"--set foundation_replay_path={replay_path} --set phase0_output_path={out_path}"
        ),
        extra={"model_substitution_reason": MACE_SUBSTITUTION_REASON},
    )
    out_path.parent.mkdir(parents=True, exist_ok=True)
    np.savez_compressed(
        out_path,
        weights=trained["target_weights"],
        bias=trained["target_bias"],
        model_repo=MACE_MP0_MODEL["repo_id"],
        model_file=MACE_MP0_MODEL["filename"],
        model_license=MACE_MP0_MODEL["license"],
        feature_method=head_meta["feature_method"],
        training_provenance=json.dumps(provenance, sort_keys=True),
    )
    base.update(
        {
            "status": "completed_scope_limited",
            "execution_summary": (
                "COMPLETED SCOPE-LIMITED — Head-B copied from Head-A; foundation head frozen; "
                "replay mix exactly 80/20; Stage 1 not performed"
            ),
            "scope": "stages 2-3 only; stage 1 was supplied as a pre-trained local Head-A artifact",
            "stage_2_target_head_initialization": {
                "status": "completed",
                "copied_from_head_A": trained["head_initialized_from_foundation"],
            },
            "stage_3_replay_fine_tuning": {
                "status": "completed",
                "foundation_head_frozen": trained["foundation_head_unchanged"],
                "replay_mix": trained["replay_mix"],
                "initial_losses": trained["initial_losses"],
                "final_losses": trained["final_losses"],
                "optimizer": trained["optimizer"],
            },
            "target_head_artifact": prov.file_ref(out_path, "fine_tuned_target_head_B"),
            "provenance": provenance,
        }
    )
    return {"phase0": base}


def _pycalphad_version() -> str:
    try:
        import pycalphad

        return str(getattr(pycalphad, "__version__", "unknown"))
    except Exception:
        return "absent"


def _available_database_names(bridge: Any) -> list[str]:
    return [str(item["name"]) for item in bridge.databases.list_databases()]


def _database_covering_elements(bridge: Any, elements: list[str], requested: str | None) -> str | None:
    from app.tools.evaluation import _find_covering_database

    wanted = {element.upper() for element in elements}
    if requested:
        db = bridge.databases.load(requested)
        if db is None:
            return None
        available = {str(element).upper() for element in getattr(db, "elements", [])}
        return requested if wanted <= available else None
    return _find_covering_database(bridge, elements)


def missing_database_message(bridge: Any, elements: list[str]) -> str:
    available = _available_database_names(bridge)
    element_set = ", ".join(elements)
    return (
        f"no thermodynamic database covering element set {{{element_set}}} found in "
        f"{bridge.databases.base_dir} (available: {', '.join(available) if available else 'none'}). "
        "A CALPHAD result requires a TDB for this system — none is synthesized."
    )


def _bcc_phase_names(bridge: Any, database_name: str) -> list[str]:
    db = bridge.databases.load(database_name)
    if db is None:
        return []
    names = [str(name) for name in db.phases]
    return sorted(name for name in names if name.upper() == "BCC" or name.upper().startswith("BCC_"))


def _conditions(elements: list[str], composition: dict[str, float], temperature: float, pressure: float) -> dict[str, float]:
    conditions: dict[str, float] = {"T": temperature, "P": pressure}
    for element in elements[:-1]:
        conditions[f"X({element.upper()})"] = float(composition[element])
    return conditions


def run_calphad_augmentation(
    *,
    elements: Any,
    temperature_K: Any = 1773.0,
    n_samples: Any = 10000,
    seed: Any = 0,
    pressure_Pa: Any = 101325.0,
    database_name: Any = None,
) -> dict[str, Any]:
    """Run Addendum E Algorithm 2 using the existing CALPHAD tier."""
    elements = normalize_elements(elements)
    temperature = float(temperature_K)
    pressure = float(pressure_Pa)
    count = int(n_samples)
    database_name = None if database_name in (None, "") else str(database_name)
    from app.tools.simulation.calphad_bridge import check_calphad_available

    if not check_calphad_available():
        return {
            "phase1": {
                "status": "unavailable",
                "tier": 2,
                "engine": "pycalphad",
                "reason": "pycalphad not importable in this interpreter",
                "install_hint": (
                    "Install into the PRISM venv only: ~/.prism/venv/bin/python -m pip "
                    "install 'pycalphad>=0.10,<0.12'."
                ),
                "dataset": [],
            }
        }

    from app.tools.simulation.calphad_bridge import get_calphad_bridge

    bridge = get_calphad_bridge()
    selected_database = _database_covering_elements(bridge, elements, database_name)
    if selected_database is None:
        reason = missing_database_message(bridge, elements)
        return {
            "phase1": {
                "status": "unavailable",
                "tier": 2,
                "engine": "pycalphad",
                "execution_summary": f"UNAVAILABLE — {reason}",
                "method": "CALPHAD database coverage preflight",
                "reason": reason,
                "install_hint": (
                    "Acquire a licensed or open TDB covering all requested elements, then import it "
                    "into ~/.prism/databases with the calphad import tool."
                ),
                "parameters": {
                    "elements": elements,
                    "temperature_K": temperature,
                    "pressure_Pa": pressure,
                    "n_samples": count,
                    "seed": int(seed),
                    "requested_database": database_name,
                },
                "dataset": [],
                "provenance": prov.build(
                    tool_name="cold_start_calphad_augmentation",
                    engine="pycalphad",
                    engine_version=_pycalphad_version(),
                    activity="thermodynamic database coverage preflight",
                    inputs={"elements": elements, "database_name": database_name},
                    units={},
                    reproduce="prism workflow run cold-start --execute",
                    extra={"tier": 2, "status": "unavailable"},
                ),
            }
        }

    bcc_phases = _bcc_phase_names(bridge, selected_database)
    if not bcc_phases:
        available_phases = bridge.databases.get_phases(selected_database) or []
        reason = (
            f"thermodynamic database '{selected_database}' covers element set "
            f"{{{', '.join(elements)}}} but defines no BCC phase "
            f"(available phases: {', '.join(available_phases) if available_phases else 'none'}). "
            "BCC stability and driving force cannot be calculated."
        )
        return {
            "phase1": {
                "status": "unavailable",
                "tier": 2,
                "engine": "pycalphad",
                "reason": reason,
                "install_hint": "Use a TDB that both covers the element set and defines a BCC phase.",
                "dataset": [],
            }
        }

    from app.tools import evaluation

    samples = sample_simplex(elements, count, int(seed))
    records: list[dict[str, Any]] = []
    for index, composition in enumerate(samples):
        fractions = [composition[element] for element in elements]
        candidate = {
            "fractions": composition,
            "temperature_K": temperature,
            "pressure_Pa": pressure,
            "calphad_database": selected_database,
        }
        equilibrium = evaluation._run_tier2(candidate, elements, fractions)
        if equilibrium.get("status") != "ok":
            return {
                "phase1": {
                    "status": "failed",
                    "tier": 2,
                    "engine": "pycalphad",
                    "reason": f"global equilibrium failed for sample {index}: {equilibrium}",
                    "completed_samples": len(records),
                    "partial_dataset": records,
                }
            }
        phi = equilibrium["properties"].get("phase_fractions") or {}
        phi_bcc = float(sum(float(phi.get(name, 0.0)) for name in bcc_phases))
        constrained = bridge.calculate_equilibrium(
            database_name=selected_database,
            components=[element.upper() for element in elements],
            phases=bcc_phases,
            conditions=_conditions(elements, composition, temperature, pressure),
        )
        if "error" in constrained:
            return {
                "phase1": {
                    "status": "failed",
                    "tier": 2,
                    "engine": "pycalphad",
                    "reason": f"BCC-constrained equilibrium failed for sample {index}: {constrained['error']}",
                    "completed_samples": len(records),
                    "partial_dataset": records,
                }
            }
        global_g = float(equilibrium["properties"]["gibbs_energy"])
        bcc_g = float(constrained["gibbs_energy"])
        driving_force = bcc_g - global_g
        stable = label_bcc_stability(phi_bcc)
        tdb_ref = prov.file_ref(
            bridge.databases.base_dir / f"{selected_database}.tdb", "thermodynamic_database"
        )
        sampling_provenance = prov.build(
            tool_name="cold_start_calphad_augmentation",
            engine="numpy.random.Generator(PCG64)",
            engine_version=np.__version__,
            activity="Dirichlet(alpha=1) uniform simplex sampling",
            inputs={"elements": elements, "seed": int(seed), "sample_index": index},
            units={"x": "atomic fraction"},
            reproduce=(
                f"sample_simplex({elements!r}, n_samples={count}, seed={int(seed)})[{index}]"
            ),
        )
        label_provenance = prov.build(
            tool_name="cold_start_calphad_augmentation",
            engine="PRISM deterministic threshold",
            engine_version=COLD_START_VERSION,
            activity="single-phase BCC stability labeling",
            inputs={"phi_BCC": phi_bcc, "threshold": 0.95, "comparison": ">"},
            units={"phi_BCC": "dimensionless", "y_stability": "binary label"},
            derived_from=[tdb_ref],
            reproduce=f"int({phi_bcc!r} > 0.95)",
            extra={"tier": 2},
        )
        driving_force_provenance = prov.build(
            tool_name="cold_start_calphad_augmentation",
            engine="pycalphad",
            engine_version=_pycalphad_version(),
            activity="G(BCC-constrained equilibrium) - G(global equilibrium)",
            inputs={
                "database": selected_database,
                "bcc_phases": bcc_phases,
                "composition": composition,
                "temperature_K": temperature,
                "pressure_Pa": pressure,
                "global_gibbs_energy_J_per_mol_atom": global_g,
                "bcc_constrained_gibbs_energy_J_per_mol_atom": bcc_g,
            },
            units={"dG_BCC": "J/mol-atom"},
            derived_from=[tdb_ref],
            reproduce=(
                "calphad equilibrium at fixed composition over all phases, then over "
                f"{bcc_phases!r}; subtract global GM from BCC-constrained GM"
            ),
            extra={"tier": 2},
        )
        records.append(
            {
                "sample_index": index,
                "x": composition,
                "y_stability": stable,
                "dG_BCC_J_per_mol_atom": driving_force,
                "phi": phi,
                "phi_BCC": phi_bcc,
                "provenance": {
                    "x": sampling_provenance,
                    "y_stability": label_provenance,
                    "dG_BCC": driving_force_provenance,
                    "phi": equilibrium.get("provenance", {}),
                },
            }
        )

    stable_records = [record for record in records if record["y_stability"] == 1]
    return {
        "phase1": {
            "status": "completed",
            "tier": 2,
            "engine": "pycalphad",
            "method": "Addendum E Algorithm 2 CALPHAD synthetic data generation",
            "database": selected_database,
            "bcc_phases": bcc_phases,
            "parameters": {
                "elements": elements,
                "temperature_K": temperature,
                "pressure_Pa": pressure,
                "n_samples": count,
                "seed": int(seed),
                "stability_rule": "phi_BCC > 0.95",
                "driving_force_definition": "G_BCC_constrained - G_global_equilibrium",
            },
            "dataset": records,
            "stable_candidates": stable_records,
            "summary": {
                "generated": len(records),
                "stable": len(stable_records),
                "eliminated": len(records) - len(stable_records),
                "elimination_fraction": (len(records) - len(stable_records)) / len(records),
            },
        }
    }


def _finite_float(candidate: dict[str, Any], key: str) -> float:
    try:
        value = float(candidate[key])
    except (KeyError, TypeError, ValueError) as exc:
        raise ValueError(f"candidate {candidate.get('id', '<unknown>')!r} needs numeric {key}") from exc
    if not math.isfinite(value):
        raise ValueError(f"candidate {candidate.get('id', '<unknown>')!r} has non-finite {key}")
    return value


def adapt_acquisition_weights(
    weights: dict[str, float],
    *,
    stagnation_batches: int,
    budget_fraction_remaining: float,
    breakthrough_detected: bool,
) -> tuple[dict[str, float], list[dict[str, Any]]]:
    """Apply only the numeric adaptation Addendum E actually specifies."""
    adapted = dict(weights)
    rules: list[dict[str, Any]] = []
    if stagnation_batches >= 5:
        adapted["uncertainty"] += 0.1
        rules.append(
            {
                "trigger": "stagnation_5_batches",
                "action": "increase_uncertainty_weight(0.1)",
                "applied": True,
            }
        )
    if budget_fraction_remaining < 0.2:
        rules.append(
            {
                "trigger": "budget_below_20_percent",
                "action": "shift_to_exploitation_mode",
                "applied": "mode_requested; Addendum E specifies no replacement numeric weights",
            }
        )
    if breakthrough_detected:
        rules.append(
            {
                "trigger": "breakthrough_detected",
                "action": "local_exploitation_burst",
                "applied": "mode_requested; Addendum E specifies no replacement numeric weights",
            }
        )
    return adapted, rules


def run_active_learning(
    *,
    candidates: Any = None,
    batch_size: Any = 16,
    weights: Any = None,
    stagnation_batches: Any = 0,
    budget_fraction_remaining: Any = 1.0,
    breakthrough_detected: Any = False,
    max_exact_combinations: Any = 100000,
) -> dict[str, Any]:
    """Score candidates and perform exact small-pool D-optimal selection."""
    candidates = _json_value(candidates, "candidates")
    if not candidates:
        return {
            "phase2": {
                "status": "unavailable",
                "execution_summary": "UNAVAILABLE — no real active-learning candidate pool supplied",
                "reason": "no active-learning candidate pool was supplied",
                "acquisition_hint": (
                    "Provide Mutator candidates with measured/model-derived sigma, mu, d_pareto, rho, "
                    "descriptor, and provenance for those inputs. PRISM will not invent missing signals."
                ),
                "scores": [],
                "selected": [],
            }
        }
    if not isinstance(candidates, list):
        raise ValueError("candidates must be a JSON list")
    supplied_weights = _json_value(weights, "weights") if weights is not None else None
    active_weights = dict(DEFAULT_ACQUISITION_WEIGHTS if supplied_weights is None else supplied_weights)
    if set(active_weights) != set(DEFAULT_ACQUISITION_WEIGHTS):
        raise ValueError(f"weights must contain exactly {sorted(DEFAULT_ACQUISITION_WEIGHTS)}")
    active_weights = {key: float(value) for key, value in active_weights.items()}
    if any(not math.isfinite(value) or value < 0 for value in active_weights.values()):
        raise ValueError("acquisition weights must be finite and non-negative")
    budget_fraction = float(budget_fraction_remaining)
    if not 0.0 <= budget_fraction <= 1.0:
        raise ValueError("budget_fraction_remaining must be in [0, 1]")
    adapted, rules = adapt_acquisition_weights(
        active_weights,
        stagnation_batches=int(stagnation_batches),
        budget_fraction_remaining=budget_fraction,
        breakthrough_detected=bool(_json_value(breakthrough_detected, "breakthrough_detected")),
    )

    scored: list[dict[str, Any]] = []
    rejected: list[dict[str, Any]] = []
    descriptor_size: int | None = None
    for index, candidate in enumerate(candidates):
        if not isinstance(candidate, dict):
            raise ValueError(f"candidate at index {index} must be an object")
        if not isinstance(candidate.get("provenance"), dict):
            raise ValueError(
                f"candidate {candidate.get('id', index)!r} needs provenance for its acquisition inputs"
            )
        if candidate.get("y_stability") != 1:
            rejected.append(
                {
                    "id": candidate.get("id", str(index)),
                    "reason": "hard CALPHAD constraint requires y_stability == 1",
                    "input": candidate,
                }
            )
            continue
        sigma = _finite_float(candidate, "sigma")
        mu = _finite_float(candidate, "mu")
        d_pareto = _finite_float(candidate, "d_pareto")
        rho = _finite_float(candidate, "rho")
        if not 0.0 <= rho <= 1.0:
            raise ValueError(f"candidate {candidate.get('id', index)!r} rho must be in [0, 1]")
        descriptor = np.asarray(candidate.get("descriptor"), dtype=float)
        if descriptor.ndim != 1 or not descriptor.size or not np.isfinite(descriptor).all():
            raise ValueError(f"candidate {candidate.get('id', index)!r} needs a finite descriptor vector")
        if descriptor_size is None:
            descriptor_size = int(descriptor.size)
        elif descriptor.size != descriptor_size:
            raise ValueError("all candidate descriptors must have the same length")
        alpha = (
            adapted["uncertainty"] * sigma
            + adapted["exploitation"] * mu
            + adapted["improvement"] * d_pareto
            + adapted["diversity"] * (1.0 - rho)
        )
        score_provenance = prov.build(
            tool_name="cold_start_active_learning",
            engine="PRISM Addendum E acquisition",
            engine_version=COLD_START_VERSION,
            activity="weighted composite acquisition score",
            inputs={
                "candidate_id": candidate.get("id", index),
                "sigma": sigma,
                "mu": mu,
                "d_pareto": d_pareto,
                "rho": rho,
                "weights": adapted,
                "input_provenance": candidate["provenance"],
            },
            units={"alpha": "weighted input units"},
            reproduce="w1*sigma + w2*mu + w3*d_pareto + w4*(1-rho)",
        )
        scored.append(
            {
                "id": candidate.get("id", str(index)),
                "composition": candidate.get("composition"),
                "alpha": alpha,
                "descriptor": descriptor.tolist(),
                "input": candidate,
                "provenance": score_provenance,
            }
        )

    if not scored:
        return {
            "phase2": {
                "status": "unavailable",
                "reason": "no candidates survived the hard CALPHAD stability constraint",
                "weights": adapted,
                "adaptation_rules": rules,
                "hard_constraint_rejections": rejected,
                "scores": [],
                "selected": [],
            }
        }

    k = int(batch_size)
    if k <= 0 or k > len(scored):
        raise ValueError("batch_size must be positive and no larger than the candidate pool")
    combination_count = math.comb(len(scored), k)
    limit = int(max_exact_combinations)
    if combination_count > limit:
        return {
            "phase2": {
                "status": "unavailable",
                "reason": (
                    f"exact D-optimal selection needs {combination_count} combinations, above the "
                    f"configured limit {limit}"
                ),
                "acquisition_hint": (
                    "Reduce the candidate pool or batch size, or explicitly introduce and validate an "
                    "approximation. This implementation will not label a greedy approximation as argmax."
                ),
                "weights": adapted,
                "adaptation_rules": rules,
                "hard_constraint_rejections": rejected,
                "scores": scored,
                "selected": [],
            }
        }

    ordered = sorted(range(len(scored)), key=lambda i: (-scored[i]["alpha"], str(scored[i]["id"])))
    best_indices: tuple[int, ...] | None = None
    best_det = -math.inf
    for indices in combinations(ordered, k):
        information = np.zeros((descriptor_size, descriptor_size), dtype=float)
        for index in indices:
            descriptor = np.asarray(scored[index]["descriptor"], dtype=float)
            information += np.outer(descriptor, descriptor)
        determinant = float(np.linalg.det(information))
        if not math.isfinite(determinant):
            raise ValueError("D-optimal determinant is non-finite")
        if determinant > best_det:
            best_det = determinant
            best_indices = indices
    assert best_indices is not None
    selected = [scored[index] for index in best_indices]
    selection_provenance = prov.build(
        tool_name="cold_start_active_learning",
        engine="NumPy exact enumeration",
        engine_version=np.__version__,
        activity="D-optimal batch argmax",
        inputs={
            "candidate_ids": [item["id"] for item in scored],
            "batch_size": k,
            "combination_count": combination_count,
            "tie_break": "descending alpha then candidate id",
        },
        units={"determinant": "descriptor units raised to matrix dimension"},
        reproduce="argmax_B det(sum(outer(d(x), d(x)) for x in B))",
    )
    return {
        "phase2": {
            "status": "completed",
            "method": "composite acquisition plus exact D-optimal batch selection",
            "weights": adapted,
            "adaptation_rules": rules,
            "hard_constraint_rejections": rejected,
            "scores": scored,
            "selected": selected,
            "d_optimality": {
                "determinant": best_det,
                "combination_count": combination_count,
                "provenance": selection_provenance,
            },
        }
    }


def _composition_string(composition: Any) -> str | None:
    if isinstance(composition, str) and composition.strip():
        return composition.strip()
    if isinstance(composition, dict) and composition:
        return "".join(
            f"{element}{float(fraction):.8f}" for element, fraction in composition.items()
        )
    return None


def run_campaign_handoff(
    *,
    elements: Any,
    temperature_K: Any,
    phase1: Any,
    phase2: Any,
    objective: Any = "maximize high-temperature refractory HEA performance",
) -> dict[str, Any]:
    """Prepare, but do not execute, the existing campaign loop handoff."""
    elements = normalize_elements(elements)
    phase1 = _json_value(phase1, "phase1")
    phase2 = _json_value(phase2, "phase2")
    blockers = []
    if not isinstance(phase1, dict) or phase1.get("status") != "completed":
        blockers.append(f"Phase 1 is not complete ({(phase1 or {}).get('reason', 'no completed result')})")
    if not isinstance(phase2, dict) or phase2.get("status") != "completed":
        blockers.append(f"Phase 2 is not complete ({(phase2 or {}).get('reason', 'no completed result')})")
    if blockers:
        return {
            "phase3": {
                "status": "blocked",
                "engine": "prism_campaign",
                "execution_summary": "BLOCKED — incomplete Phase 1 and/or Phase 2; campaign not invoked",
                "reason": "; ".join(blockers),
                "handoff": None,
                "note": "The existing crates/campaign loop was not invoked or reimplemented.",
            }
        }

    selected = phase2.get("selected") or []
    seed_records = []
    for item in selected:
        composition = _composition_string(item.get("composition") or item.get("input", {}).get("composition"))
        if composition is None:
            raise ValueError(f"selected candidate {item.get('id')!r} has no composition")
        seed_records.append(
            {
                "composition": composition,
                "source_candidate_id": item.get("id"),
                "provenance": item.get("provenance"),
            }
        )
    if not seed_records:
        raise ValueError("Phase 2 selected no candidates for campaign handoff")

    handoff_provenance = prov.build(
        tool_name="cold_start_campaign_handoff",
        engine="PRISM workflow",
        engine_version=COLD_START_VERSION,
        activity="construct CampaignGoal/CampaignConfig handoff without executing campaign",
        inputs={
            "elements": elements,
            "temperature_K": float(temperature_K),
            "selected_candidate_ids": [record["source_candidate_id"] for record in seed_records],
        },
        units={"temperature_K": "K"},
        reproduce="pass handoff.campaign_goal and handoff.campaign_config to prism_campaign::Campaign::new",
    )
    handoff = {
        "integration_target": "prism_campaign::Campaign::new (existing crates/campaign loop)",
        "campaign_goal": {
            "description": "Discover BCC-stable refractory high-entropy alloys after cold-start bootstrap",
            "elements": elements,
            "objective": str(objective),
            "constraints": [
                f"cold-start seeds passed phi_BCC > 0.95 at {float(temperature_K):g} K",
                "allowed element set is enforced by CampaignGoal.elements",
                "HEA configurational-entropy and principal-element hard constraints remain enabled by the existing campaign",
            ],
            "seeds": [record["composition"] for record in seed_records],
        },
        "campaign_config": {
            "use_existing_defaults": True,
            "checkpoint_and_resume": "owned by crates/campaign",
            "hard_compositional_constraints": "owned by crates/campaign",
        },
        "seed_records": seed_records,
        "provenance": handoff_provenance,
    }
    return {
        "phase3": {
            "status": "ready",
            "engine": "prism_campaign",
            "handoff": handoff,
            "note": "No campaign was started; the existing durable loop receives this artifact.",
        }
    }
