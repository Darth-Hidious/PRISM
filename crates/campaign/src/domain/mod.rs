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

    /// The reward vocabulary: `(property, unit)` pairs keyed exactly as this
    /// domain's evaluator reports them. Reward validation and derivation both
    /// read THIS list, so an objective can never name a property the active
    /// domain's evaluator will not report — an alloy descriptor offered to a
    /// polymer campaign is refused by name, never silently dropped.
    fn reward_registry(&self) -> &'static [(&'static str, &'static str)];

    /// Refuse, by name and before compute is spent, campaign configuration
    /// this domain cannot honour. A configured objective that is silently
    /// ignored runs a campaign nobody asked for.
    fn validate_config(&self, config: &CampaignConfig, goal: &CampaignGoal) -> Result<()> {
        validate_reward_vocabulary(self.name(), self.reward_registry(), config, goal)
    }

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

/// Everything the reward path ranks on — spec terms, weighted properties,
/// the declared target property — must be in the domain's reward registry.
/// Refusal is BY NAME: a silently dropped term is an objective nobody agreed
/// to, and a foreign property (an alloy descriptor on a polymer campaign)
/// would rank nothing or, worse, rank on a coincidence.
pub(crate) fn validate_reward_vocabulary(
    domain_id: &str,
    registry: &[(&'static str, &'static str)],
    config: &CampaignConfig,
    goal: &CampaignGoal,
) -> Result<()> {
    if let Some(spec) = &config.reward_spec {
        validate_spec_vocabulary(domain_id, registry, spec)?;
    }
    for property in config.reward_weights.keys() {
        ensure_in_registry(domain_id, registry, property, "reward weight")?;
    }
    if let Some(property) = &goal.target_property {
        ensure_in_registry(domain_id, registry, property, "goal.target_property")?;
    }
    Ok(())
}

/// Refuse a reward spec whose terms name properties outside the domain's
/// registry. Called both when a stored spec is about to score and when a
/// campaign starts, so the refusal happens before compute is spent.
pub(crate) fn validate_spec_vocabulary(
    domain_id: &str,
    registry: &[(&'static str, &'static str)],
    spec: &crate::reward::RewardSpec,
) -> Result<()> {
    for term in &spec.terms {
        ensure_in_registry(domain_id, registry, &term.property, "reward objective term")?;
    }
    Ok(())
}

/// The reward sign a declared target property earns, from the goal's
/// DECLARED direction. Both domains used to read it off the objective text
/// with `objective.contains("minimize")`; a differently worded or non-English
/// objective ranked as maximize and a campaign returned its worst candidates
/// as best. A declared property with no declared direction is refused here,
/// by name, before any budget is spent ranking on a guessed sign.
pub fn signed_by_direction(
    goal: &crate::CampaignGoal,
    property: &str,
    value: f64,
) -> anyhow::Result<f64> {
    match goal.target_direction {
        Some(crate::Direction::Maximize) => Ok(value),
        Some(crate::Direction::Minimize) => Ok(-value),
        None => anyhow::bail!(
            "goal declares target_property {property:?} but no target_direction — declare \
             \"maximize\" or \"minimize\"; the sign is never inferred from the objective text"
        ),
    }
}

