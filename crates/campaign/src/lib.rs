// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! PRISM Campaign Engine
//!
//! Long-running autonomous materials discovery campaigns.
//!
//! A campaign is a budget-limited, checkpointable loop that:
//! 1. Proposes candidate materials (via LLM, MCMC, or seed data)
//! 2. Evaluates each candidate (via tools such as `hea_descriptors`)
//! 3. Ranks by a scalarized reward function
//! 4. Narrows the search around top performers (adaptive sampling)
//! 5. Checkpoints state for resume after interruption
//! 6. Records every step to the provenance chain
//! 7. Pauses for human approval at configurable milestones
//!
//! Unlike a workflow (which is a static YAML DAG), a campaign is dynamic:
//! the LLM decides what to sample next based on results so far. The
//! workflow engine handles the mechanical execution; the campaign engine
//! handles the strategy.
//!
//! # Example
//!
//! ```no_run
//! use prism_campaign::{Campaign, CampaignConfig, CampaignGoal};
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let goal = CampaignGoal {
//!     description: "Refractory high-entropy alloy for turbine blades at 1200°C".into(),
//!     elements: vec!["W".into(), "Mo".into(), "Ta".into(), "Nb".into(), "Cr".into(), "V".into()],
//!     objective: "maximize creep resistance".into(),
//!     constraints: vec!["density < 12 g/cm³".into(), "melting_point > 2000K".into()],
//!     seeds: vec![],
//! };
//! let config = CampaignConfig {
//!     max_iterations: 100,
//!     batch_size: 10,
//!     budget_usd: Some(50.0),
//!     checkpoint_every: 10,
//!     approval_gate_at: vec![50],
//!     ..Default::default()
//! };
//! let mut campaign = Campaign::new(goal, config, "campaign-001".into());
//! let result = campaign.run().await;
//! # });
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use prism_provenance::{ActionType, Actor, ProvenanceRecord, ProvenanceStore, new_record};

const EVALUATION_TOOL: &str = "hea_descriptors";

/// Durable schedules and watchers — what wakes a paused or crashed goal back
/// up without a human. See [`schedule`] for the design rationale.
pub mod schedule;

/// The USD a tool result says it cost, or 0.0 when it says nothing. Accepts
/// the two names tools in this workspace actually emit. Never estimates: an
/// invented price would turn the budget ceiling into a fiction, and
/// [`CampaignState::budget_status`] reports "nothing ever billed" honestly
/// rather than letting 0.0 read as "safely under budget".
fn reported_cost(value: &serde_json::Value) -> f64 {
    ["cost_usd", "cost"]
        .iter()
        .find_map(|k| value.get(*k).and_then(serde_json::Value::as_f64))
        .unwrap_or(0.0)
        .max(0.0)
}

// ── Configuration ───────────────────────────────────────────────────

/// The user's discovery goal — what the campaign is trying to find.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CampaignGoal {
    /// Natural-language description of what to discover.
    pub description: String,
    /// Allowed elements (e.g. ["W", "Mo", "Ta", "Nb"]).
    /// Empty = no restriction (agent picks from full periodic table).
    #[serde(default)]
    pub elements: Vec<String>,
    /// What to optimize (e.g. "maximize creep resistance", "minimize density").
    #[serde(default)]
    pub objective: String,
    /// Hard constraints (e.g. "density < 12 g/cm³").
    #[serde(default)]
    pub constraints: Vec<String>,
    /// Seed compositions to start from (optional — if empty, the LLM proposes).
    #[serde(default)]
    pub seeds: Vec<String>,
}

/// Budget and control parameters for a campaign.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CampaignConfig {
    /// Maximum number of discovery iterations (each iteration = one batch
    /// of propose → evaluate → rank). Hard cap; campaign stops after this.
    pub max_iterations: usize,
    /// How many candidates to propose per iteration.
    pub batch_size: usize,
    /// Optional USD budget cap. If cumulative compute cost exceeds this,
    /// the campaign stops. None = no budget limit.
    #[serde(default)]
    pub budget_usd: Option<f64>,
    /// How often to checkpoint state to disk (in iterations).
    /// 0 = no checkpointing. 10 = checkpoint every 10 iterations.
    #[serde(default = "default_checkpoint_every")]
    pub checkpoint_every: usize,
    /// Iteration numbers at which to pause for human approval.
    /// The campaign stops and waits for `campaign.resume()` before
    /// continuing. Empty = no approval gates (fully autonomous).
    #[serde(default)]
    pub approval_gate_at: Vec<usize>,
    /// Path to the checkpoint directory. Defaults to `~/.prism/campaigns/`.
    #[serde(default)]
    pub checkpoint_dir: Option<PathBuf>,
    /// LLM model to use for the proposal step (empty = use default).
    #[serde(default)]
    pub llm_model: String,
    /// Temperature for the LLM proposal step (higher = more diverse).
    #[serde(default = "default_temperature")]
    pub llm_temperature: f64,
    /// Reward weights for multi-objective optimization.
    /// Maps property name → weight. e.g. {"density": -1.0, "entropy": 0.5}
    /// Negative = minimize, positive = maximize.
    #[serde(default)]
    pub reward_weights: BTreeMap<String, f64>,
    /// Base URL override for the proposal LLM. None = the configured chat
    /// target, unless `$LLM_API_BASE` is explicitly set.
    #[serde(default)]
    pub llm_base_url: Option<String>,
    /// Project root used by the shared chat-target resolver.
    /// `None` uses the current working directory.
    #[serde(default)]
    pub project_root: Option<PathBuf>,
    /// Base URL override for the PRISM node that runs the evaluation tool.
    /// None = `http://127.0.0.1:$PRISM_NODE_PORT` (default port 7327).
    #[serde(default)]
    pub node_base_url: Option<String>,
}

fn default_checkpoint_every() -> usize {
    10
}

fn default_temperature() -> f64 {
    0.7
}

impl Default for CampaignConfig {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            batch_size: 10,
            budget_usd: None,
            checkpoint_every: 10,
            approval_gate_at: Vec::new(),
            checkpoint_dir: None,
            llm_model: String::new(),
            llm_temperature: 0.7,
            reward_weights: BTreeMap::new(),
            llm_base_url: None,
            project_root: None,
            node_base_url: None,
        }
    }
}

// ── Campaign State ──────────────────────────────────────────────────

/// Lifecycle status of a goal (campaign). Every change of status goes
/// through [`Campaign::transition`], which persists one provenance record
/// per transition AND rewrites the checkpoint — the store holds the audit
/// trail, the checkpoint holds the resumable state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    /// Created and checkpointed, loop not started yet.
    #[default]
    Submitted,
    /// The propose → evaluate → rank loop is executing.
    Running,
    /// Stopped at an approval gate; resumable via `resume`.
    Paused,
    /// Terminal success — the loop finished AND at least one candidate was
    /// really evaluated. A goal can never be `Completed` otherwise.
    Completed,
    /// Terminal failure — a step could not run (LLM/evaluator unreachable)
    /// or the loop ended without a single evaluated candidate. Retryable
    /// with `campaign continue` once the cause is fixed.
    Failed,
}

impl GoalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// What the USD budget ceiling can honestly say about a goal.
///
/// `uncosted_llm_calls` is the count of completion calls this goal made whose
/// price nothing reported. [`Campaign::propose_candidates`] makes one such
/// call per iteration past seed exhaustion, through [`prism_llm::LlmClient`],
/// which honours `LLM_BASE_URL` / `LLM_API_KEY` / `MARC27_TOKEN` — so on a
/// billed backend that is real money the ceiling never sees. There is no
/// price table this crate could apply (`prism-agent` depends on
/// `prism-campaign`, not the other way round, and the campaign points at
/// whatever `LLM_BASE_URL` names), and inventing one would make the ceiling a
/// fiction. So the ceiling says out loud what it does not cover instead of
/// under-counting in silence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BudgetStatus {
    /// No ceiling was configured. The iteration cap and the scheduler's
    /// wake-up ceiling are the limits.
    NoCeiling,
    /// A ceiling is configured, real spend has been reported against it, and
    /// every billable step this goal took reported its price.
    Measured { spent: f64, ceiling: f64 },
    /// A ceiling is configured and spend has been reported, but
    /// `uncosted_llm_calls` completion calls also billed and reported no
    /// price — the ceiling under-counts by an unknown amount.
    PartiallyMeasured {
        spent: f64,
        ceiling: f64,
        uncosted_llm_calls: usize,
    },
    /// A ceiling is configured, iterations have run, and not one of them
    /// reported a cost — so the ceiling cannot fire. A defect to surface,
    /// not a green light.
    Unmeasured {
        ceiling: f64,
        uncosted_llm_calls: usize,
    },
}

/// The clause every "…but the ceiling does not cover it" message ends with.
fn uncosted_clause(calls: usize) -> String {
    let (plural, verb) = if calls == 1 { ("", "is") } else { ("s", "are") };
    format!(
        "{calls} LLM proposal call{plural} billed with no reported price and {verb} NOT in that \
         figure — the completion API returns no cost, so this ceiling cannot cover them; the \
         iteration cap and the schedule's wake-up ceiling are the limits that do"
    )
}

impl std::fmt::Display for BudgetStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCeiling => write!(f, "no USD ceiling set (iteration cap is the limit)"),
            Self::Measured { spent, ceiling } => {
                write!(f, "${spent:.4} spent of ${ceiling:.4} ceiling")
            }
            Self::PartiallyMeasured {
                spent,
                ceiling,
                uncosted_llm_calls,
            } => write!(
                f,
                "${spent:.4} spent of ${ceiling:.4} ceiling, but {}",
                uncosted_clause(*uncosted_llm_calls)
            ),
            Self::Unmeasured {
                ceiling,
                uncosted_llm_calls: 0,
            } => write!(
                f,
                "${ceiling:.4} ceiling set but NO step has reported a cost — the USD ceiling \
                 CANNOT stop this goal; the iteration cap and the schedule's wake-up ceiling are \
                 the only real limits"
            ),
            Self::Unmeasured {
                ceiling,
                uncosted_llm_calls,
            } => write!(
                f,
                "${ceiling:.4} ceiling set but NO step has reported a cost — the USD ceiling \
                 CANNOT stop this goal; {}",
                uncosted_clause(*uncosted_llm_calls)
            ),
        }
    }
}

/// A single evaluated candidate material.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    /// Composition string (e.g. "W0.3 Mo0.2 Ta0.3 Nb0.2").
    pub composition: String,
    /// Physics descriptors from the configured evaluation tool.
    #[serde(default)]
    pub properties: serde_json::Value,
    /// Scalarized reward score (higher = better).
    pub reward: f64,
    /// Which iteration produced this candidate.
    pub iteration: usize,
    /// How it was generated: "llm", "mcmc", "seed", "mutation".
    pub source: String,
}

