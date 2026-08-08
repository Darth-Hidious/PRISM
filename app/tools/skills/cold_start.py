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
            "foundation_head_path": {
                "type": "string",
                "description": (
                    "Path to the .npz holding the frozen foundation Head-A: "
                    "'weights', 'bias', and the model_repo/model_file/"
                    "model_license/feature_method metadata. Rejected unless that "
                    "metadata identifies the MIT-licensed MACE-MP-0 artifact. "
                    "Omit any of the four paths and the skill returns an "
                    "'unavailable' preflight instead of a trained head."
                ),
            },
            "target_data_path": {
                "type": "string",
                "description": (
                    "Path to the sparse target dataset .npz ('features' = frozen "
                    "MACE-MP-0 descriptors, 'targets' = labels), carrying the same "
                    "MACE-MP-0 metadata. Supplies the 80% target side of every "
                    "replay batch."
                ),
            },
            "foundation_replay_path": {
                "type": "string",
                "description": (
                    "Path to the foundation replay .npz ('features', 'targets') "
                    "drawn from the pre-training distribution. Supplies the 20% "
                    "replay side of every batch, which is what keeps Head-B from "
                    "drifting off the foundation task."
                ),
            },
            "output_path": {
                "type": "string",
                "description": (
                    "Destination .npz for the fine-tuned Head-B; must end in .npz. "
                    "Parent directories are created. Written with the MACE-MP-0 "
                    "metadata and the full training provenance record."
                ),
            },
            "epochs": {
                "type": "integer",
                "minimum": 1,
                "description": (
                    "Passes over the training schedule (default 5). Total gradient "
                    "updates = epochs x steps_per_epoch."
                ),
            },
            "steps_per_epoch": {
                "type": "integer",
                "minimum": 1,
                "description": (
                    "Mini-batch gradient steps per epoch (default 10). Batches are "
                    "resampled with replacement each step, so raising this exposes "
                    "Head-B to more of the target and replay pools."
                ),
            },
            "batch_size": {
                "type": "integer",
                "minimum": 5,
                "multipleOf": 5,
                "description": (
                    "Examples per gradient step (default 20). Must be a positive "
                    "multiple of 5 so the 80/20 split is exact: batch_size//5 rows "
                    "come from the foundation replay pool, the rest from the "
                    "sparse target pool."
                ),
            },
            "learning_rate": {
                "type": "number",
                "exclusiveMinimum": 0,
                "description": (
                    "SGD step size for the cloned Head-B dense layer (default "
                    "0.001). Head-A is never updated at any learning rate."
                ),
            },
            "seed": {
                "type": "integer",
                "description": (
                    "Seed for the NumPy PCG64 generator that draws and shuffles "
                    "replay batch indices (default 0). Same seed plus same "
                    "artifacts reproduces Head-B exactly."
                ),
            },
        },
        "required": [
            "foundation_head_path",
            "target_data_path",
            "foundation_replay_path",
            "output_path",
        ],
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
            "elements": {
                "type": "array",
                "items": {"type": "string"},
                "description": (
                    "Element symbols spanning the composition simplex to sample, "
                    "e.g. ['Nb','Mo','Ta','W']. At least two, no duplicates. A "
                    "local TDB covering all of them (and defining a BCC phase) "
                    "must exist or the call returns 'unavailable'."
                ),
            },
            "temperature_K": {
                "type": "number",
                "exclusiveMinimum": 0,
                "description": (
                    "Equilibrium temperature in kelvin, applied to every sampled "
                    "composition (default 1773)."
                ),
            },
            "n_samples": {
                "type": "integer",
                "minimum": 1,
                "description": (
                    "Number of compositions drawn from the Dirichlet(alpha=1) "
                    "uniform simplex (default 10000). Each sample costs two "
                    "pycalphad equilibria (global + BCC-constrained), so runtime "
                    "scales linearly with this."
                ),
            },
            "seed": {
                "type": "integer",
                "description": (
                    "Seed for the NumPy PCG64 simplex sampler (default 0). Same "
                    "seed plus same element list reproduces the identical "
                    "composition set."
                ),
            },
            "pressure_Pa": {
                "type": "number",
                "exclusiveMinimum": 0,
                "description": (
                    "Pressure in pascals held fixed across all equilibria "
                    "(default 101325, i.e. 1 atm)."
                ),
            },
            "database_name": {
                "type": "string",
                "description": (
                    "TDB name in ~/.prism/databases. Omit to auto-select the "
                    "first local database covering every requested element. A "
                    "named database that does not cover them all returns "
                    "'unavailable' rather than falling back to another."
                ),
            },
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
            "candidates": {
                "type": "array",
                "description": (
                    "Mutator candidate pool. Each entry needs y_stability (only "
                    "y_stability == 1 survives the hard CALPHAD gate), the numeric "
                    "acquisition inputs sigma, mu, d_pareto and rho (rho in [0,1]), "
                    "a finite descriptor vector of the same length for every "
                    "candidate, and a provenance object covering those inputs. "
                    "Candidates missing any of these are rejected, never imputed."
                ),
            },
            "batch_size": {
                "type": "integer",
                "minimum": 1,
                "description": (
                    "Number of candidates in the selected D-optimal batch (default "
                    "16). Must not exceed the pool that survived the stability "
                    "gate. Exact enumeration costs C(surviving_pool, batch_size) "
                    "determinants, so this drives the runtime."
                ),
            },
            "weights": {
                "type": "object",
                "description": (
                    "Acquisition weights, exactly the keys uncertainty, "
                    "exploitation, improvement and diversity, each finite and "
                    "non-negative. Default {0.4, 0.2, 0.3, 0.1} for "
                    "alpha = w_unc*sigma + w_exp*mu + w_imp*d_pareto + "
                    "w_div*(1-rho)."
                ),
            },
            "stagnation_batches": {
                "type": "integer",
                "minimum": 0,
                "description": (
                    "Consecutive batches without improvement (default 0). At 5 or "
                    "more, the uncertainty weight is raised by 0.1 — the only "
                    "numeric adaptation Addendum E specifies."
                ),
            },
            "budget_fraction_remaining": {
                "type": "number",
                "minimum": 0,
                "maximum": 1,
                "description": (
                    "Fraction of the experimental budget still available, in [0,1] "
                    "(default 1.0). Below 0.2 an exploitation-mode trigger is "
                    "recorded in the result, but no weight changes: the playbook "
                    "specifies no replacement numbers."
                ),
            },
            "breakthrough_detected": {
                "type": "boolean",
                "description": (
                    "True when the campaign has just found a breakthrough (default "
                    "false). Records a local-exploitation-burst trigger only; like "
                    "the budget rule it changes no numeric weight."
                ),
            },
            "max_exact_combinations": {
                "type": "integer",
                "minimum": 1,
                "description": (
                    "Ceiling on C(pool, batch_size) for the exact D-optimal search "
                    "(default 100000). Above it the tool returns 'unavailable' "
                    "instead of silently substituting a greedy approximation."
                ),
            },
        },
        "required": ["candidates"],
        "additionalProperties": False,
    },
    func=_phase2,
    category="cold_start",
)


