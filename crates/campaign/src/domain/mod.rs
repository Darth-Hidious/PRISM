//! Domain plugins for the discovery loop.
//!
//! The campaign engine owns iteration, persistence, budgets, provenance, and
//! ranking order. A [`Domain`] owns candidate identity, scientific constraints,
//! evaluator selection, scalarization, and proposal guidance.

pub(crate) mod alloy;
pub(crate) mod polymer;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{CampaignConfig, CampaignGoal};

/// Stable id of the built-in alloy domain — the default `CampaignConfig::domain`
/// value and the id legacy checkpoints carry (or omit, which also means this).
pub const ALLOY_DOMAIN_ID: &str = "alloy";
/// Stable id of the built-in polymer electrical-insulation domain.
pub const POLYMER_DOMAIN_ID: &str = "polymer";

pub(crate) fn default_domain_id() -> String {
    ALLOY_DOMAIN_ID.to_string()
}

pub(crate) fn is_default_domain_id(domain: &str) -> bool {
    domain == ALLOY_DOMAIN_ID
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

// ── Registry ──────────────────────────────────────────────────────────────

/// Ordered registry of domain plugins — the ONE place a domain id resolves
/// to its plugin, mirroring `prism_ingest::ontologies::OntologyRegistry`.
/// Iteration order is registration order, which keeps error listings
/// deterministic.
pub struct DomainRegistry {
    domains: Vec<&'static dyn Domain>,
    ids: Vec<&'static str>,
    by_id: std::collections::HashMap<&'static str, usize>,
}

impl DomainRegistry {
    pub fn new() -> Self {
        Self {
            domains: Vec::new(),
            ids: Vec::new(),
            by_id: std::collections::HashMap::new(),
        }
    }

    /// The built-in domains, in canonical order.
    pub fn builtin() -> Self {
        let mut reg = Self::new();
        reg.register(&alloy::ALLOY_DOMAIN)
            .expect("built-in domain declarations are valid and unique");
        reg.register(&polymer::POLYMER_DOMAIN)
            .expect("built-in domain declarations are valid and unique");
        reg
    }

    /// Add a domain under a FREE id. A taken id is a loud refusal — taking
    /// over a registered domain is [`Self::replace`]. On refusal nothing
    /// changes.
    pub fn register(&mut self, domain: &'static dyn Domain) -> Result<()> {
        let id = validated_domain_id(domain)?;
        if self.by_id.contains_key(id) {
            bail!(
                "domain id '{id}' is already registered; swap it deliberately with \
                 DomainRegistry::replace (replace_domain for the process-wide registry)"
            );
        }
        self.by_id.insert(id, self.domains.len());
        self.ids.push(id);
        self.domains.push(domain);
        Ok(())
    }

    /// Deliberately swap the domain registered under the SAME id. The id
    /// must be taken; returns the displaced domain.
    pub fn replace(&mut self, domain: &'static dyn Domain) -> Result<&'static dyn Domain> {
        let id = validated_domain_id(domain)?;
        let Some(&index) = self.by_id.get(id) else {
            bail!(
                "no domain '{id}' registered to replace; add it with \
                 DomainRegistry::register (register_domain for the process-wide registry)"
            );
        };
        Ok(std::mem::replace(&mut self.domains[index], domain))
    }

    /// Look up a domain by its stable id.
    pub fn get(&self, id: &str) -> Option<&'static dyn Domain> {
        self.by_id.get(id).map(|&index| self.domains[index])
    }

    /// All registered ids, in registration order.
    pub fn ids(&self) -> Vec<&'static str> {
        self.ids.clone()
    }
}

impl Default for DomainRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

fn validated_domain_id(domain: &'static dyn Domain) -> Result<&'static str> {
    let id = domain.name();
    if id.trim().is_empty() {
        bail!("a domain plugin must declare a non-empty id (name)");
    }
    Ok(id)
}

/// The process-wide registry: starts as [`DomainRegistry::builtin`] and is
/// extendable at runtime through [`register_domain`].
static REGISTRY: std::sync::LazyLock<std::sync::RwLock<DomainRegistry>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(DomainRegistry::builtin()));

/// Register a domain plugin in the process-wide registry. A taken id is a
/// loud refusal; swapping is [`replace_domain`].
pub fn register_domain(domain: &'static dyn Domain) -> Result<()> {
    REGISTRY
        .write()
        .expect("domain registry lock poisoned")
        .register(domain)
}