/// Mutable state of a running campaign — checkpointed to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CampaignState {
    /// Unique campaign ID.
    pub campaign_id: String,
    /// The goal being pursued (materials campaigns). For research campaigns
    /// (`kind == Research`) this carries the description only; the research
    /// goal lives in `research_goal`.
    pub goal: CampaignGoal,
    /// The config (immutable for a given campaign).
    pub config: CampaignConfig,
    /// What kind of campaign this is — determines the iteration body.
    /// Defaults to Materials so existing checkpoints deserialize unchanged.
    #[serde(default)]
    pub kind: CampaignGoalKind,
    /// The research goal (only set when `kind == Research`). None for
    /// materials campaigns.
    #[serde(default)]
    pub research_goal: Option<ResearchCampaignGoal>,
    /// Accumulated research outcomes (only used when `kind == Research`).
    /// The materials analogue is `candidates`.
    #[serde(default)]
    pub research_outcomes: Vec<ResearchIterationOutcome>,
    /// All candidates evaluated so far, ranked by reward (best first).
    /// (Materials campaigns only.)
    pub candidates: Vec<Candidate>,
    /// Current iteration number (0-based).
    pub current_iteration: usize,
    /// Cumulative compute cost in USD, as reported by the steps that billed.
    /// It does NOT include the proposal completion calls — see
    /// `uncosted_llm_calls` and [`CampaignState::budget_status`].
    pub total_cost_usd: f64,
    /// How many completion calls this goal has made that reported no price.
    /// One per [`Campaign::propose_candidates`] call that actually reached
    /// the LLM (the seed path makes none). `#[serde(default)]` so checkpoints
    /// written before this field existed still load — they resume as 0, which
    /// under-reports the pre-upgrade calls and is the only honest default.
    #[serde(default)]
    pub uncosted_llm_calls: usize,
    /// Whether the campaign is paused at an approval gate.
    /// Kept in sync with `status` for older checkpoint readers.
    pub paused: bool,
    /// Whether the campaign completed successfully (iteration or budget cap
    /// with real evaluations). Kept in sync with `status`.
    pub completed: bool,
    /// Lifecycle status. Checkpoints written before this field existed
    /// deserialize as `Submitted` and are fixed up from the legacy flags
    /// in [`Campaign::from_checkpoint`].
    #[serde(default)]
    pub status: GoalStatus,
    /// Approval gates that already fired (iteration numbers). A gate pauses
    /// the loop exactly once — without this, `resume` would re-pause at the
    /// same gate forever.
    #[serde(default)]
    pub gates_hit: Vec<usize>,
    /// Why the campaign reached a terminal status (reason or error).
    #[serde(default)]
    pub completion_reason: String,
    /// ISO-8601 timestamp of when the campaign started.
    pub started_at: String,
    /// ISO-8601 timestamp of the last checkpoint.
    #[serde(default)]
    pub last_checkpoint_at: String,
}

impl CampaignState {
    pub fn new(campaign_id: String, goal: CampaignGoal, config: CampaignConfig) -> Self {
        Self {
            campaign_id,
            goal,
            config,
            kind: CampaignGoalKind::Materials,
            research_goal: None,
            research_outcomes: Vec::new(),
            candidates: Vec::new(),
            current_iteration: 0,
            total_cost_usd: 0.0,
            uncosted_llm_calls: 0,
            paused: false,
            completed: false,
            status: GoalStatus::Submitted,
            gates_hit: Vec::new(),
            completion_reason: String::new(),
            started_at: Utc::now().to_rfc3339(),
            last_checkpoint_at: String::new(),
        }
    }

    /// Create a research-campaign state. The `goal` field is synthesized from
    /// the research objective (so materials-shaped result/summary paths still
    /// have a description); the real goal lives in `research_goal`.
    #[must_use]
    pub fn new_research(
        campaign_id: String,
        research_goal: ResearchCampaignGoal,
        config: CampaignConfig,
    ) -> Self {
        let materials_goal = CampaignGoal {
            description: research_goal.objective.clone(),
            elements: Vec::new(),
            objective: String::new(),
            constraints: Vec::new(),
            seeds: Vec::new(),
        };
        Self {
            campaign_id,
            goal: materials_goal,
            config,
            kind: CampaignGoalKind::Research,
            research_goal: Some(research_goal),
            research_outcomes: Vec::new(),
            candidates: Vec::new(),
            current_iteration: 0,
            total_cost_usd: 0.0,
            uncosted_llm_calls: 0,
            paused: false,
            completed: false,
            status: GoalStatus::Submitted,
            gates_hit: Vec::new(),
            completion_reason: String::new(),
            started_at: Utc::now().to_rfc3339(),
            last_checkpoint_at: String::new(),
        }
    }

    /// The top-N candidates by reward.
    pub fn top_n(&self, n: usize) -> &[Candidate] {
        let len = self.candidates.len().min(n);
        &self.candidates[..len]
    }

    /// Best candidate so far (highest reward).
    pub fn best(&self) -> Option<&Candidate> {
        self.candidates.first()
    }

    /// Total number of candidates evaluated.
    pub fn total_evaluated(&self) -> usize {
        self.candidates.len()
    }

    /// Honest account of the USD ceiling.
    ///
    /// The loop stops when `total_cost_usd >= budget_usd`, but that check is
    /// only meaningful if something is actually reporting costs. When a
    /// ceiling is configured and work has run yet nothing has ever billed,
    /// the ceiling is **unenforceable** — reporting that as "under budget"
    /// would be a check that says OK about an unusable thing.
    ///
    /// The same rule applies one level down: a goal whose proposal calls
    /// billed money nothing priced has a ceiling that under-counts, and
    /// "$0.42 spent of $50 ceiling" would read as headroom it does not have.
    #[must_use]
    pub fn budget_status(&self) -> BudgetStatus {
        let Some(ceiling) = self.config.budget_usd else {
            return BudgetStatus::NoCeiling;
        };
        let uncosted_llm_calls = self.uncosted_llm_calls;
        let did_work = self.current_iteration > 0;
        if did_work && self.total_cost_usd <= 0.0 {
            return BudgetStatus::Unmeasured {
                ceiling,
                uncosted_llm_calls,
            };
        }
        if uncosted_llm_calls > 0 {
            return BudgetStatus::PartiallyMeasured {
                spent: self.total_cost_usd,
                ceiling,
                uncosted_llm_calls,
            };
        }
        BudgetStatus::Measured {
            spent: self.total_cost_usd,
            ceiling,
        }
    }

    /// Average reward across all candidates.
    pub fn avg_reward(&self) -> f64 {
        if self.candidates.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.candidates.iter().map(|c| c.reward).sum();
        sum / self.candidates.len() as f64
    }

    /// Build a human-readable summary of the campaign results.
    pub fn summary(&self, winners: &[Candidate]) -> String {
        let mut s = String::new();
        s.push_str(&format!("Campaign: {}\n", self.campaign_id));
        s.push_str(&format!("Goal: {}\n", self.goal.description));
        let status_line = match self.status {
            GoalStatus::Completed => format!("completed ({})", self.completion_reason),
            // completion_reason is already "failed: <error>" here.
            GoalStatus::Failed => self.completion_reason.clone(),
            GoalStatus::Paused => "paused (approval gate)".to_string(),
            GoalStatus::Running => "running".to_string(),
            GoalStatus::Submitted => "submitted".to_string(),
        };
        s.push_str(&format!("Status: {status_line}\n"));
        s.push_str(&format!(
            "Iterations: {} / {}\n",
            self.current_iteration, self.config.max_iterations
        ));
        s.push_str(&format!(
            "Candidates evaluated: {}\n",
            self.total_evaluated()
        ));
        s.push_str(&format!("Budget: {}\n", self.budget_status()));
        s.push_str(&format!("Avg reward: {:.4}\n", self.avg_reward()));
        if let Some(best) = self.best() {
            s.push_str(&format!(
                "Best: {} (reward={:.4})\n",
                best.composition, best.reward
            ));
        }
        if !winners.is_empty() {
            s.push_str("\nTop candidates:\n");
            for (i, c) in winners.iter().enumerate() {
                let descriptors = summarize_descriptors(&c.properties);
                s.push_str(&format!(
                    "  {}. {} — reward={:.4} (iter {}, {}){}\n",
                    i + 1,
                    c.composition,
                    c.reward,
                    c.iteration,
                    c.source,
                    descriptors
                ));
            }
        }
        s
    }
}

fn summarize_descriptors(properties: &serde_json::Value) -> String {
    const KEYS: [&str; 8] = [
        "Tm_estimate_K",
        "delta_S_mix_J_per_molK",
        "delta_H_mix_kJ_per_mol",
        "omega",
        "VEC",
        "delta_radius_pct",
        "mixing_entropy",
        "density",
    ];
    let mut descriptors = KEYS
        .iter()
        .filter_map(|key| {
            properties
                .get(key)
                .and_then(serde_json::Value::as_f64)
                .map(|value| format!("{key}={value:.4}"))
        })
        .collect::<Vec<_>>();
    if let Some(phase) = properties
        .get("phase_prediction")
        .and_then(serde_json::Value::as_str)
    {
        descriptors.push(format!("phase_prediction={phase}"));
    }

    if descriptors.is_empty() {
        String::new()
    } else {
        format!("; descriptors: {}", descriptors.join(", "))
    }
}

// ── Campaign Result ─────────────────────────────────────────────────

/// Final result of a completed campaign.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CampaignResult {
    pub campaign_id: String,
    pub goal: CampaignGoal,
    pub state: CampaignState,
    /// Top candidates by reward, limited to the requested number.
    pub winners: Vec<Candidate>,
    /// Summary text for display.
    pub summary: String,
    /// Full provenance chain (all records for this campaign).
    pub provenance: Vec<ProvenanceRecord>,
}

// ── Research-campaign generalization ────────────────────────────────
//
// The campaign engine was born materials-discovery-shaped: CampaignGoal
// carries `elements`/`objective`/`seeds` and run_iteration does propose→
// evaluate→rank. Long-form literature/knowledge research (LONG_RESEARCH_PLAN
// gap #4) is the SAME durable shape (checkpoint/budget/approval/resume) but a
// DIFFERENT iteration body: search → read → extract → cite, driven by the LLM
// tool-call loop (run_turn) rather than a hardcoded propose/evaluate.
//
// This block generalizes the goal and defines the iteration-executor seam so a
// research campaign can delegate each iteration to the agent layer WITHOUT the
// campaign crate depending on the agent crate (which would be circular). The
// agent layer implements [`ResearchIterationExecutor`] and supplies it to the
// campaign loop; the materials path is untouched.

/// The kind of campaign, determining which iteration body runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum CampaignGoalKind {
    /// Materials discovery: propose compositions → evaluate → rank (the
    /// original `run_iteration` body). The default/legacy path.
    #[default]
    Materials,
    /// Long-running research: the agent's LLM tool-call loop drives each
    /// iteration against a research goal, via a [`ResearchIterationExecutor`].
    Research,
}

/// A research campaign goal — the non-materials counterpart of
/// [`CampaignGoal`]. Natural-language objective + constraints + success
/// criteria, no composition/elements. This is what a "research this topic"
/// task packages into the durable checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ResearchCampaignGoal {
    /// The research question or objective, in the user's words.
    pub objective: String,
    /// Hard constraints (e.g. "only primary sources", "last 5 years").
    #[serde(default)]
    pub constraints: Vec<String>,
    /// What "done" looks like (e.g. "a cited report with ≥3 corroborated
    /// claims per conclusion"). The executor checks this each iteration.
    #[serde(default)]
    pub success_criteria: Vec<String>,
}

/// The outcome of one research iteration, reported by the executor back to the
/// campaign loop. Mirrors how a materials iteration reports candidates/reward,
/// but in research terms: findings, artifact references, and a self-assessed
/// progress signal.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResearchIterationOutcome {
    /// One-line summary of what this iteration accomplished (checkpointed).
    pub summary: String,
    /// Artifact references produced this iteration (provenance ids), to be
    /// carried as handles into the next iteration's context.
    #[serde(default)]
    pub artifact_refs: Vec<String>,
    /// The executor's self-assessed progress toward `success_criteria`
    /// (0.0 = just started, 1.0 = criteria met → campaign can complete).
    pub progress: f64,
    /// Optional notes to fold into the task's working memory next iteration.
    #[serde(default)]
    pub notes: Vec<String>,
    /// What this iteration actually cost, in USD, as reported by whatever
    /// billed it. This is the ONLY way a research campaign's USD ceiling can
    /// be enforced — an executor that never reports a cost leaves the
    /// ceiling unmeasurable, which [`CampaignState::budget_status`] reports
    /// rather than passing off as "under budget".
    #[serde(default)]
    pub cost_usd: f64,
}

