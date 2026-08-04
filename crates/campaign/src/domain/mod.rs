//! Domain plugins for the discovery loop.
//!
//! The campaign engine owns iteration, persistence, budgets, provenance, and
//! ranking order. A [`Domain`] owns candidate identity, scientific constraints,
//! evaluator selection, scalarization, and proposal guidance.

pub(crate) mod alloy;
pub(crate) mod polymer;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{CampaignConfig, CampaignGoal};

/// Built-in discovery domains. Existing checkpoints omit this field and
/// therefore continue as alloy campaigns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainKind {
    #[default]
    Alloy,
    Polymer,
}

impl DomainKind {
    pub(crate) fn is_alloy(&self) -> bool {
        *self == Self::Alloy
    }
}

/// Comparison used by a user-configured, named property constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintOperator {
    AtLeast,
    AtMost,
}

impl ConstraintOperator {
    pub(crate) fn symbol(self) -> &'static str {
        match self {
            Self::AtLeast => ">=",
            Self::AtMost => "<=",
        }
    }

    fn violated_by(self, value: f64, threshold: f64) -> bool {
        match self {
            Self::AtLeast => value < threshold,
            Self::AtMost => value > threshold,
        }
    }
}

/// A hard property constraint whose policy name and source are persisted and
/// reported. Domains decide which property vocabulary is valid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyConstraint {
    pub definition: String,
    pub property: String,
    pub operator: ConstraintOperator,
    pub threshold: f64,
    pub unit: String,
    pub citation: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCandidate {
    pub original: String,
    pub canonical: String,
}

/// One evaluator rung a domain permits. Selection belongs to the domain even
/// when a domain currently has only one scientifically defensible rung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EvaluatorTier {
    pub tier: u8,
    pub name: &'static str,
    pub method: &'static str,
    pub citation: &'static str,
}

/// Scientific behavior required by the otherwise domain-neutral campaign
/// loop. Implementations must reject missing evidence rather than synthesize
/// candidate identities or properties.
pub trait Domain: Send + Sync {
    fn name(&self) -> &'static str;
    fn candidate_plural(&self) -> &'static str;

    fn validate_goal(&self, goal: &CampaignGoal) -> std::result::Result<(), String>;
    fn apply_goal_implied_constraints(&self, config: &mut CampaignConfig, goal: &CampaignGoal);
    fn parse_candidate(
        &self,
        candidate: &str,
        goal: &CampaignGoal,
    ) -> std::result::Result<ParsedCandidate, String>;

    fn definition(&self, config: &CampaignConfig) -> Option<serde_json::Value>;
    fn definition_label(&self) -> &'static str;
    fn configured_constraints(&self, config: &CampaignConfig) -> Vec<String>;
    fn constraint_violations(
        &self,
        parsed: &ParsedCandidate,
        properties: &serde_json::Value,
        config: &CampaignConfig,
    ) -> Vec<String>;

    fn evaluator_tool(&self) -> &'static str;
    fn evaluator_tiers(&self) -> &'static [EvaluatorTier];
    fn evaluator_inputs(&self, parsed: &ParsedCandidate) -> serde_json::Value;
    fn dependency_install_hint(&self) -> Option<&'static str> {
        None
    }

    fn decorate_properties(
        &self,
        properties: &mut serde_json::Value,
        parsed: &ParsedCandidate,
        config: &CampaignConfig,
    ) -> Result<()>;
    fn compute_reward(
        &self,
        goal: &CampaignGoal,
        config: &CampaignConfig,
        properties: &serde_json::Value,
    ) -> Result<f64>;
    fn summarize_properties(&self, properties: &serde_json::Value) -> String;

    fn proposal_system_prompt(&self) -> &'static str;
    fn search_space_prompt(&self, goal: &CampaignGoal) -> String;
    fn improvement_prompt(&self, batch: usize) -> String;
    fn initial_prompt(&self, batch: usize) -> String;

    fn validate_tier(&self, config: &CampaignConfig) -> Result<u8> {
        let selected = config.evaluation_tier.unwrap_or_else(|| {
            self.evaluator_tiers()
                .first()
                .map(|tier| tier.tier)
                .unwrap_or(0)
        });
        if self
            .evaluator_tiers()
            .iter()
            .any(|tier| tier.tier == selected)
        {
            Ok(selected)
        } else {
            let available = self
                .evaluator_tiers()
                .iter()
                .map(|tier| tier.tier.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!(
                "domain '{}' does not support evaluator tier {selected}; available tiers: [{available}]",
                self.name()
            )
        }
    }
}

/// Resolve a stable built-in plugin for campaign construction, inspection, or
/// testing. Checkpoints persist [`DomainKind`], so resume selects identically.
pub fn builtin_domain(kind: DomainKind) -> &'static dyn Domain {
    match kind {
        DomainKind::Alloy => &alloy::ALLOY_DOMAIN,
        DomainKind::Polymer => &polymer::POLYMER_DOMAIN,
    }
}

pub(crate) fn structured_constraint_descriptions(
    constraints: &[PropertyConstraint],
) -> Vec<String> {
    constraints
        .iter()
        .map(|constraint| {
            format!(
                "[{}] {} {} {:.4} {} (citation: {})",
                constraint.definition,
                constraint.property,
                constraint.operator.symbol(),
                constraint.threshold,
                constraint.unit,
                constraint.citation
            )
        })
        .collect()
}

pub(crate) fn structured_constraint_violations(
    constraints: &[PropertyConstraint],
    properties: &serde_json::Value,
    evaluator_tool: &str,
) -> Vec<String> {
    let mut violations = Vec::new();
    for constraint in constraints {
        if constraint.definition.trim().is_empty() || constraint.citation.trim().is_empty() {
            violations.push(format!(
                "property constraint for '{}' is missing its named definition or citation",
                constraint.property
            ));
            continue;
        }
        if !constraint.threshold.is_finite() {
            violations.push(format!(
                "[{}] threshold for '{}' must be finite (citation: {})",
                constraint.definition, constraint.property, constraint.citation
            ));
            continue;
        }
        match properties
            .get(&constraint.property)
            .and_then(serde_json::Value::as_f64)
        {
            Some(value) if constraint.operator.violated_by(value, constraint.threshold) => {
                violations.push(format!(
                    "[{}] {}={value:.4} {} hard limit {:.4} {} (citation: {})",
                    constraint.definition,
                    constraint.property,
                    match constraint.operator {
                        ConstraintOperator::AtLeast => "is below",
                        ConstraintOperator::AtMost => "is above",
                    },
                    constraint.threshold,
                    constraint.unit,
                    constraint.citation
                ));
            }
            Some(_) => {}
            None => violations.push(format!(
                "[{}] {evaluator_tool} returned no numeric '{}' required by hard constraint (citation: {})",
                constraint.definition, constraint.property, constraint.citation
            )),
        }
    }
    violations
}