PHASE3_CAMPAIGN_HANDOFF_SKILL = Skill(
    name="cold_start_campaign_handoff",
    description=(
        "Validate the cold-start outputs and build a provenance-bearing handoff "
        "for the existing durable prism_campaign loop. Returns the CampaignGoal "
        "and CampaignConfig payload plus the seed compositions selected in Phase "
        "2, or a 'blocked' result naming the phase that is not complete. This "
        "skill neither reimplements nor starts the campaign."
    ),
    steps=[
        SkillStep("validate_bootstrap", "Require completed CALPHAD and active-learning phases", "internal"),
        SkillStep("build_handoff", "Construct CampaignGoal and CampaignConfig inputs", "internal"),
    ],
    input_schema={
        "type": "object",
        "properties": {
            "elements": {
                "type": "array",
                "items": {"type": "string"},
                "description": (
                    "Element symbols the campaign may use, e.g. "
                    "['Nb','Mo','Ta','W']. At least two, no duplicates. Becomes "
                    "CampaignGoal.elements, which is what enforces the allowed "
                    "element set downstream."
                ),
            },
            "temperature_K": {
                "type": "number",
                "description": (
                    "Temperature in kelvin at which the cold-start seeds were "
                    "labelled BCC-stable. Recorded verbatim in the campaign "
                    "constraint text and in the handoff provenance."
                ),
            },
            "phase1": {
                "type": "object",
                "description": (
                    "The 'phase1' object returned by "
                    "cold_start_calphad_augmentation. Its status must be "
                    "'completed' or the handoff comes back 'blocked'."
                ),
            },
            "phase2": {
                "type": "object",
                "description": (
                    "The 'phase2' object returned by cold_start_active_learning. "
                    "Its status must be 'completed'; its 'selected' entries become "
                    "the campaign seed compositions, and every one of them must "
                    "carry a composition."
                ),
            },
            "objective": {
                "type": "string",
                "description": (
                    "Free-text campaign objective copied into "
                    "CampaignGoal.objective (default 'maximize high-temperature "
                    "refractory HEA performance')."
                ),
            },
        },
        "required": ["elements", "temperature_K", "phase1", "phase2"],
        "additionalProperties": False,
    },
    func=_phase3,
    category="cold_start",
)