/// Executor seam for research campaigns. Implemented by the agent layer
/// (which owns `run_turn` and the tool catalog); the campaign loop calls
/// `execute_iteration` once per iteration. Keeping this as a trait in the
/// campaign crate avoids a circular `campaign → agent` dependency: the agent
/// depends on campaign (it constructs the checkpoint), campaign depends only
/// on this trait, not on the agent crate.
///
/// The single method returns a pinned boxed future rather than using
/// `async_trait` to avoid pulling a new dependency for one seam.
pub trait ResearchIterationExecutor: Send + Sync {
    /// Run one research iteration against the given goal and the running
    /// context (prior artifacts/notes). Returns the outcome; errors abort the
    /// campaign with the error as the completion reason.
    fn execute_iteration<'a>(
        &'a self,
        goal: &'a ResearchCampaignGoal,
        context: &'a ResearchIterationContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ResearchIterationOutcome>> + Send + 'a>,
    >;
}

/// The running context passed into each research iteration: what prior
/// iterations produced. This is the campaign-side mirror of the agent's
/// `ResearchTaskContext` artifact/notes state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResearchIterationContext {
    /// Iteration number (0-based) about to run.
    pub iteration: usize,
    /// Artifact references accumulated so far (provenance ids).
    #[serde(default)]
    pub artifact_refs: Vec<String>,
    /// Working notes accumulated so far.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// The campaign orchestrator.
pub struct Campaign {
    state: CampaignState,
    provenance: Option<ProvenanceStore>,
    checkpoint_path: PathBuf,
}

#[derive(Debug)]
struct LocalNodeIdentity {
    user_id: String,
    display_name: Option<String>,
}

