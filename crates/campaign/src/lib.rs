//! PRISM Campaign Engine
//!
//! Long-running autonomous materials discovery campaigns.
//!
//! A campaign is a budget-limited, checkpointable loop that:
//! 1. Proposes candidate materials (via LLM, MCMC, or seed data)
//! 2. Evaluates each candidate (via tools: evaluate_material, predict, DFT)
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
//!     checkpoint_dir: None,
//!     llm_model: String::new(),
//!     llm_temperature: 0.7,
//!     reward_weights: std::collections::BTreeMap::new(),
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
        }
    }
}

// ── Campaign State ──────────────────────────────────────────────────

/// A single evaluated candidate material.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    /// Composition string (e.g. "W0.3 Mo0.2 Ta0.3 Nb0.2").
    pub composition: String,
    /// Physics descriptors from evaluate_material or similar.
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
    /// The goal being pursued.
    pub goal: CampaignGoal,
    /// The config (immutable for a given campaign).
    pub config: CampaignConfig,
    /// All candidates evaluated so far, ranked by reward (best first).
    pub candidates: Vec<Candidate>,
    /// Current iteration number (0-based).
    pub current_iteration: usize,
    /// Cumulative compute cost in USD.
    pub total_cost_usd: f64,
    /// Whether the campaign is paused at an approval gate.
    pub paused: bool,
    /// Whether the campaign has completed (success, budget, or iteration cap).
    pub completed: bool,
    /// Why the campaign completed (if it did).
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
            candidates: Vec::new(),
            current_iteration: 0,
            total_cost_usd: 0.0,
            paused: false,
            completed: false,
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

    /// Average reward across all candidates.
    pub fn avg_reward(&self) -> f64 {
        if self.candidates.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.candidates.iter().map(|c| c.reward).sum();
        sum / self.candidates.len() as f64
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

// ── Campaign Engine ─────────────────────────────────────────────────

/// The campaign orchestrator.
pub struct Campaign {
    state: CampaignState,
    provenance: Option<ProvenanceStore>,
    checkpoint_path: PathBuf,
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

    /// Resume a campaign from a checkpoint file.
    pub fn from_checkpoint(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read campaign checkpoint: {}", path.display()))?;
        let state: CampaignState = serde_json::from_str(&text)
            .context("failed to parse campaign checkpoint (version mismatch?)")?;
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
        info!(
            campaign = %self.state.campaign_id,
            goal = %self.state.goal.description,
            max_iterations = self.state.config.max_iterations,
            "campaign started"
        );

        self.record_event(
            "campaign.start",
            serde_json::json!({
                "goal": self.state.goal,
                "config": self.state.config,
            }),
        )
        .await;

        while !self.state.completed && !self.state.paused {
            // Check iteration cap
            if self.state.current_iteration >= self.state.config.max_iterations {
                self.state.completed = true;
                self.state.completion_reason = "iteration_limit".into();
                info!(campaign = %self.state.campaign_id, "campaign hit iteration limit");
                break;
            }

            // Check budget
            if let Some(budget) = self.state.config.budget_usd
                && self.state.total_cost_usd >= budget
            {
                self.state.completed = true;
                self.state.completion_reason = "budget_exhausted".into();
                info!(
                    campaign = %self.state.campaign_id,
                    spent = self.state.total_cost_usd,
                    budget,
                    "campaign hit budget limit"
                );
                break;
            }

            // Check approval gate
            let iter = self.state.current_iteration;
            if self.state.config.approval_gate_at.contains(&iter) && iter > 0 {
                self.state.paused = true;
                info!(
                    campaign = %self.state.campaign_id,
                    iteration = iter,
                    "campaign paused at approval gate"
                );
                self.checkpoint()?;
                break;
            }

            // Run one iteration
            self.run_iteration().await?;

            // Checkpoint
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
                    "candidates": self.state.total_evaluated(),
                    "best_reward": self.state.best().map(|c| c.reward).unwrap_or(0.0),
                }),
            )
            .await;
        }

        // Final checkpoint
        self.checkpoint()?;

        // Build result
        let winners: Vec<Candidate> = self.state.top_n(10).to_vec();

        let summary = self.build_summary(&winners);

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

    /// Resume a paused campaign (after human approval at a gate).
    pub async fn resume(&mut self) -> Result<CampaignResult> {
        if !self.state.paused {
            bail!("campaign is not paused — nothing to resume");
        }
        self.state.paused = false;
        info!(
            campaign = %self.state.campaign_id,
            iteration = self.state.current_iteration,
            "campaign resumed from approval gate"
        );
        self.run().await
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
                }
            }
        }

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

        // Use the LLM to propose candidates.
        let base_url = std::env::var("LLM_BASE_URL")
            .or_else(|_| std::env::var("LLM_API_BASE"))
            .unwrap_or_else(|_| "http://127.0.0.1:8081/v1".to_string());
        let api_key = std::env::var("LLM_API_KEY")
            .or_else(|_| std::env::var("MARC27_TOKEN"))
            .ok();
        let model = if self.state.config.llm_model.is_empty() {
            std::env::var("LLM_MODEL").unwrap_or_else(|_| "gemma-4-12b".to_string())
        } else {
            self.state.config.llm_model.clone()
        };

        let config = prism_llm::LlmConfig {
            base_url,
            api_key,
            model: model.clone(),
            embedding_model: None,
            ..Default::default()
        };
        let client = prism_llm::LlmClient::new(config);

        let system = "You are a materials scientist designing novel alloys. \
                      Respond with ONLY a JSON array of composition strings, \
                      no explanation. Example: [\"W0.3 Mo0.2 Ta0.3 Nb0.2\", \"Cr0.4 V0.3 Ti0.3\"]";

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
            }),
        )
        .await;

        // Parse the response — expect a JSON array of composition strings.
        let compositions = self.parse_compositions(&response);

        if compositions.is_empty() {
            warn!(
                campaign = %self.state.campaign_id,
                iteration = iter,
                raw = %response,
                "LLM returned no parseable compositions; falling back to MCMC"
            );
            // Fall back to simple random generation if LLM fails.
            return Ok(self.fallback_proposals(batch));
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

    /// Fallback: generate simple placeholder compositions when the LLM fails.
    fn fallback_proposals(&self, batch: usize) -> Vec<String> {
        let elements: Vec<String> = if self.state.goal.elements.is_empty() {
            vec![
                "Fe".into(),
                "Ni".into(),
                "Cr".into(),
                "Co".into(),
                "Ti".into(),
            ]
        } else {
            self.state.goal.elements.clone()
        };

        (0..batch)
            .map(|i| {
                // Simple equal-fraction distribution, rotated by index.
                let n = elements.len().min(4);
                let offset = i % elements.len();
                elements
                    .iter()
                    .cycle()
                    .skip(offset)
                    .take(n)
                    .map(|el| format!("{}{:.1}", el, 1.0 / n as f64))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    /// Evaluate a single candidate composition.
    ///
    /// In the current implementation, this calls the `evaluate_material` tool
    /// via the local PRISM node API and computes a scalarized reward from
    /// the returned physics descriptors.
    async fn evaluate_candidate(&self, composition: &str, iteration: usize) -> Result<Candidate> {
        // Call the local PRISM node's evaluate_material tool.
        let port = std::env::var("PRISM_NODE_PORT").unwrap_or_else(|_| "7327".to_string());
        let url = format!("http://127.0.0.1:{port}/api/tools/evaluate_material/run");
        let body = serde_json::json!({
            "inputs": { "composition": composition },
        });

        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("failed to call evaluate_material")?;

        let status = resp.status();
        let resp_body: serde_json::Value = resp
            .json()
            .await
            .unwrap_or_else(|_| serde_json::json!({"error": "failed to parse response"}));

        if !status.is_success() {
            bail!("evaluate_material returned {}: {}", status, resp_body);
        }

        // Compute scalarized reward from the properties.
        let reward = self.compute_reward(&resp_body);

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
            source: if iteration == 0 {
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
    fn compute_reward(&self, props: &serde_json::Value) -> f64 {
        if self.state.config.reward_weights.is_empty() {
            // Default heuristic: reward high entropy, penalize high density.
            let entropy = props
                .get("mixing_entropy")
                .or_else(|| props.get("entropy"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let density = props.get("density").and_then(|v| v.as_f64()).unwrap_or(8.0);
            // Normalize: entropy typically 0-2 R, density 2-20 g/cm³.
            let entropy_score = entropy / 2.0;
            let density_score = 1.0 - (density / 20.0).clamp(0.0, 1.0);
            return entropy_score * 0.6 + density_score * 0.4;
        }

        // Weighted sum of named properties.
        let mut reward = 0.0;
        for (prop, weight) in &self.state.config.reward_weights {
            let value = props.get(prop).and_then(|v| v.as_f64()).unwrap_or(0.0);
            reward += value * weight;
        }
        reward
    }

    /// Save campaign state to a checkpoint file.
    fn checkpoint(&mut self) -> Result<()> {
        self.state.last_checkpoint_at = Utc::now().to_rfc3339();
        if let Some(parent) = self.checkpoint_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&self.state)?;
        std::fs::write(&self.checkpoint_path, text)?;
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

    /// Build a human-readable summary of the campaign results.
    fn build_summary(&self, winners: &[Candidate]) -> String {
        let mut s = String::new();
        s.push_str(&format!("Campaign: {}\n", self.state.campaign_id));
        s.push_str(&format!("Goal: {}\n", self.state.goal.description));
        s.push_str(&format!(
            "Status: {}",
            if self.state.completed {
                &self.state.completion_reason
            } else if self.state.paused {
                "paused (approval gate)"
            } else {
                "running"
            }
        ));
        s.push('\n');
        s.push_str(&format!(
            "Iterations: {} / {}\n",
            self.state.current_iteration, self.state.config.max_iterations
        ));
        s.push_str(&format!(
            "Candidates evaluated: {}\n",
            self.state.total_evaluated()
        ));
        s.push_str(&format!("Avg reward: {:.4}\n", self.state.avg_reward()));
        if let Some(best) = self.state.best() {
            s.push_str(&format!(
                "Best: {} (reward={:.4})\n",
                best.composition, best.reward
            ));
        }
        if !winners.is_empty() {
            s.push_str("\nTop candidates:\n");
            for (i, c) in winners.iter().enumerate() {
                s.push_str(&format!(
                    "  {}. {} — reward={:.4} (iter {}, {})\n",
                    i + 1,
                    c.composition,
                    c.reward,
                    c.iteration,
                    c.source
                ));
            }
        }
        s
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn fallback_proposals_uses_allowed_elements() {
        let campaign = Campaign::new(test_goal(), CampaignConfig::default(), "c1".into());
        let proposals = campaign.fallback_proposals(3);
        assert_eq!(proposals.len(), 3);
        // Each should contain at least one of the allowed elements
        for p in &proposals {
            assert!(p.contains("Ti") || p.contains("Al") || p.contains("V"));
        }
    }

    #[test]
    fn compute_reward_default_heuristic() {
        let campaign = Campaign::new(test_goal(), CampaignConfig::default(), "c1".into());
        let props = json!({
            "mixing_entropy": 1.5,
            "density": 4.5,
        });
        let reward = campaign.compute_reward(&props);
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
        let reward = campaign.compute_reward(&props);
        // reward = 1.0*2.0 + 5.0*(-1.0) = 2.0 - 5.0 = -3.0
        assert!((reward - (-3.0)).abs() < 1e-9);
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
        state.completion_reason = "iteration_limit".into();
        state.current_iteration = 50;
        state.candidates.push(Candidate {
            composition: "Ti0.8 Al0.2".into(),
            properties: json!({}),
            reward: 0.9,
            iteration: 45,
            source: "llm".into(),
        });
        let campaign = Campaign {
            state,
            provenance: None,
            checkpoint_path: std::env::temp_dir().join("prism_campaign_test/c1.json"),
        };
        let winners = campaign.state.top_n(10).to_vec();
        let summary = campaign.build_summary(&winners);
        assert!(summary.contains("c1"));
        assert!(summary.contains("iteration_limit"));
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
}