/// Deliberately swap a domain registered in the process-wide registry.
/// Returns the displaced domain — hand it back to restore.
pub fn replace_domain(domain: &'static dyn Domain) -> Result<&'static dyn Domain> {
    REGISTRY
        .write()
        .expect("domain registry lock poisoned")
        .replace(domain)
}

/// Resolve a domain id through the process-wide registry. An id nothing
/// registered is a LOUD error naming what is registered — never a silent
/// fallback to the alloy domain, which would run the wrong physics while
/// looking configured.
pub fn resolve_domain(id: &str) -> Result<&'static dyn Domain> {
    let registry = REGISTRY.read().expect("domain registry lock poisoned");
    registry.get(id).ok_or_else(|| {
        anyhow::anyhow!(
            "no domain '{id}' is registered (registered: {}). Set \
             CampaignConfig::domain to a registered id, or register yours with \
             prism_campaign::domain::register_domain",
            registry.ids().join(", ")
        )
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// CONTRACT CHANGE (dehardcoding): the domain registry is OPEN — a new
    /// domain registers at runtime with ZERO Rust edits (no enum variant,
    /// no match arm), mirroring the ontology registry. This test registers
    /// a synthetic LEGAL domain (arbitrary vocabulary, a stand-in) and
    /// proves resolution, the two-call contract, and the loud refusal.
    #[test]
    fn the_domain_registry_is_open_and_refuses_loudly() {
        struct LegalDomain;
        impl Domain for LegalDomain {
            fn name(&self) -> &'static str {
                "dehardcode-test-legal"
            }
            fn candidate_plural(&self) -> &'static str {
                "cases"
            }
            fn validate_goal(
                &self,
                _goal: &crate::CampaignGoal,
            ) -> std::result::Result<(), String> {
                Ok(())
            }
            fn apply_goal_implied_constraints(
                &self,
                _config: &mut crate::CampaignConfig,
                _goal: &crate::CampaignGoal,
            ) {
            }
            fn parse_candidate(
                &self,
                _candidate: &str,
                _goal: &crate::CampaignGoal,
            ) -> std::result::Result<ParsedCandidate, String> {
                unreachable!("not exercised")
            }
            fn definition(&self, _config: &crate::CampaignConfig) -> Option<serde_json::Value> {
                None
            }
            fn definition_label(&self) -> &'static str {
                "Legal domain"
            }
            fn configured_constraints(&self, _config: &crate::CampaignConfig) -> Vec<String> {
                Vec::new()
            }
            fn constraint_violations(
                &self,
                _parsed: &ParsedCandidate,
                _properties: &serde_json::Value,
                _config: &crate::CampaignConfig,
            ) -> Vec<String> {
                Vec::new()
            }
            fn evaluator_tool(&self) -> &'static str {
                "unavailable"
            }
            fn evaluator_tiers(&self) -> &'static [EvaluatorTier] {
                &[]
            }
            fn evaluator_inputs(&self, _parsed: &ParsedCandidate) -> serde_json::Value {
                serde_json::json!({})
            }
            fn decorate_properties(
                &self,
                _properties: &mut serde_json::Value,
                _parsed: &ParsedCandidate,
                _config: &crate::CampaignConfig,
            ) -> Result<()> {
                Ok(())
            }
            fn compute_reward(
                &self,
                _goal: &crate::CampaignGoal,
                _config: &crate::CampaignConfig,
                _properties: &serde_json::Value,
            ) -> Result<f64> {
                Ok(0.0)
            }
            fn summarize_properties(&self, _properties: &serde_json::Value) -> String {
                String::new()
            }
            fn proposal_system_prompt(&self) -> &'static str {
                ""
            }
            fn search_space_prompt(&self, _goal: &crate::CampaignGoal) -> String {
                String::new()
            }
            fn improvement_prompt(&self, _batch: usize) -> String {
                String::new()
            }
            fn initial_prompt(&self, _batch: usize) -> String {
                String::new()
            }
        }

        // Built-ins resolve by id.
        assert_eq!(resolve_domain(ALLOY_DOMAIN_ID).unwrap().name(), "alloy");
        assert_eq!(resolve_domain(POLYMER_DOMAIN_ID).unwrap().name(), "polymer");
        // Unknown id: loud, names what is registered — never the alloy default.
        let Err(error) = resolve_domain("obligation") else {
            panic!("an unregistered domain id must not resolve");
        };
        let message = format!("{error:#}");
        assert!(message.contains("no domain 'obligation'"), "{message}");
        assert!(message.contains("alloy"), "{message}");
        // The registry contract: register refuses a taken id, replace
        // refuses a free one.
        let mut registry = DomainRegistry::builtin();
        assert!(registry.register(&alloy::ALLOY_DOMAIN).is_err());
        let Err(free_error) = registry.replace(&LegalDomain) else {
            panic!("replacing a FREE id must be refused");
        };
        assert!(
            format!("{free_error:#}").contains("no domain"),
            "{free_error:#}"
        );
        registry
            .register(&LegalDomain)
            .expect("a free id registers");
        assert_eq!(
            registry.get("dehardcode-test-legal").unwrap().name(),
            "dehardcode-test-legal"
        );
    }
}