impl Campaign {
    /// Create a new campaign with the given goal and config.
    pub fn new(goal: CampaignGoal, config: CampaignConfig, campaign_id: String) -> Self {
        let checkpoint_dir = config.checkpoint_dir.clone().unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home).join(".prism").join("campaigns")
        });
        let checkpoint_path = checkpoint_dir.join(format!("{campaign_id}.json"));

        Self {
            state: CampaignState::new(campaign_id, goal, config),
            provenance: None,
            checkpoint_path,
        }
    }

    /// Create a new RESEARCH campaign with the given research goal + config.
    /// The iteration body is supplied externally via [`run_research`] (an
    /// executor implementing [`ResearchIterationExecutor`]).
    pub fn new_research(
        research_goal: ResearchCampaignGoal,
        config: CampaignConfig,
        campaign_id: String,
    ) -> Self {
        let checkpoint_dir = config.checkpoint_dir.clone().unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home).join(".prism").join("campaigns")
        });
        let checkpoint_path = checkpoint_dir.join(format!("{campaign_id}.json"));

        Self {
            state: CampaignState::new_research(campaign_id, research_goal, config),
            provenance: None,
            checkpoint_path,
        }
    }

    /// Resume a campaign from a checkpoint file.
    pub fn from_checkpoint(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read campaign checkpoint: {}", path.display()))?;
        let mut state: CampaignState = serde_json::from_str(&text)
            .context("failed to parse campaign checkpoint (version mismatch?)")?;
        // Back-compat: checkpoints written before `status` existed carry
        // only the legacy flags — derive the status from them.
        if state.status == GoalStatus::Submitted {
            if state.completed {
                state.status = GoalStatus::Completed;
            } else if state.paused {
                state.status = GoalStatus::Paused;
            } else if state.current_iteration > 0 {
                state.status = GoalStatus::Running;
            }
        }
        let checkpoint_path = path.to_path_buf();
        Ok(Self {
            state,
            provenance: None,
            checkpoint_path,
        })
    }

    /// Attach a provenance store. If not called, the campaign runs without
    /// provenance recording (useful for dry runs and tests).
    pub fn with_provenance(mut self, store: ProvenanceStore) -> Self {
        self.provenance = Some(store);
        self
    }

    /// Get the current campaign state (read-only).
    pub fn state(&self) -> &CampaignState {
        &self.state
    }

    /// Run the campaign to completion (or until paused at an approval gate).
    ///
    /// This is the main entry point. It loops:
    /// 1. Check budget / iteration cap / approval gates
    /// 2. Propose candidates (LLM or MCMC)
    /// 3. Evaluate each candidate
    /// 4. Rank and narrow
    /// 5. Checkpoint
    /// 6. Record provenance
    pub async fn run(&mut self) -> Result<CampaignResult> {
        // Completed is final; Failed stays re-runnable (retry after the
        // cause — evaluator down, LLM unreachable — is fixed).
        if self.state.status == GoalStatus::Completed {
            bail!(
                "campaign '{}' already completed ({}) — nothing to run",
                self.state.campaign_id,
                self.state.completion_reason
            );
        }

        info!(
            campaign = %self.state.campaign_id,
            goal = %self.state.goal.description,
            max_iterations = self.state.config.max_iterations,
            "campaign started"
        );

        if self.state.status == GoalStatus::Submitted {
            // First run of this goal: record the durable submission before
            // any step executes.
            self.record_event(
                "campaign.submitted",
                serde_json::json!({
                    "goal": self.state.goal,
                    "config": self.state.config,
                }),
            )
            .await;
        }
        self.transition(GoalStatus::Running, |_| serde_json::json!({}))
            .await?;

        if let Err(e) = self.drive().await {
            // A goal must never look alive (or worse, completed) when its
            // steps didn't run: persist the failure as a real terminal
            // transition, then propagate the error to the caller.
            self.state.completion_reason = format!("failed: {e:#}");
            if let Err(persist) = self
                .transition(
                    GoalStatus::Failed,
                    |_| serde_json::json!({ "error": format!("{e:#}") }),
                )
                .await
            {
                warn!(error = %persist, "failed to persist Failed transition");
            }
            return Err(e);
        }

        // Build result (state is now Completed or Paused).
        let winners: Vec<Candidate> = self.state.top_n(10).to_vec();

        let summary = self.state.summary(&winners);

        let provenance = if let Some(ref prov) = self.provenance {
            prov.query_by_session(&self.state.campaign_id)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        Ok(CampaignResult {
            campaign_id: self.state.campaign_id.clone(),
            goal: self.state.goal.clone(),
            state: self.state.clone(),
            winners,
            summary,
            provenance,
        })
    }

    /// The core loop. Extracted from `run` so any error becomes a persisted
    /// `Failed` transition there — a dying worker must never leave a goal
    /// with a stale non-terminal status.
    async fn drive(&mut self) -> Result<()> {
        loop {
            // Check iteration cap
            if self.state.current_iteration >= self.state.config.max_iterations {
                info!(campaign = %self.state.campaign_id, "campaign hit iteration limit");
                return self.finish("iteration_limit").await;
            }

            // Check budget
            if let Some(budget) = self.state.config.budget_usd
                && self.state.total_cost_usd >= budget
            {
                info!(
                    campaign = %self.state.campaign_id,
                    spent = self.state.total_cost_usd,
                    budget,
                    "campaign hit budget limit"
                );
                return self.finish("budget_exhausted").await;
            }

            // Check approval gate — each gate fires exactly once.
            let iter = self.state.current_iteration;
            if self.state.config.approval_gate_at.contains(&iter)
                && iter > 0
                && !self.state.gates_hit.contains(&iter)
            {
                self.state.gates_hit.push(iter);
                info!(
                    campaign = %self.state.campaign_id,
                    iteration = iter,
                    "campaign paused at approval gate"
                );
                self.transition(GoalStatus::Paused, |_| serde_json::json!({ "gate": iter }))
                    .await?;
                return Ok(());
            }

            // Run one iteration
            self.run_iteration().await?;

            // Cadence checkpoint between transitions.
            if self.state.config.checkpoint_every > 0
                && iter > 0
                && iter.is_multiple_of(self.state.config.checkpoint_every)
            {
                self.checkpoint()?;
            }
        }
    }

    /// Terminal transition for a loop that ended on its own (iteration or
    /// budget cap). Honesty gate: a goal that never evaluated a single
    /// candidate did NOT do its work — that is a failure, not a completion,
    /// no matter which cap tripped.
    async fn finish(&mut self, reason: &str) -> Result<()> {
        if self.state.total_evaluated() == 0 {
            bail!("{reason} reached with zero candidates evaluated — steps never ran");
        }
        self.state.completion_reason = reason.to_string();
        self.transition(GoalStatus::Completed, |state| {
            let winners: Vec<Candidate> = state.top_n(10).to_vec();
            serde_json::json!({
                "reason": state.completion_reason,
                "iterations": state.current_iteration,
                "candidates": state.total_evaluated(),
                "best_reward": state.best().map(|c| c.reward).unwrap_or(0.0),
                "summary": state.summary(&winners),
                "winners": winners,
            })
        })
        .await
    }

    /// Move the goal to `status`, write the transition to the provenance
    /// store (if attached), and rewrite the checkpoint. The legacy
    /// `paused`/`completed` flags stay in sync for older readers. `build`
    /// produces the event payload AFTER the status is applied, so payloads
    /// (e.g. the terminal summary) see the final state.
    async fn transition(
        &mut self,
        status: GoalStatus,
        build: impl FnOnce(&CampaignState) -> serde_json::Value,
    ) -> Result<()> {
        let from = self.state.status;
        self.state.status = status;
        self.state.paused = status == GoalStatus::Paused;
        self.state.completed = status == GoalStatus::Completed;
        let mut data = build(&self.state);
        if let Some(obj) = data.as_object_mut() {
            obj.insert("from".into(), serde_json::json!(from.as_str()));
            obj.insert(
                "iteration".into(),
                serde_json::json!(self.state.current_iteration),
            );
        }
        self.record_event(&format!("campaign.status.{}", status.as_str()), data)
            .await;
        self.checkpoint()
    }

    /// Resume a paused campaign (after human approval at a gate).
    pub async fn resume(&mut self) -> Result<CampaignResult> {
        if self.state.status != GoalStatus::Paused {
            bail!("campaign is not paused — nothing to resume");
        }
        info!(
            campaign = %self.state.campaign_id,
            iteration = self.state.current_iteration,
            "campaign resumed from approval gate"
        );
        self.run().await
    }

    /// Run a RESEARCH campaign: the same durable loop (budget / iteration cap /
    /// approval gate / checkpoint / provenance) as materials `run`, but each
    /// iteration is driven by an external [`ResearchIterationExecutor`] (the
    /// agent layer's `AgentResearchExecutor`, which runs a `run_turn` against
    /// the research goal). The campaign completes when the executor reports
    /// `progress >= 1.0` (success criteria met) OR the caps are hit.
    ///
    /// Reuses the materials loop's invariants (LONG_RESEARCH_PLAN): no
    /// blocking calls inside the loop, the checkpoint is the truth, honest
    /// degradation. The materials `run` path is untouched.
    pub async fn run_research(
        &mut self,
        executor: &dyn ResearchIterationExecutor,
    ) -> Result<CampaignResult> {
        let research_goal = self
            .state
            .research_goal
            .clone()
            .ok_or_else(|| anyhow::anyhow!("run_research called on a non-research campaign"))?;

        info!(
            campaign = %self.state.campaign_id,
            objective = %research_goal.objective,
            max_iterations = self.state.config.max_iterations,
            "research campaign started"
        );

        self.record_event(
            "campaign.start",
            serde_json::json!({
                "kind": "research",
                "objective": research_goal.objective,
                "constraints": research_goal.constraints,
                "success_criteria": research_goal.success_criteria,
            }),
        )
        .await;

        while !self.state.completed && !self.state.paused {
            // Iteration cap.
            if self.state.current_iteration >= self.state.config.max_iterations {
                self.state.completed = true;
                self.state.completion_reason = "iteration_limit".into();
                info!(campaign = %self.state.campaign_id, "research campaign hit iteration limit");
                break;
            }
            // Budget cap.
            if let Some(budget) = self.state.config.budget_usd
                && self.state.total_cost_usd >= budget
            {
                self.state.completed = true;
                self.state.completion_reason = "budget_exhausted".into();
                info!(campaign = %self.state.campaign_id, "research campaign hit budget limit");
                break;
            }
            // Approval gate. Recorded exactly like the materials loop's:
            // `transition` sets `status` (not just the legacy `paused` flag),
            // writes the provenance event, and checkpoints; `gates_hit` stops
            // the same gate re-pausing a resumed campaign forever. Setting
            // only `paused` left the checkpoint claiming `status: submitted`
            // while the goal sat at a gate — a scheduler reading `status`
            // would have seen a resumable goal and walked straight through
            // the approval.
            let iter = self.state.current_iteration;
            if self.state.config.approval_gate_at.contains(&iter)
                && iter > 0
                && !self.state.gates_hit.contains(&iter)
            {
                self.state.gates_hit.push(iter);
                info!(
                    campaign = %self.state.campaign_id,
                    iteration = iter,
                    "research campaign paused at approval gate"
                );
                self.transition(GoalStatus::Paused, |_| serde_json::json!({ "gate": iter }))
                    .await?;
                break;
            }

            // Build the running context from accumulated outcomes.
            let (prior_artifacts, prior_notes) = self.accumulated_research_state();
            let context = ResearchIterationContext {
                iteration: iter,
                artifact_refs: prior_artifacts,
                notes: prior_notes,
            };

            // Drive one research turn via the executor.
            let outcome = executor.execute_iteration(&research_goal, &context).await?;
            info!(
                campaign = %self.state.campaign_id,
                iteration = iter,
                progress = outcome.progress,
                summary = %outcome.summary,
                "research iteration complete"
            );

            // Spend accrues BEFORE the next budget check at the loop top, so
            // the ceiling is tested against what has really been billed.
            self.state.total_cost_usd += outcome.cost_usd.max(0.0);

            self.record_event(
                "campaign.research.iter",
                serde_json::json!({
                    "iteration": iter,
                    "summary": outcome.summary,
                    "progress": outcome.progress,
                    "artifacts": outcome.artifact_refs,
                    "notes": outcome.notes,
                    "cost_usd": outcome.cost_usd,
                    "total_cost_usd": self.state.total_cost_usd,
                }),
            )
            .await;

            self.state.research_outcomes.push(outcome.clone());
            self.state.current_iteration = iter + 1;

            // Success-criteria completion: the executor signalled done.
            if outcome.progress >= 1.0 {
                self.state.completed = true;
                self.state.completion_reason = "success_criteria_met".into();
                info!(
                    campaign = %self.state.campaign_id,
                    "research campaign completed: success criteria met"
                );
            }

            // Periodic checkpoint.
            if self.state.config.checkpoint_every > 0
                && iter > 0
                && iter.is_multiple_of(self.state.config.checkpoint_every)
            {
                self.checkpoint()?;
            }
        }

        if self.state.completed {
            self.record_event(
                "campaign.complete",
                serde_json::json!({
                    "reason": self.state.completion_reason,
                    "iterations": self.state.current_iteration,
                    "research_steps": self.state.research_outcomes.len(),
                }),
            )
            .await;
            // Write the terminal STATUS, not just the legacy `completed`
            // flag. `from_checkpoint`'s flag→status fixup only fires while
            // status is still `Submitted`, so a research goal that had ever
            // paused at a gate (which now sets `status: paused` for real)
            // would otherwise stay "paused" forever after finishing — every
            // reader, this scheduler included, would treat a done goal as
            // still waiting on a human.
            self.transition(GoalStatus::Completed, |state| {
                serde_json::json!({
                    "reason": state.completion_reason,
                    "research_steps": state.research_outcomes.len(),
                })
            })
            .await?;
        } else {
            self.checkpoint()?;
        }

        // Build a research-shaped result. winners/candidates are empty (those
        // are materials concepts); the research summary + artifact refs live
        // in `state.research_outcomes` and the summary string.
        let summary = self.build_research_summary();
        let provenance = if let Some(ref prov) = self.provenance {
            prov.query_by_session(&self.state.campaign_id)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        Ok(CampaignResult {
            campaign_id: self.state.campaign_id.clone(),
            goal: self.state.goal.clone(),
            state: self.state.clone(),
            winners: Vec::new(),
            summary,
            provenance,
        })
    }

    /// Collect accumulated artifact refs + notes from prior research outcomes.
    fn accumulated_research_state(&self) -> (Vec<String>, Vec<String>) {
        let mut artifacts = Vec::new();
        let mut notes = Vec::new();
        for o in &self.state.research_outcomes {
            artifacts.extend(o.artifact_refs.iter().cloned());
            notes.extend(o.notes.iter().cloned());
        }
        (artifacts, notes)
    }

    /// Build a human-readable summary of a research campaign's progress.
    fn build_research_summary(&self) -> String {
        let outcomes = &self.state.research_outcomes;
        let mut lines = vec![
            format!("Research campaign: {}", self.state.goal.description),
            format!("Iterations: {}", self.state.current_iteration),
            format!("Status: {}", {
                if self.state.completed {
                    if self.state.completion_reason == "success_criteria_met" {
                        "complete (success criteria met)".to_string()
                    } else {
                        format!("complete ({})", self.state.completion_reason)
                    }
                } else if self.state.paused {
                    "paused at approval gate".to_string()
                } else {
                    "in progress".to_string()
                }
            }),
        ];
        if !outcomes.is_empty() {
            lines.push("Steps:".into());
            for (i, o) in outcomes.iter().enumerate() {
                lines.push(format!(
                    "  {}. [progress {:.0}%] {}",
                    i + 1,
                    o.progress * 100.0,
                    o.summary
                ));
            }
        }
        lines.join("\n")
    }

    /// Run a single discovery iteration: propose → evaluate → rank.
    async fn run_iteration(&mut self) -> Result<()> {
        let iter = self.state.current_iteration;
        info!(
            campaign = %self.state.campaign_id,
            iteration = iter,
            "starting iteration"
        );

        // ── 1. Propose candidates ────────────────────────────────────
        let proposals = self.propose_candidates().await?;

        // ── 2. Evaluate each candidate ───────────────────────────────
        let mut evaluated: Vec<Candidate> = Vec::new();
        let mut last_err: Option<anyhow::Error> = None;
        for comp in &proposals {
            match self.evaluate_candidate(comp, iter).await {
                Ok(candidate) => evaluated.push(candidate),
                Err(e) => {
                    warn!(
                        campaign = %self.state.campaign_id,
                        composition = %comp,
                        error = %e,
                        "evaluation failed for candidate"
                    );
                    last_err = Some(e);
                }
            }
        }

        // An iteration where every single evaluation failed is not
        // progress — it is the evaluator being down. Fail the goal instead
        // of spinning to the iteration cap and "completing" with nothing.
        if evaluated.is_empty() && !proposals.is_empty() {
            let detail = last_err
                .map(|e| format!("{e:#}"))
                .unwrap_or_else(|| "unknown error".into());
            bail!(
                "iteration {iter}: all {} candidate evaluations failed — {detail}",
                proposals.len()
            );
        }
        let n_evaluated = evaluated.len();

        // Accrue whatever the evaluator actually reported spending. Read, do
        // not estimate: a made-up price would make the ceiling a fiction.
        //
        // This is the EVALUATOR's spend only. Step 1 above also billed a
        // completion call whose price nothing reports; that one is counted in
        // `state.uncosted_llm_calls` and disclosed by `budget_status` rather
        // than folded in here at an invented rate.
        let iteration_cost: f64 = evaluated.iter().map(|c| reported_cost(&c.properties)).sum();
        self.state.total_cost_usd += iteration_cost;

        // ── 3. Rank by reward (descending) ───────────────────────────
        evaluated.sort_by(|a, b| {
            b.reward
                .partial_cmp(&a.reward)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // ── 4. Merge into state ──────────────────────────────────────
        // Insert in reward order so state.candidates stays sorted.
        for candidate in evaluated {
            // Insert maintaining sorted order (best first).
            let pos = self
                .state
                .candidates
                .partition_point(|c| c.reward > candidate.reward);
            self.state.candidates.insert(pos, candidate);
        }

        self.state.current_iteration = iter + 1;

        // Persist the per-iteration progress transition — the durable trail
        // must show every step, not only the terminal status.
        self.record_event(
            "campaign.iteration",
            serde_json::json!({
                "iteration": iter,
                "proposed": proposals.len(),
                "evaluated": n_evaluated,
                "total_evaluated": self.state.total_evaluated(),
                "best_reward": self.state.best().map(|c| c.reward),
                "best": self.state.best().map(|c| c.composition.clone()),
            }),
        )
        .await;

        if let Some(best) = self.state.best() {
            info!(
                campaign = %self.state.campaign_id,
                iteration = iter,
                evaluated = self.state.total_evaluated(),
                best_reward = best.reward,
                best = %best.composition,
                "iteration complete"
            );
        }

        Ok(())
    }

    /// Propose candidate compositions for this iteration.
    ///
    /// On iteration 0, uses seed data or asks the LLM to propose.
    /// On later iterations, asks the LLM to propose variations around
    /// the best-performing candidates so far (adaptive narrowing).
    async fn propose_candidates(&mut self) -> Result<Vec<String>> {
        let batch = self.state.config.batch_size;
        let iter = self.state.current_iteration;

        if iter == 0 && !self.state.goal.seeds.is_empty() {
            // Use provided seeds for the first iteration.
            let seeds: Vec<String> = self.state.goal.seeds.iter().take(batch).cloned().collect();
            return Ok(seeds);
        }

        // Build the LLM prompt for proposal.
        let prompt = self.build_proposal_prompt(batch);

        // Use the shared chat-target resolver for the proposal LLM. An
        // explicit endpoint override remains an escape hatch, but the
        // resolver is still attempted first so it supplies the selected
        // target's model and credentials when available.
        let explicit_base_url = std::env::var("LLM_API_BASE")
            .ok()
            .or_else(|| self.state.config.llm_base_url.clone())
            .or_else(|| std::env::var("LLM_BASE_URL").ok());
        let project_root = self
            .state
            .config
            .project_root
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let paths = prism_runtime::PrismPaths::discover()
            .context("failed to locate PRISM state directories for campaign LLM resolution")?;
        let resolved = match prism_runtime::llm_resolve::resolve_llm(&project_root, &paths) {
            Ok(resolved) => Some(resolved),
            Err(error) if explicit_base_url.is_some() => {
                debug!(error = %error, "using explicit campaign LLM endpoint override");
                None
            }
            Err(error) => return Err(error),
        };
        let base_url = explicit_base_url
            .or_else(|| resolved.as_ref().map(|llm| llm.base_url.clone()))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No LLM endpoint resolved. Run `prism use marc27 --model <model>` for the hosted platform or `prism use local --url <url> --model <model>` for a local model."
                )
            })?;
        let api_key = resolved
            .as_ref()
            .and_then(|llm| llm.api_key.clone())
            .or_else(|| {
                std::env::var("LLM_API_KEY")
                    .or_else(|_| std::env::var("MARC27_TOKEN"))
                    .ok()
            });
        let model = if self.state.config.llm_model.is_empty() {
            resolved
                .as_ref()
                .map(|llm| llm.model.clone())
                .filter(|model| !model.is_empty())
                .or_else(|| std::env::var("LLM_MODEL").ok())
                .unwrap_or_else(|| "gemma-4-12b".to_string())
        } else {
            self.state.config.llm_model.clone()
        };

        let config = prism_llm::LlmConfig {
            base_url,
            api_key,
            model: model.clone(),
            embedding_model: resolved.and_then(|llm| llm.embedding_model),
            ..Default::default()
        };
        let client = prism_llm::LlmClient::new(config);

        let system = "You are a materials scientist designing novel alloys. \
                      Respond with ONLY a JSON array of composition strings, \
                      no explanation. Example: [\"W0.3 Mo0.2 Ta0.3 Nb0.2\", \"Cr0.4 V0.3 Ti0.3\"]";

        // Counted BEFORE the await: a call that errors mid-flight may still
        // have been billed, and a ceiling that only counts successes
        // under-reports in exactly the case worth reporting.
        //
        // `LlmClient::chat` returns `Result<String>` — no usage, no cost — so
        // this spend cannot be added to `total_cost_usd`. It is not an
        // estimate withheld; there is nothing to estimate from that would not
        // be invented. `budget_status` reports the gap instead.
        self.state.uncosted_llm_calls += 1;
        let response = client
            .chat(system, &prompt)
            .await
            .context("LLM proposal call failed")?;

        self.record_event(
            "campaign.propose",
            serde_json::json!({
                "iteration": iter,
                "prompt": prompt,
                "response": &response,
                "uncosted_llm_calls": self.state.uncosted_llm_calls,
            }),
        )
        .await;

        // Parse the response — expect a JSON array of composition strings.
        let compositions = self.parse_compositions(&response);

        if compositions.is_empty() {
            // No synthetic proposals, ever: a campaign that cannot get
            // parseable proposals from the model HALTS instead of running
            // on fabricated compositions.
            warn!(
                campaign = %self.state.campaign_id,
                iteration = iter,
                raw = %response,
                "LLM returned no parseable compositions; halting proposal step"
            );
            anyhow::bail!(
                "proposal step failed: LLM returned no parseable compositions                  (campaign halted rather than proposing synthetic candidates)"
            );
        }

        Ok(compositions.into_iter().take(batch).collect())
    }

    /// Build the LLM prompt for the proposal step.
    fn build_proposal_prompt(&self, batch: usize) -> String {
        let mut prompt = format!(
            "Goal: {}\nObjective: {}\n",
            self.state.goal.description, self.state.goal.objective
        );

        if !self.state.goal.elements.is_empty() {
            prompt.push_str(&format!(
                "Allowed elements: {}\n",
                self.state.goal.elements.join(", ")
            ));
        }

        if !self.state.goal.constraints.is_empty() {
            prompt.push_str(&format!(
                "Constraints: {}\n",
                self.state.goal.constraints.join("; ")
            ));
        }

        if self.state.current_iteration > 0 && !self.state.candidates.is_empty() {
            // Show the LLM the top performers so it can narrow the search.
            prompt.push_str("\nBest candidates so far (composition → reward):\n");
            for c in self.state.top_n(5) {
                prompt.push_str(&format!("  {} → {:.4}\n", c.composition, c.reward));
            }
            prompt.push_str(&format!(
                "\nPropose {} NEW compositions that improve on these. \
                 Vary the ratios and try new element combinations within the allowed set.\n",
                batch
            ));
        } else {
            prompt.push_str(&format!(
                "\nPropose {} initial candidate compositions.\n",
                batch
            ));
        }

        prompt
    }

    /// Parse composition strings from an LLM response.
    /// Handles JSON arrays, newline-separated lists, and free text.
    fn parse_compositions(&self, text: &str) -> Vec<String> {
        // Try JSON array first.
        if let Ok(arr) = serde_json::from_str::<Vec<String>>(text.trim()) {
            return arr;
        }

        // Try to find a JSON array anywhere in the text.
        if let Some(start) = text.find('[')
            && let Some(end) = text[start..].find(']')
        {
            let json = &text[start..start + end + 1];
            if let Ok(arr) = serde_json::from_str::<Vec<String>>(json) {
                return arr;
            }
        }

        // Fall back to line-by-line parsing — each non-empty line that
        // looks like a composition (contains an element symbol + fraction).
        let mut comps = Vec::new();
        for line in text.lines() {
            let line = line.trim().trim_start_matches(|c: char| {
                c == '-' || c == '*' || c == '•' || c == '.' || c == ' '
            });
            if line.is_empty() || line.len() < 3 {
                continue;
            }
            // Heuristic: contains at least one uppercase letter followed by
            // a digit or another uppercase letter.
            let looks_like_comp = line.chars().any(|c| c.is_ascii_uppercase())
                && line.chars().any(|c| c.is_ascii_digit() || c == '.');
            if looks_like_comp && !line.starts_with("Propose") && !line.starts_with("Goal") {
                comps.push(line.to_string());
            }
        }
        comps
    }

    /// Evaluate a single candidate composition.
    ///
    /// Calls the registered HEA descriptor tool via the local PRISM node API
    /// and computes a scalarized reward from the returned physics descriptors.
    async fn evaluate_candidate(&self, composition: &str, iteration: usize) -> Result<Candidate> {
        // Call the PRISM node's registered HEA evaluation tool.
        let base = self.state.config.node_base_url.clone().unwrap_or_else(|| {
            let port = std::env::var("PRISM_NODE_PORT").unwrap_or_else(|_| "7327".to_string());
            format!("http://127.0.0.1:{port}")
        });
        let paths = prism_runtime::PrismPaths::discover().context(
            "failed to locate PRISM state directories; authenticate with `prism login --no-browser` and retry",
        )?;
        let identity = load_local_node_identity(&paths)?;
        let resp_body = call_evaluate_material(&base, composition, Some(&identity)).await?;

        // Compute scalarized reward from the properties.
        let reward = self.compute_reward(&resp_body)?;

        self.record_event(
            "campaign.evaluate",
            serde_json::json!({
                "iteration": iteration,
                "composition": composition,
                "properties": &resp_body,
                "reward": reward,
            }),
        )
        .await;

        Ok(Candidate {
            composition: composition.to_string(),
            properties: resp_body,
            reward,
            iteration,
            source: if iteration == 0 && !self.state.goal.seeds.is_empty() {
                "seed".into()
            } else {
                "llm".into()
            },
        })
    }

    /// Compute a scalarized reward from physics descriptors.
    ///
    /// Uses `config.reward_weights` to combine multiple properties into
    /// a single score. If no weights are configured, uses a default
    /// heuristic: higher mixing entropy and lower density = better.
    fn compute_reward(&self, props: &serde_json::Value) -> Result<f64> {
        if self.state.config.reward_weights.is_empty() {
            let objective = self.state.goal.objective.to_ascii_lowercase();
            if objective.contains("melting point") {
                let melting_point = props
                    .get("Tm_estimate_K")
                    .or_else(|| props.get("melting_point_k"))
                    .or_else(|| props.get("melting_point"))
                    .and_then(serde_json::Value::as_f64)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "{EVALUATION_TOOL} returned no numeric melting-point descriptor for objective '{}'",
                            self.state.goal.objective
                        )
                    })?;
                return Ok(if objective.contains("minimize") {
                    -melting_point
                } else {
                    melting_point
                });
            }

            // Default heuristic: reward high entropy and, when available,
            // lower density. Missing descriptors are not replaced with
            // invented defaults.
            let entropy = props
                .get("mixing_entropy")
                .or_else(|| props.get("entropy"))
                .and_then(serde_json::Value::as_f64)
                .or_else(|| {
                    props
                        .get("delta_S_mix_J_per_molK")
                        .and_then(serde_json::Value::as_f64)
                        .map(|value| value / 8.314)
                });
            let density = props.get("density").and_then(serde_json::Value::as_f64);
            return match (entropy, density) {
                (Some(entropy), Some(density)) => {
                    let entropy_score = entropy / 2.0;
                    let density_score = 1.0 - (density / 20.0).clamp(0.0, 1.0);
                    Ok(entropy_score * 0.6 + density_score * 0.4)
                }
                (Some(entropy), None) => Ok(entropy / 2.0),
                (None, Some(density)) => Ok(1.0 - (density / 20.0).clamp(0.0, 1.0)),
                (None, None) => bail!(
                    "{EVALUATION_TOOL} returned no numeric descriptors supported by the campaign reward function"
                ),
            };
        }

        // Weighted sum of named properties. Every configured property must be
        // present; silently substituting zero would fabricate a ranking.
        let mut reward = 0.0;
        for (prop, weight) in &self.state.config.reward_weights {
            let value = props
                .get(prop)
                .and_then(serde_json::Value::as_f64)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{EVALUATION_TOOL} returned no numeric value for weighted property '{prop}'"
                    )
                })?;
            reward += value * weight;
        }
        Ok(reward)
    }

    /// Save campaign state to a checkpoint file.
    /// Persist current state to the checkpoint file. Public so a caller can
    /// write the INITIAL checkpoint before detaching the loop into a
    /// background process — the goal id must exist on disk (and thus at
    /// `GET /api/goals`) the moment `--detach` returns, not only after the
    /// first `checkpoint_every` iterations.
    /// The write is atomic — a temp file in the same directory, then a
    /// rename. `std::fs::write` truncates first, so a crash mid-write (the
    /// exact event this checkpoint exists to survive) left a half-written
    /// file that `from_checkpoint` could not parse: the goal, its budget and
    /// all its accumulated work, gone. A rename either happens or does not.
    pub fn checkpoint(&mut self) -> Result<()> {
        self.state.last_checkpoint_at = Utc::now().to_rfc3339();
        if let Some(parent) = self.checkpoint_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&self.state)?;
        let tmp = self
            .checkpoint_path
            .with_extension(format!("tmp-{}", std::process::id()));
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp)
                .with_context(|| format!("failed to open temp checkpoint {}", tmp.display()))?;
            file.write_all(text.as_bytes())?;
            // Durability of the CONTENT before the rename publishes it —
            // without this the rename can land ahead of the bytes and a
            // power loss leaves a valid name pointing at an empty file.
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &self.checkpoint_path).with_context(|| {
            format!(
                "failed to publish checkpoint {}",
                self.checkpoint_path.display()
            )
        })?;
        debug!(
            campaign = %self.state.campaign_id,
            path = %self.checkpoint_path.display(),
            iteration = self.state.current_iteration,
            "checkpoint saved"
        );
        Ok(())
    }

    /// Record a campaign event to the provenance store (if attached).
    async fn record_event(&self, action: &str, data: serde_json::Value) {
        if let Some(ref prov) = self.provenance {
            let rec = new_record(
                &self.state.campaign_id,
                ActionType::Workflow,
                Actor::Agent,
                Some(action),
                None,
                data,
            );
            if let Err(e) = prov.record(&rec).await {
                warn!(error = %e, "failed to record campaign provenance");
            }
        }
    }
}

