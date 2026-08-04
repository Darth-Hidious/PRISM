"""Workflow-facing skills for Addendum E's cold-start phases."""

from __future__ import annotations

from app.tools.ml.cold_start import (
    run_active_learning,
    run_calphad_augmentation,
    run_campaign_handoff,
    run_foundation_bootstrap,
)
from app.tools.skills.base import Skill, SkillStep


def _phase0(**kwargs):
    return run_foundation_bootstrap(**kwargs)


def _phase1(**kwargs):
    return run_calphad_augmentation(**kwargs)


def _phase2(**kwargs):
    return run_active_learning(**kwargs)


def _phase3(**kwargs):
    return run_campaign_handoff(**kwargs)


PHASE0_BOOTSTRAP_SKILL = Skill(
    name="cold_start_foundation_bootstrap",
    description=(
        "Run Addendum E Phase 0 stages 2-3: initialize a target head from a "
        "commercial-safe MIT MACE-MP-0 foundation head and fine-tune only the "
        "target head with an exact 80/20 sparse-target/foundation replay mix. "
        "Full approximately 150k-structure foundation pre-training is reported "
        "as an external GPU/HPC requirement, never simulated."
    ),
    steps=[
        SkillStep("load_head_a", "Load a provenance-bearing MACE-MP-0 Head-A artifact", "internal"),
        SkillStep("initialize_head_b", "Copy Head-A parameters into Head-B", "internal"),
        SkillStep("replay_fine_tune", "Update Head-B only with exact 80/20 replay batches", "internal"),
        SkillStep("write_head_b", "Write the fine-tuned local target-head artifact", "internal"),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "foundation_head_path": {"type": "string"},
            "target_data_path": {"type": "string"},
            "foundation_replay_path": {"type": "string"},
            "output_path": {"type": "string"},
            "epochs": {"type": "integer", "minimum": 1},
            "steps_per_epoch": {"type": "integer", "minimum": 1},
            "batch_size": {"type": "integer", "minimum": 5, "multipleOf": 5},
            "learning_rate": {"type": "number", "exclusiveMinimum": 0},
            "seed": {"type": "integer"},
        },
        "additionalProperties": False,
    },
    func=_phase0,
    category="cold_start",
    requires_approval=True,
)


PHASE1_CALPHAD_SKILL = Skill(
    name="cold_start_calphad_augmentation",
    description=(
        "Run Addendum E Algorithm 2 with real pycalphad equilibria: uniformly "
        "sample the composition simplex, label BCC stability at phi_BCC > 0.95, "
        "and compute dG_BCC as constrained-BCC minus global equilibrium Gibbs "
        "energy. Reuses PRISM's CALPHAD tier and fails honestly when no covering "
        "TDB or BCC phase exists."
    ),
    steps=[
        SkillStep("preflight_tdb", "Find a local TDB covering the full element set", "evaluation_tier_status"),
        SkillStep("sample_simplex", "Sample Dirichlet(alpha=1) compositions", "internal"),
        SkillStep("global_equilibrium", "Run the existing tier-2 CALPHAD evaluator", "evaluate_candidate"),
        SkillStep("bcc_driving_force", "Run BCC-constrained CALPHAD equilibrium", "calphad_compute"),
        SkillStep("label_and_filter", "Apply the strict phi_BCC > 0.95 rule", "internal"),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "elements": {"type": "array", "items": {"type": "string"}},
            "temperature_K": {"type": "number", "exclusiveMinimum": 0},
            "n_samples": {"type": "integer", "minimum": 1},
            "seed": {"type": "integer"},
            "pressure_Pa": {"type": "number", "exclusiveMinimum": 0},
            "database_name": {"type": "string"},
        },
        "required": ["elements"],
        "additionalProperties": False,
    },
    func=_phase1,
    category="cold_start",
    requires_approval=True,
)


PHASE2_ACTIVE_LEARNING_SKILL = Skill(
    name="cold_start_active_learning",
    description=(
        "Run Addendum E Phase 2: score provenance-bearing candidates with "
        "w1*sigma + w2*mu + w3*d_Pareto + w4*(1-rho), apply the playbook's "
        "specified adaptation triggers, and select an exact small-pool "
        "D-optimal batch after dropping every candidate whose real CALPHAD "
        "label is not y_stability=1. Missing signals are never invented."
    ),
    steps=[
        SkillStep("adapt_weights", "Apply the playbook adaptation rules", "internal"),
        SkillStep("score", "Compute the composite acquisition score", "internal"),
        SkillStep("d_optimal", "Maximize the descriptor information determinant", "internal"),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "candidates": {"type": "array"},
            "batch_size": {"type": "integer", "minimum": 1},
            "weights": {"type": "object"},
            "stagnation_batches": {"type": "integer", "minimum": 0},
            "budget_fraction_remaining": {"type": "number", "minimum": 0, "maximum": 1},
            "breakthrough_detected": {"type": "boolean"},
            "max_exact_combinations": {"type": "integer", "minimum": 1},
        },
        "additionalProperties": False,
    },
    func=_phase2,
    category="cold_start",
)


PHASE3_CAMPAIGN_HANDOFF_SKILL = Skill(
    name="cold_start_campaign_handoff",
    description=(
        "Validate the cold-start outputs and build a provenance-bearing handoff "
        "for the existing durable prism_campaign loop. This skill neither "
        "reimplements nor starts the campaign."
    ),
    steps=[
        SkillStep("validate_bootstrap", "Require completed CALPHAD and active-learning phases", "internal"),
        SkillStep("build_handoff", "Construct CampaignGoal and CampaignConfig inputs", "internal"),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "elements": {"type": "array", "items": {"type": "string"}},
            "temperature_K": {"type": "number"},
            "phase1": {"type": "object"},
            "phase2": {"type": "object"},
            "objective": {"type": "string"},
        },
        "required": ["elements", "temperature_K", "phase1", "phase2"],
        "additionalProperties": False,
    },
    func=_phase3,
    category="cold_start",
)