pub(crate) fn ensure_in_registry(
    domain_id: &str,
    registry: &[(&'static str, &'static str)],
    property: &str,
    role: &str,
) -> Result<()> {
    if registry.iter().any(|(name, _)| *name == property) {
        return Ok(());
    }
    let allowed = registry
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "{role} '{property}' is not a property the '{domain_id}' domain's evaluator reports, \
         so it cannot be optimised here. Allowed properties: [{allowed}]"
    )
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
            fn reward_registry(&self) -> &'static [(&'static str, &'static str)] {
                &[]
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

    fn bare_goal() -> crate::CampaignGoal {
        crate::CampaignGoal {
            description: String::new(),
            elements: Vec::new(),
            objective: String::new(),
            target_property: None,
            target_direction: None,
            constraints: Vec::new(),
            seeds: Vec::new(),
        }
    }

    /// A reward property is valid only in ITS domain's vocabulary: a polymer
    /// key weighted on an alloy campaign (and the reverse, a target property
    /// from the alloy set on a polymer campaign) is refused BY NAME, with the
    /// domain's own vocabulary listed — never silently ignored.
    #[test]
    fn reward_properties_outside_the_domain_registry_are_refused_by_name() {
        let alloy = resolve_domain(ALLOY_DOMAIN_ID).unwrap();
        let polymer = resolve_domain(POLYMER_DOMAIN_ID).unwrap();

        let mut config = crate::CampaignConfig::default();
        config
            .reward_weights
            .insert("glass_transition_temperature_k".into(), 1.0);
        let error = alloy
            .validate_config(&config, &bare_goal())
            .expect_err("a polymer key must not pass alloy validation");
        let message = format!("{error:#}");
        assert!(
            message.contains("glass_transition_temperature_k"),
            "{message}"
        );
        assert!(message.contains("'alloy'"), "{message}");
        assert!(
            message.contains("Tm_estimate_K"),
            "vocabulary listed: {message}"
        );

        let mut goal = bare_goal();
        goal.target_property = Some("Tm_estimate_K".into());
        goal.target_direction = Some(crate::Direction::Maximize);
        let error = polymer
            .validate_config(&crate::CampaignConfig::default(), &goal)
            .expect_err("an alloy key must not pass polymer validation");
        let message = format!("{error:#}");
        assert!(message.contains("Tm_estimate_K"), "{message}");
        assert!(message.contains("'polymer'"), "{message}");

        // Each domain still accepts its OWN vocabulary.
        let mut alloy_config = crate::CampaignConfig::default();
        alloy_config
            .reward_weights
            .insert("Tm_estimate_K".into(), 1.0);
        alloy
            .validate_config(&alloy_config, &bare_goal())
            .expect("alloy vocabulary passes on the alloy domain");
        let mut polymer_goal = bare_goal();
        polymer_goal.target_property = Some("glass_transition_temperature_k".into());
        polymer_goal.target_direction = Some(crate::Direction::Maximize);
        polymer
            .validate_config(&crate::CampaignConfig::default(), &polymer_goal)
            .expect("polymer vocabulary passes on the polymer domain");
    }

    /// "I'm not gonna put tungsten in polymers": HEA compositional thresholds
    /// are alloy vocabulary. A polymer campaign configured with one is
    /// refused, never run with the threshold silently unenforced.
    #[test]
    fn hea_constraints_on_a_polymer_campaign_are_refused_not_ignored() {
        let polymer = resolve_domain(POLYMER_DOMAIN_ID).unwrap();
        let alloy = resolve_domain(ALLOY_DOMAIN_ID).unwrap();

        let config = crate::CampaignConfig {
            min_configurational_entropy_j_per_mol_k: Some(8.314),
            ..Default::default()
        };
        let error = polymer
            .validate_config(&config, &bare_goal())
            .expect_err("an HEA entropy floor is meaningless for polymers");
        let message = format!("{error:#}");
        assert!(
            message.contains("min_configurational_entropy_j_per_mol_k"),
            "{message}"
        );
        // The same config is legitimate on the alloy domain.
        alloy
            .validate_config(&config, &bare_goal())
            .expect("HEA thresholds are alloy vocabulary");

        let config = crate::CampaignConfig {
            min_principal_elements: Some(4),
            ..Default::default()
        };
        assert!(polymer.validate_config(&config, &bare_goal()).is_err());
        let config = crate::CampaignConfig {
            hea_definition: Some(crate::HeaDefinition::PermissiveRhea),
            ..Default::default()
        };
        assert!(polymer.validate_config(&config, &bare_goal()).is_err());
    }

    /// The sign comes from the declaration, never from the words. "lower the
    /// density" with no declared direction is refused, not maximized.
    #[test]
    fn the_reward_sign_is_declared_never_inferred_from_the_objective() {
        let mut goal = crate::CampaignGoal {
            description: "d".into(),
            elements: Vec::new(),
            objective: "lower the density".into(),
            target_property: Some("density_g_cm3".into()),
            target_direction: None,
            constraints: Vec::new(),
            seeds: Vec::new(),
        };
        let err = signed_by_direction(&goal, "density_g_cm3", 7.9).unwrap_err();
        assert!(err.to_string().contains("target_direction"), "{err:#}");
        goal.target_direction = Some(crate::Direction::Minimize);
        assert_eq!(
            signed_by_direction(&goal, "density_g_cm3", 7.9).unwrap(),
            -7.9
        );
        goal.target_direction = Some(crate::Direction::Maximize);
        assert_eq!(
            signed_by_direction(&goal, "density_g_cm3", 7.9).unwrap(),
            7.9
        );
    }
}