fn load_local_node_identity(paths: &prism_runtime::PrismPaths) -> Result<LocalNodeIdentity> {
    let state = paths.load_cli_state().with_context(|| {
        "failed to read PRISM credentials; authenticate with `prism login --no-browser` and retry"
    })?;
    let credentials = state.credentials.ok_or_else(|| {
        anyhow::anyhow!(
            "campaign evaluation requires a PRISM identity; authenticate with `prism login --no-browser` (or `prism login --token <PAT>` for non-interactive authentication) and retry"
        )
    })?;
    let user_id = credentials.user_id.ok_or_else(|| {
        anyhow::anyhow!(
            "stored PRISM credentials have no user identity; re-authenticate with `prism login --no-browser` and retry"
        )
    })?;

    Ok(LocalNodeIdentity {
        user_id,
        display_name: credentials.display_name,
    })
}

async fn call_evaluate_material(
    base: &str,
    composition: &str,
    identity: Option<&LocalNodeIdentity>,
) -> Result<serde_json::Value> {
    let identity = identity.ok_or_else(|| {
        anyhow::anyhow!(
            "campaign evaluation has no PRISM identity; authenticate with `prism login --no-browser` (or `prism login --token <PAT>` for non-interactive authentication) and retry"
        )
    })?;
    let node_token = prism_client::node_session::mint_local_session(
        base,
        &identity.user_id,
        identity.display_name.as_deref(),
    )
    .await
    .with_context(|| {
        format!(
            "could not establish an authenticated session with the PRISM node at {base}; run `prism node up` and retry. If the stored identity is no longer valid, re-authenticate with `prism login --no-browser`"
        )
    })?;

    let url = format!(
        "{}/api/tools/{EVALUATION_TOOL}/run",
        base.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "inputs": { "composition": composition },
    });

    let resp = reqwest::Client::new()
        .post(&url)
        .header("Authorization", format!("Bearer {node_token}"))
        .json(&body)
        .send()
        .await
        .with_context(|| {
            format!("failed to reach the PRISM node at {base}; run `prism node up` and retry")
        })?;

    let status = resp.status();
    let response_text = resp
        .text()
        .await
        .with_context(|| format!("failed to read {EVALUATION_TOOL} response"))?;
    let resp_body: serde_json::Value = serde_json::from_str(&response_text).with_context(|| {
        format!("{EVALUATION_TOOL} returned a non-JSON response (HTTP {status})")
    })?;

    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        bail!(
            "the PRISM node rejected the campaign's minted session (HTTP {status}): {resp_body}. Re-authenticate with `prism login --no-browser`, run `prism node up`, and retry"
        );
    }
    if !status.is_success() {
        bail!("{EVALUATION_TOOL} returned HTTP {status}: {resp_body}");
    }

    extract_evaluation_result(resp_body)
}

fn extract_evaluation_result(mut value: serde_json::Value) -> Result<serde_json::Value> {
    loop {
        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
            bail!("{EVALUATION_TOOL} failed: {error}");
        }
        if value.get("success").and_then(serde_json::Value::as_bool) == Some(false) {
            bail!("{EVALUATION_TOOL} reported an unsuccessful evaluation: {value}");
        }

        let Some(object) = value.as_object() else {
            bail!("{EVALUATION_TOOL} returned an invalid descriptor payload: {value}");
        };
        let is_node_envelope = object.contains_key("tool") && object.contains_key("result");
        let is_python_envelope = object.len() == 1 && object.contains_key("result");
        if is_node_envelope || is_python_envelope {
            let Some(result) = object.get("result").cloned() else {
                bail!("{EVALUATION_TOOL} returned an invalid result envelope: {value}");
            };
            value = result;
            continue;
        }
        return Ok(value);
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::Json;
    use axum::routing::post;
    use serde_json::{Value, json};
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl Into<OsString>) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: tests serialize environment changes with ENV_LOCK.
            unsafe { std::env::set_var(key, value.into()) };
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: tests serialize environment changes with ENV_LOCK.
            unsafe { std::env::remove_var(key) };
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: tests serialize environment changes with ENV_LOCK.
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    async fn configured_target_chat(
        State(model): State<Arc<Mutex<Option<String>>>>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        *model.lock().unwrap() = body["model"].as_str().map(str::to_string);
        Json(json!({
            "choices": [{
                "message": { "content": "[\"W0.5 Mo0.5\"]" }
            }]
        }))
    }

    #[derive(Clone, Default)]
    struct GatedEvaluationNode {
        session_calls: Arc<AtomicUsize>,
        auth_headers: Arc<Mutex<Vec<Option<String>>>>,
    }

    async fn mint_test_node_session(State(node): State<GatedEvaluationNode>) -> Json<Value> {
        node.session_calls.fetch_add(1, Ordering::SeqCst);
        Json(json!({"session_id": "campaign-node-session"}))
    }

    async fn gated_evaluate_material(
        State(node): State<GatedEvaluationNode>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        let auth = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        node.auth_headers.lock().unwrap().push(auth.clone());
        if auth.as_deref() != Some("Bearer campaign-node-session") {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "missing or invalid session token"})),
            );
        }

        (
            StatusCode::OK,
            Json(json!({
                "tool": "hea_descriptors",
                "result": {
                    "result": {
                        "composition": body["inputs"]["composition"],
                        "mixing_entropy": 1.5,
                        "density": 12.0,
                    }
                }
            })),
        )
    }

    async fn tool_error_evaluate_material() -> Json<Value> {
        Json(json!({
            "tool": "hea_descriptors",
            "result": {
                "error": "descriptor engine failed"
            }
        }))
    }

    #[tokio::test]
    async fn evaluation_mints_local_session_and_sends_bearer() {
        let node = GatedEvaluationNode::default();
        let app = axum::Router::new()
            .route("/api/sessions", post(mint_test_node_session))
            .route(
                "/api/tools/hea_descriptors/run",
                post(gated_evaluate_material),
            )
            .with_state(node.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let identity = LocalNodeIdentity {
            user_id: "campaign-user".into(),
            display_name: Some("Campaign User".into()),
        };

        let properties = call_evaluate_material(&base, "W0.5 Mo0.5", Some(&identity))
            .await
            .expect("authenticated evaluation should succeed");

        assert_eq!(properties["mixing_entropy"], 1.5);
        assert_eq!(node.session_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            node.auth_headers.lock().unwrap().as_slice(),
            &[Some("Bearer campaign-node-session".into())]
        );
        server.abort();
    }

    #[tokio::test]
    async fn evaluation_rejects_tool_error_payload() {
        let app = axum::Router::new()
            .route("/api/sessions", post(mint_test_node_session))
            .route(
                "/api/tools/hea_descriptors/run",
                post(tool_error_evaluate_material),
            )
            .with_state(GatedEvaluationNode::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let identity = LocalNodeIdentity {
            user_id: "campaign-user".into(),
            display_name: None,
        };

        let error = call_evaluate_material(&base, "W0.5 Mo0.5", Some(&identity))
            .await
            .expect_err("a tool error payload must halt evaluation");

        assert!(
            error.to_string().contains("descriptor engine failed"),
            "error: {error:#}"
        );
        server.abort();
    }

    #[test]
    fn evaluation_without_local_identity_has_authentication_guidance() {
        let temp = tempfile::tempdir().unwrap();
        let paths = prism_runtime::PrismPaths {
            config_dir: temp.path().join("config"),
            cache_dir: temp.path().join("cache"),
            data_dir: temp.path().join("data"),
            state_dir: temp.path().join("state"),
        };

        let error =
            load_local_node_identity(&paths).expect_err("missing credentials must halt evaluation");
        let message = format!("{error:#}");
        assert!(
            message.contains("prism login --no-browser"),
            "error: {message}"
        );
    }

    #[tokio::test]
    async fn unreachable_node_has_startup_guidance() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let identity = LocalNodeIdentity {
            user_id: "campaign-user".into(),
            display_name: None,
        };

        let error = call_evaluate_material(&base, "W0.5 Mo0.5", Some(&identity))
            .await
            .expect_err("an unreachable node must halt evaluation");
        let message = format!("{error:#}");
        assert!(message.contains("prism node up"), "error: {message}");
    }

    fn test_goal() -> CampaignGoal {
        CampaignGoal {
            description: "High-strength Ti alloy".into(),
            elements: vec![
                "Ti".into(),
                "Al".into(),
                "V".into(),
                "Cr".into(),
                "Mo".into(),
            ],
            objective: "maximize strength-to-weight ratio".into(),
            constraints: vec!["density < 5 g/cm³".into()],
            seeds: vec!["Ti0.9 Al0.06 V0.04".into()],
        }
    }

    #[test]
    fn campaign_state_new_initializes_correctly() {
        let state = CampaignState::new("c1".into(), test_goal(), CampaignConfig::default());
        assert_eq!(state.current_iteration, 0);
        assert!(!state.completed);
        assert!(!state.paused);
        assert_eq!(state.total_evaluated(), 0);
    }

    #[test]
    fn budget_ceiling_reports_unmeasured_instead_of_pretending_to_be_under() {
        let config = CampaignConfig {
            budget_usd: Some(25.0),
            ..Default::default()
        };
        let mut state = CampaignState::new("c1".into(), test_goal(), config);
        // Nothing has run yet — a ceiling with no spend is simply untouched.
        assert_eq!(
            state.budget_status(),
            BudgetStatus::Measured {
                spent: 0.0,
                ceiling: 25.0
            }
        );
        // Iterations ran and nothing billed: the ceiling cannot fire, and the
        // status must say so rather than read as "$0.00 of $25.00, fine".
        state.current_iteration = 4;
        assert_eq!(
            state.budget_status(),
            BudgetStatus::Unmeasured {
                ceiling: 25.0,
                uncosted_llm_calls: 0
            }
        );
        assert!(
            state.budget_status().to_string().contains("CANNOT stop"),
            "the unmeasured case must name the defect: {}",
            state.budget_status()
        );
        // Once something bills, it becomes a real measurement again.
        state.total_cost_usd = 3.25;
        assert_eq!(
            state.budget_status(),
            BudgetStatus::Measured {
                spent: 3.25,
                ceiling: 25.0
            }
        );
    }

    #[test]
    fn ceiling_declares_the_proposal_spend_it_cannot_see() {
        // Every iteration past seed exhaustion makes one `LlmClient::chat`
        // call. That signature is `-> Result<String>`: no usage, no cost, so
        // the spend can never reach `total_cost_usd`. On a billed backend
        // (`LLM_BASE_URL` / `LLM_API_KEY` / `MARC27_TOKEN`) that is real
        // money. "$3.25 spent of $25.00 ceiling" would read as headroom the
        // goal does not have, so the ceiling must name what it excludes.
        let config = CampaignConfig {
            budget_usd: Some(25.0),
            ..Default::default()
        };
        let mut state = CampaignState::new("c1".into(), test_goal(), config);
        state.current_iteration = 3;
        state.total_cost_usd = 3.25;
        state.uncosted_llm_calls = 3;

        assert_eq!(
            state.budget_status(),
            BudgetStatus::PartiallyMeasured {
                spent: 3.25,
                ceiling: 25.0,
                uncosted_llm_calls: 3
            }
        );
        let shown = state.budget_status().to_string();
        assert!(
            shown.contains("$3.2500 spent of $25.0000 ceiling"),
            "{shown}"
        );
        assert!(shown.contains("3 LLM proposal calls"), "{shown}");
        assert!(shown.contains("NOT in that figure"), "{shown}");
        assert!(shown.contains("cannot cover them"), "{shown}");
        // The summary the user reads carries it too.
        assert!(state.summary(&[]).contains("NOT in that figure"));

        // Nothing billed at all AND proposals ran: still unmeasured, but now
        // it says how many calls went unpriced instead of just "no step".
        state.total_cost_usd = 0.0;
        let shown = state.budget_status().to_string();
        assert!(shown.contains("CANNOT stop"), "{shown}");
        assert!(shown.contains("3 LLM proposal calls"), "{shown}");

        // Singular reads correctly.
        state.uncosted_llm_calls = 1;
        state.total_cost_usd = 1.0;
        assert!(
            state
                .budget_status()
                .to_string()
                .contains("1 LLM proposal call billed"),
            "{}",
            state.budget_status()
        );

        // A goal whose proposals never ran (all seeds) is fully measured.
        state.uncosted_llm_calls = 0;
        assert_eq!(
            state.budget_status(),
            BudgetStatus::Measured {
                spent: 1.0,
                ceiling: 25.0
            }
        );
    }

    #[test]
    fn uncosted_call_count_survives_a_pre_upgrade_checkpoint() {
        // Checkpoints written before the field existed must still load.
        let legacy = json!({
            "campaign_id": "c1",
            "goal": test_goal(),
            "config": CampaignConfig::default(),
            "candidates": [],
            "current_iteration": 2,
            "total_cost_usd": 0.0,
            "paused": false,
            "gates_hit": [],
            "completed": false,
            "completion_reason": "",
            "started_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        });
        let state: CampaignState = serde_json::from_value(legacy).expect("legacy checkpoint loads");
        assert_eq!(state.uncosted_llm_calls, 0);
    }

    #[test]
    fn no_ceiling_says_so() {
        let state = CampaignState::new("c1".into(), test_goal(), CampaignConfig::default());
        assert_eq!(state.budget_status(), BudgetStatus::NoCeiling);
    }

    #[test]
    fn reported_cost_reads_what_the_tool_said_and_never_invents() {
        assert_eq!(reported_cost(&json!({"cost_usd": 0.42})), 0.42);
        assert_eq!(reported_cost(&json!({"cost": 1.5})), 1.5);
        // Silence is 0.0 — surfaced by budget_status, never estimated here.
        assert_eq!(reported_cost(&json!({"density": 8.1})), 0.0);
        // A negative price is nonsense; clamp rather than credit the goal.
        assert_eq!(reported_cost(&json!({"cost_usd": -5.0})), 0.0);
    }

    #[test]
    fn research_outcome_cost_defaults_to_zero_on_legacy_checkpoints() {
        // Checkpoints written before cost_usd existed must still load.
        let legacy = json!({"summary": "s", "progress": 0.5});
        let outcome: ResearchIterationOutcome = serde_json::from_value(legacy).unwrap();
        assert_eq!(outcome.cost_usd, 0.0);
    }

    #[test]
    fn top_n_returns_best_first() {
        let mut state = CampaignState::new("c1".into(), test_goal(), CampaignConfig::default());
        state.candidates.push(Candidate {
            composition: "A".into(),
            properties: json!({}),
            reward: 0.3,
            iteration: 0,
            source: "llm".into(),
        });
        state.candidates.push(Candidate {
            composition: "B".into(),
            properties: json!({}),
            reward: 0.9,
            iteration: 1,
            source: "llm".into(),
        });
        state.candidates.push(Candidate {
            composition: "C".into(),
            properties: json!({}),
            reward: 0.5,
            iteration: 2,
            source: "llm".into(),
        });
        // Sort by reward descending
        state
            .candidates
            .sort_by(|a, b| b.reward.partial_cmp(&a.reward).unwrap());
        let top = state.top_n(2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].composition, "B");
        assert_eq!(top[1].composition, "C");
    }

    #[test]
    fn avg_reward_empty_is_zero() {
        let state = CampaignState::new("c1".into(), test_goal(), CampaignConfig::default());
        assert_eq!(state.avg_reward(), 0.0);
    }

    #[test]
    fn avg_reward_non_empty() {
        let mut state = CampaignState::new("c1".into(), test_goal(), CampaignConfig::default());
        state.candidates.push(Candidate {
            composition: "A".into(),
            properties: json!({}),
            reward: 0.4,
            iteration: 0,
            source: "llm".into(),
        });
        state.candidates.push(Candidate {
            composition: "B".into(),
            properties: json!({}),
            reward: 0.8,
            iteration: 1,
            source: "llm".into(),
        });
        assert!((state.avg_reward() - 0.6).abs() < 1e-9);
    }

    #[test]
    fn parse_compositions_json_array() {
        let campaign = Campaign::new(test_goal(), CampaignConfig::default(), "c1".into());
        let parsed = campaign.parse_compositions("[\"W0.3 Mo0.2\", \"Ta0.5 Nb0.5\"]");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], "W0.3 Mo0.2");
    }

    #[tokio::test]
    async fn proposal_without_chat_target_fails_with_actionable_error() {
        let _env_lock = ENV_LOCK.lock().await;
        let tmp = tempfile::tempdir().unwrap();
        let missing_config = tmp.path().join("missing-config.toml");
        let _config = EnvGuard::set("PRISM_CONFIG_PATH", missing_config.into_os_string());
        let _api_base = EnvGuard::remove("LLM_API_BASE");
        let _base_url = EnvGuard::remove("LLM_BASE_URL");

        let mut goal = test_goal();
        goal.seeds.clear();
        let mut campaign = Campaign::new(goal, CampaignConfig::default(), "no-target".into());
        let error = campaign
            .propose_candidates()
            .await
            .expect_err("an unresolved chat target must halt the campaign");
        let message = format!("{error:#}");

        assert!(message.contains("prism use marc27"), "error: {message}");
        assert!(message.contains("prism use local"), "error: {message}");
        assert!(!message.contains("127.0.0.1:8081"), "error: {message}");
    }

    #[tokio::test]
    async fn llm_api_base_override_wins_over_campaign_endpoint() {
        let _env_lock = ENV_LOCK.lock().await;
        let app = axum::Router::new()
            .route("/v1/chat/completions", post(configured_target_chat))
            .with_state(Arc::new(Mutex::new(None)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let tmp = tempfile::tempdir().unwrap();
        let missing_config = tmp.path().join("missing-config.toml");
        let _config = EnvGuard::set("PRISM_CONFIG_PATH", missing_config.into_os_string());
        let _api_base = EnvGuard::set("LLM_API_BASE", format!("{base_url}/v1"));
        let _base_url = EnvGuard::remove("LLM_BASE_URL");
        let mut goal = test_goal();
        goal.seeds.clear();
        let mut campaign = Campaign::new(
            goal,
            CampaignConfig {
                llm_base_url: Some("http://127.0.0.1:1/v1".into()),
                ..Default::default()
            },
            "api-base-override".into(),
        );

        let proposals = campaign
            .propose_candidates()
            .await
            .expect("LLM_API_BASE should override the campaign endpoint");

        assert_eq!(proposals, vec!["W0.5 Mo0.5"]);
        server.abort();
    }

    #[tokio::test]
    async fn proposal_uses_configured_chat_target_and_model() {
        let _env_lock = ENV_LOCK.lock().await;
        let captured_model = Arc::new(Mutex::new(None));
        let app = axum::Router::new()
            .route("/v1/chat/completions", post(configured_target_chat))
            .with_state(captured_model.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!(
                "[chat]\nmode = \"local\"\nurl = \"{base_url}/v1\"\nmodel = \"target-model\"\n"
            ),
        )
        .unwrap();
        let _config = EnvGuard::set("PRISM_CONFIG_PATH", config_path.into_os_string());
        let _api_base = EnvGuard::remove("LLM_API_BASE");
        let _base_url = EnvGuard::remove("LLM_BASE_URL");

        let mut goal = test_goal();
        goal.seeds.clear();
        let mut campaign = Campaign::new(
            goal,
            CampaignConfig {
                checkpoint_dir: Some(tmp.path().to_path_buf()),
                ..Default::default()
            },
            "configured-target".into(),
        );
        let proposals = campaign
            .propose_candidates()
            .await
            .expect("configured target should serve proposals");

        assert_eq!(proposals, vec!["W0.5 Mo0.5"]);
        assert_eq!(
            captured_model.lock().unwrap().as_deref(),
            Some("target-model")
        );
        server.abort();
    }

    #[test]
    fn parse_compositions_json_embedded_in_text() {
        let campaign = Campaign::new(test_goal(), CampaignConfig::default(), "c1".into());
        let text = "Here are my suggestions:\n[\"Ti0.8 Al0.2\", \"Ti0.7 V0.3\"]\nGood luck!";
        let parsed = campaign.parse_compositions(text);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], "Ti0.8 Al0.2");
    }

    #[test]
    fn parse_compositions_line_by_line() {
        let campaign = Campaign::new(test_goal(), CampaignConfig::default(), "c1".into());
        let text = "Ti0.9 Al0.06 V0.04\nTi0.8 Al0.1 Mo0.1\nCr0.4 V0.3 Ti0.3";
        let parsed = campaign.parse_compositions(text);
        assert_eq!(parsed.len(), 3);
    }

    #[test]
    fn parse_compositions_empty_text() {
        let campaign = Campaign::new(test_goal(), CampaignConfig::default(), "c1".into());
        assert!(campaign.parse_compositions("").is_empty());
    }

    #[test]
    fn compute_reward_default_heuristic() {
        let campaign = Campaign::new(test_goal(), CampaignConfig::default(), "c1".into());
        let props = json!({
            "mixing_entropy": 1.5,
            "density": 4.5,
        });
        let reward = campaign.compute_reward(&props).unwrap();
        // entropy_score = 1.5/2.0 = 0.75, density_score = 1 - 4.5/20 = 0.775
        // reward = 0.75*0.6 + 0.775*0.4 = 0.45 + 0.31 = 0.76
        assert!(reward > 0.0 && reward < 1.0);
    }

    #[test]
    fn compute_reward_with_weights() {
        let mut config = CampaignConfig::default();
        config.reward_weights.insert("density".into(), -1.0);
        config.reward_weights.insert("mixing_entropy".into(), 2.0);
        let campaign = Campaign::new(test_goal(), config, "c1".into());
        let props = json!({
            "mixing_entropy": 1.0,
            "density": 5.0,
        });
        let reward = campaign.compute_reward(&props).unwrap();
        // reward = 1.0*2.0 + 5.0*(-1.0) = 2.0 - 5.0 = -3.0
        assert!((reward - (-3.0)).abs() < 1e-9);
    }

    #[test]
    fn melting_point_objective_uses_evaluated_descriptor() {
        let mut goal = test_goal();
        goal.objective = "maximize melting point".into();
        let campaign = Campaign::new(goal, CampaignConfig::default(), "c1".into());

        let reward = campaign
            .compute_reward(&json!({"Tm_estimate_K": 3123.4}))
            .unwrap();

        assert_eq!(reward, 3123.4);
    }

    #[test]
    fn missing_reward_descriptors_halt_evaluation() {
        let campaign = Campaign::new(test_goal(), CampaignConfig::default(), "c1".into());

        let error = campaign
            .compute_reward(&json!({"error": "unknown tool"}))
            .expect_err("missing descriptors must not produce a fallback reward");

        assert!(error.to_string().contains("no numeric descriptors"));
    }

    #[test]
    fn build_proposal_prompt_includes_goal() {
        let campaign = Campaign::new(test_goal(), CampaignConfig::default(), "c1".into());
        let prompt = campaign.build_proposal_prompt(5);
        assert!(prompt.contains("High-strength Ti alloy"));
        assert!(prompt.contains("maximize strength-to-weight"));
        assert!(prompt.contains("Ti"));
    }

    #[test]
    fn build_proposal_prompt_includes_best_candidates() {
        let mut state = CampaignState::new("c1".into(), test_goal(), CampaignConfig::default());
        state.current_iteration = 3;
        state.candidates.push(Candidate {
            composition: "Ti0.8 Al0.2".into(),
            properties: json!({}),
            reward: 0.85,
            iteration: 2,
            source: "llm".into(),
        });
        // Need to construct a Campaign with this state — use checkpoint roundtrip
        let _config = CampaignConfig::default();
        let _checkpoint_dir = std::env::temp_dir().join("prism_campaign_test");
        let campaign = Campaign {
            state,
            provenance: None,
            checkpoint_path: std::env::temp_dir().join("prism_campaign_test/c1.json"),
        };
        let prompt = campaign.build_proposal_prompt(3);
        assert!(prompt.contains("Ti0.8 Al0.2"));
        assert!(prompt.contains("0.85"));
        assert!(prompt.contains("NEW"));
    }

    #[test]
    fn build_summary_contains_key_info() {
        let mut state = CampaignState::new("c1".into(), test_goal(), CampaignConfig::default());
        state.completed = true;
        state.status = GoalStatus::Completed;
        state.completion_reason = "iteration_limit".into();
        state.current_iteration = 50;
        state.candidates.push(Candidate {
            composition: "Ti0.8 Al0.2".into(),
            properties: json!({}),
            reward: 0.9,
            iteration: 45,
            source: "llm".into(),
        });
        let winners = state.top_n(10).to_vec();
        let summary = state.summary(&winners);
        assert!(summary.contains("c1"));
        assert!(summary.contains("completed (iteration_limit)"));
        assert!(summary.contains("Ti0.8 Al0.2"));
        assert!(summary.contains("50"));
    }

    #[test]
    fn checkpoint_roundtrip() {
        let temp = std::env::temp_dir().join("prism_campaign_checkpoint_test");
        std::fs::create_dir_all(&temp).unwrap();
        let path = temp.join("test_campaign.json");

        let mut campaign = Campaign::new(test_goal(), CampaignConfig::default(), "test-cp".into());
        campaign.checkpoint_path = path.clone();
        campaign.state.current_iteration = 5;
        campaign.state.candidates.push(Candidate {
            composition: "Ti0.9 Al0.1".into(),
            properties: json!({"density": 4.0}),
            reward: 0.7,
            iteration: 3,
            source: "llm".into(),
        });
        campaign.checkpoint().unwrap();

        let resumed = Campaign::from_checkpoint(&path).unwrap();
        assert_eq!(resumed.state.campaign_id, "test-cp");
        assert_eq!(resumed.state.current_iteration, 5);
        assert_eq!(resumed.state.candidates.len(), 1);
        assert_eq!(resumed.state.candidates[0].composition, "Ti0.9 Al0.1");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn config_default_sensible_values() {
        let config = CampaignConfig::default();
        assert_eq!(config.max_iterations, 50);
        assert_eq!(config.batch_size, 10);
        assert_eq!(config.checkpoint_every, 10);
        assert!(config.budget_usd.is_none());
    }

    // ── Research-campaign generalization tests ──────────────────────────

    #[test]
    fn research_goal_serde_roundtrip() {
        // A research goal must survive checkpoint serialization (resume).
        let goal = ResearchCampaignGoal {
            objective: "Survey refractory HEAs for 1200C turbine blades".into(),
            constraints: vec!["density < 12 g/cm^3".into()],
            success_criteria: vec!["cited report with >=3 corroborated claims".into()],
        };
        let json = serde_json::to_string(&goal).expect("serialize");
        let back: ResearchCampaignGoal = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(goal, back);
    }

    #[test]
    fn research_goal_defaults_empty_optionals() {
        // A minimal research goal (objective only) deserializes with empty
        // optional fields — the common case for a quick research task.
        let json = r#"{"objective":"quick lookup"}"#;
        let goal: ResearchCampaignGoal =
            serde_json::from_str(json).expect("deserialize minimal goal");
        assert_eq!(goal.objective, "quick lookup");
        assert!(goal.constraints.is_empty());
        assert!(goal.success_criteria.is_empty());
    }

    #[test]
    fn campaign_goal_kind_defaults_to_materials() {
        // Backward compat: unspecified kind is the legacy materials path, so
        // existing checkpoints and the shipping binary are unaffected.
        assert_eq!(CampaignGoalKind::default(), CampaignGoalKind::Materials);
    }

    #[test]
    fn goal_kind_serde_roundtrip_preserves_discriminator() {
        for kind in [CampaignGoalKind::Materials, CampaignGoalKind::Research] {
            let json = serde_json::to_string(&kind).expect("serialize");
            let back: CampaignGoalKind = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn research_executor_trait_object_can_be_invoked() {
        // Verify the executor seam compiles and is object-safe: a stub
        // executor implementing the trait can be boxed and awaited. The agent
        // layer will implement this for real (driving run_turn per iteration).
        struct StubExecutor;
        impl ResearchIterationExecutor for StubExecutor {
            fn execute_iteration<'a>(
                &'a self,
                _goal: &'a ResearchCampaignGoal,
                ctx: &'a ResearchIterationContext,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<ResearchIterationOutcome>> + Send + 'a>,
            > {
                Box::pin(async move {
                    Ok(ResearchIterationOutcome {
                        summary: format!("stub iteration {}", ctx.iteration),
                        artifact_refs: Vec::new(),
                        progress: 0.5,
                        notes: Vec::new(),
                        cost_usd: 0.0,
                    })
                })
            }
        }

        let exec: Box<dyn ResearchIterationExecutor> = Box::new(StubExecutor);
        let goal = ResearchCampaignGoal {
            objective: "test".into(),
            ..Default::default()
        };
        let ctx = ResearchIterationContext {
            iteration: 0,
            ..Default::default()
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let outcome = rt
            .block_on(exec.execute_iteration(&goal, &ctx))
            .expect("stub iteration succeeds");
        assert_eq!(outcome.summary, "stub iteration 0");
        assert!((0.0..=1.0).contains(&outcome.progress));
    }

    // ── run_research: the full research-campaign loop ───────────────────

    /// An executor that reports partial progress for the first N iterations
    /// then signals completion — simulating a research task that gathers
    /// findings over several turns then declares success.
    struct PhasedExecutor {
        complete_at: usize,
        /// USD this executor reports per iteration — the seam the real
        /// budget ceiling is enforced through.
        cost_per_iteration: f64,
    }
    impl ResearchIterationExecutor for PhasedExecutor {
        fn execute_iteration<'a>(
            &'a self,
            _goal: &'a ResearchCampaignGoal,
            ctx: &'a ResearchIterationContext,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<ResearchIterationOutcome>> + Send + 'a>,
        > {
            Box::pin(async move {
                let iter = ctx.iteration;
                let done = iter + 1 >= self.complete_at;
                Ok(ResearchIterationOutcome {
                    summary: format!("step {} findings", iter + 1),
                    artifact_refs: vec![format!("prov:step{iter}")],
                    progress: if done { 1.0 } else { 0.3 },
                    notes: vec![format!("note from step {}", iter + 1)],
                    cost_usd: self.cost_per_iteration,
                })
            })
        }
    }

    fn research_goal_for_run() -> ResearchCampaignGoal {
        ResearchCampaignGoal {
            objective: "Survey refractory HEAs for turbines".into(),
            constraints: vec![],
            success_criteria: vec!["cited report".into()],
        }
    }

    #[test]
    fn run_research_completes_when_executor_signals_success() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let mut config = CampaignConfig {
            max_iterations: 10,
            checkpoint_every: 0, // no checkpoint files during the test
            ..Default::default()
        };
        config.checkpoint_dir = Some(temp.path().parent().unwrap().to_path_buf());
        let mut campaign =
            Campaign::new_research(research_goal_for_run(), config, "test-research-1".into());
        // Completes at iteration 3 (0-indexed: iters 0,1,2,3 where 3 is done).
        let executor = PhasedExecutor {
            complete_at: 3,
            cost_per_iteration: 0.0,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt
            .block_on(campaign.run_research(&executor))
            .expect("research campaign runs");

        assert!(result.state.completed);
        assert_eq!(result.state.completion_reason, "success_criteria_met");
        assert_eq!(result.state.research_outcomes.len(), 3);
        // No materials candidates in a research campaign.
        assert!(result.winners.is_empty());
        assert!(result.summary.contains("success criteria met"));
        // Artifact refs accumulated across iterations.
        assert!(result.summary.contains("step"));
    }

    #[test]
    fn run_research_hits_iteration_cap_without_completion() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let mut config = CampaignConfig {
            max_iterations: 2,
            checkpoint_every: 0,
            ..Default::default()
        };
        config.checkpoint_dir = Some(temp.path().parent().unwrap().to_path_buf());
        let mut campaign =
            Campaign::new_research(research_goal_for_run(), config, "test-research-cap".into());
        // Never completes — progress stays at 0.3 forever.
        let executor = PhasedExecutor {
            complete_at: 100,
            cost_per_iteration: 0.0,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt
            .block_on(campaign.run_research(&executor))
            .expect("runs to cap");

        assert!(result.state.completed);
        assert_eq!(result.state.completion_reason, "iteration_limit");
        assert_eq!(result.state.research_outcomes.len(), 2);
    }

    #[test]
    fn run_research_records_its_approval_gate_like_the_materials_loop() {
        // The gate used to set only the legacy `paused` flag: the checkpoint
        // said `status: submitted` while the goal sat waiting for a human, and
        // `gates_hit` stayed empty so a resume would re-pause at the same gate
        // forever.
        let temp = tempfile::NamedTempFile::new().unwrap();
        let mut config = CampaignConfig {
            max_iterations: 100,
            checkpoint_every: 0,
            approval_gate_at: vec![2],
            ..Default::default()
        };
        config.checkpoint_dir = Some(temp.path().parent().unwrap().to_path_buf());
        let mut campaign =
            Campaign::new_research(research_goal_for_run(), config, "test-research-gate".into());
        let executor = PhasedExecutor {
            complete_at: 100,
            cost_per_iteration: 0.0,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt
            .block_on(campaign.run_research(&executor))
            .expect("pauses at the gate");

        assert_eq!(result.state.status, GoalStatus::Paused);
        assert!(result.state.paused);
        assert!(!result.state.completed);
        assert_eq!(result.state.current_iteration, 2);
        assert_eq!(
            result.state.gates_hit,
            vec![2],
            "the gate must be recorded so a resume does not re-pause forever"
        );
    }

    #[test]
    fn run_research_stops_at_the_budget_ceiling_and_says_why() {
        // Before executor-reported cost existed, `total_cost_usd` was never
        // written and this ceiling could not fire at any spend — the goal ran
        // to its iteration cap regardless of `budget_usd`.
        let temp = tempfile::NamedTempFile::new().unwrap();
        let mut config = CampaignConfig {
            max_iterations: 100,
            checkpoint_every: 0,
            budget_usd: Some(2.5),
            ..Default::default()
        };
        config.checkpoint_dir = Some(temp.path().parent().unwrap().to_path_buf());
        let mut campaign = Campaign::new_research(
            research_goal_for_run(),
            config,
            "test-research-budget".into(),
        );
        // Never finishes on its own; bills $1 per iteration.
        let executor = PhasedExecutor {
            complete_at: 100,
            cost_per_iteration: 1.0,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt
            .block_on(campaign.run_research(&executor))
            .expect("runs to the ceiling");

        assert_eq!(
            result.state.completion_reason, "budget_exhausted",
            "the ceiling must stop the loop, not the iteration cap"
        );
        // Stopped as soon as spend reached the ceiling — 3 iterations to go
        // from $0 to $3, checked at the top of the 4th.
        assert_eq!(result.state.research_outcomes.len(), 3);
        assert_eq!(result.state.total_cost_usd, 3.0);
        assert!(
            result.state.current_iteration < 100,
            "must not run to the iteration cap"
        );
        assert_eq!(
            result.state.budget_status(),
            BudgetStatus::Measured {
                spent: 3.0,
                ceiling: 2.5
            }
        );
    }

    #[test]
    fn run_research_carries_prior_artifacts_into_later_iterations() {
        // The accumulated_research_state helper must fold prior artifact refs
        // + notes forward so iteration N sees what iterations 0..N produced.
        let temp = tempfile::NamedTempFile::new().unwrap();
        let mut config = CampaignConfig {
            max_iterations: 3,
            checkpoint_every: 0,
            ..Default::default()
        };
        config.checkpoint_dir = Some(temp.path().parent().unwrap().to_path_buf());
        let mut campaign =
            Campaign::new_research(research_goal_for_run(), config, "test-research-ctx".into());

        // An executor that asserts it sees accumulated prior state.
        struct AssertingExecutor;
        impl ResearchIterationExecutor for AssertingExecutor {
            fn execute_iteration<'a>(
                &'a self,
                _goal: &'a ResearchCampaignGoal,
                ctx: &'a ResearchIterationContext,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<ResearchIterationOutcome>> + Send + 'a>,
            > {
                Box::pin(async move {
                    // Iteration N should see N prior artifact refs + N prior notes.
                    assert_eq!(
                        ctx.artifact_refs.len(),
                        ctx.iteration,
                        "iter {} should have {} prior artifacts",
                        ctx.iteration,
                        ctx.iteration
                    );
                    assert_eq!(ctx.notes.len(), ctx.iteration);
                    Ok(ResearchIterationOutcome {
                        summary: format!("iter {}", ctx.iteration),
                        artifact_refs: vec![format!("prov:i{}", ctx.iteration)],
                        progress: if ctx.iteration >= 2 { 1.0 } else { 0.2 },
                        notes: vec![format!("note {}", ctx.iteration)],
                        cost_usd: 0.0,
                    })
                })
            }
        }
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt
            .block_on(campaign.run_research(&AssertingExecutor))
            .expect("runs");
        assert!(result.state.completed);
        assert_eq!(result.state.completion_reason, "success_criteria_met");
    }

    #[test]
    fn research_checkpoint_roundtrip_preserves_outcomes() {
        // A research campaign's checkpoint must survive save/load (resume).
        let temp = tempfile::NamedTempFile::new().unwrap();
        let mut config = CampaignConfig {
            max_iterations: 5,
            checkpoint_every: 0,
            ..Default::default()
        };
        let dir = temp.path().parent().unwrap().to_path_buf();
        config.checkpoint_dir = Some(dir.clone());
        let mut campaign =
            Campaign::new_research(research_goal_for_run(), config, "test-research-cp".into());
        let executor = PhasedExecutor {
            complete_at: 2,
            cost_per_iteration: 0.0,
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _ = rt.block_on(campaign.run_research(&executor)).expect("runs");

        let cp_path = dir.join("test-research-cp.json");
        let resumed = Campaign::from_checkpoint(&cp_path).expect("checkpoint reloads");
        assert_eq!(resumed.state.kind, CampaignGoalKind::Research);
        assert!(resumed.state.research_goal.is_some());
        assert_eq!(resumed.state.research_outcomes.len(), 2);
        assert!(resumed.state.completed);
        std::fs::remove_file(&cp_path).ok();
    }
}
