//! Alloy-domain plugin.
//!
//! This module deliberately contains the element grammar, HEA definitions,
//! empirical descriptor vocabulary, and alloy reward policy. The campaign
//! engine calls these through [`super::Domain`] and does not interpret alloy
//! physics itself.

use std::collections::BTreeSet;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use super::{
    Domain, EvaluatorTier, ParsedCandidate, structured_constraint_descriptions,
    structured_constraint_violations,
};
use crate::{CampaignConfig, CampaignGoal};

pub(crate) const ALLOY_DOMAIN: AlloyDomain = AlloyDomain;
pub(crate) const EVALUATION_TOOL: &str = "hea_descriptors";
pub(crate) const PRINCIPAL_ELEMENT_MIN_ATOMIC_FRACTION: f64 = 0.05;

/// Maximum absolute error accepted for `sum(fractions) == 1.0`.
///
/// `1e-6` admits ordinary decimal round-off (for example, three fractions
/// written as `0.333333`, `0.333333`, `0.333334`) without accepting ratios,
/// percentages, or model output that needs normalization. PRISM rejects
/// outside this tolerance; it never silently changes the proposed material.
pub const COMPOSITION_SUM_TOLERANCE: f64 = 1e-6;

const ELEMENT_SYMBOLS: &str = "H He Li Be B C N O F Ne Na Mg Al Si P S Cl Ar K Ca Sc Ti V Cr Mn Fe Co Ni Cu Zn Ga Ge As Se Br Kr Rb Sr Y Zr Nb Mo Tc Ru Rh Pd Ag Cd In Sn Sb Te I Xe Cs Ba La Ce Pr Nd Pm Sm Eu Gd Tb Dy Ho Er Tm Yb Lu Hf Ta W Re Os Ir Pt Au Hg Tl Pb Bi Po At Rn Fr Ra Ac Th Pa U Np Pu Am Cm Bk Cf Es Fm Md No Lr Rf Db Sg Bh Hs Mt Ds Rg Cn Nh Fl Mc Lv Ts Og";

fn is_element_symbol(symbol: &str) -> bool {
    ELEMENT_SYMBOLS
        .split_ascii_whitespace()
        .any(|candidate| candidate == symbol)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExpandedComposition {
    pub(crate) original: String,
    pub(crate) expanded: String,
    pub(crate) fractions: Vec<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompositionNotation {
    BareEquiatomic,
    AtomicPercent,
    AtomicFraction,
}

/// Parse standard HEA notation, expand it to explicit atomic fractions, then
/// enforce strict validation. Bare symbols mean equiatomic; integer suffixes
/// mean atomic percent; decimal/scientific suffixes mean atomic fractions.
/// Mixing those conventions is rejected rather than guessed.
pub(crate) fn parse_and_expand_composition(
    composition: &str,
    allowed_elements: &[String],
) -> std::result::Result<ExpandedComposition, String> {
    let original = composition.trim();
    if original.is_empty() {
        return Err("composition is empty".into());
    }
    let compact = original
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect::<String>();
    let bytes = compact.as_bytes();
    let mut index = 0;
    let mut seen_elements = BTreeSet::new();
    let mut element_names = Vec::new();
    let mut values = Vec::new();
    let mut notation = None;

    while index < bytes.len() {
        let symbol_start = index;
        if !bytes[index].is_ascii_uppercase() {
            return Err(format!(
                "expected an element symbol at byte {index} in '{original}'"
            ));
        }
        index += 1;
        if index < bytes.len() && bytes[index].is_ascii_lowercase() {
            index += 1;
        }
        let symbol = &compact[symbol_start..index];
        if !is_element_symbol(symbol) {
            return Err(format!("'{symbol}' is not a real element symbol"));
        }
        if !allowed_elements.is_empty() && !allowed_elements.iter().any(|item| item == symbol) {
            return Err(format!(
                "element {symbol} is outside the allowed set [{}]",
                allowed_elements.join(", ")
            ));
        }
        if !seen_elements.insert(symbol.to_string()) {
            return Err(format!("element {symbol} appears more than once"));
        }
        element_names.push(symbol.to_string());

        let fraction_start = index;
        let mut has_mantissa_digit = false;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
            has_mantissa_digit = true;
        }
        let mut is_fraction = false;
        if index < bytes.len() && bytes[index] == b'.' {
            is_fraction = true;
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
                has_mantissa_digit = true;
            }
            if !has_mantissa_digit {
                return Err(format!("invalid decimal fraction for element {symbol}"));
            }
        }
        if has_mantissa_digit
            && index < bytes.len()
            && matches!(bytes[index], b'e' | b'E')
            && index + 1 < bytes.len()
            && (matches!(bytes[index + 1], b'+' | b'-') || bytes[index + 1].is_ascii_digit())
        {
            is_fraction = true;
            index += 1;
            if index < bytes.len() && matches!(bytes[index], b'+' | b'-') {
                index += 1;
            }
            let exponent_start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            if exponent_start == index {
                return Err(format!("invalid exponent for element {symbol}"));
            }
        }

        // A bare integer zero is invalid in either notation. Treat it as a
        // fraction so `W1.0 Mo0` retains the precise positivity error instead
        // of becoming a misleading mixed-notation error.
        if !is_fraction && fraction_start < index && &compact[fraction_start..index] == "0" {
            is_fraction = true;
        }
        let current_notation = if fraction_start == index {
            CompositionNotation::BareEquiatomic
        } else if is_fraction {
            CompositionNotation::AtomicFraction
        } else {
            CompositionNotation::AtomicPercent
        };
        if let Some(previous) = notation
            && previous != current_notation
        {
            return Err(
                "composition mixes bare, atomic-percent, or atomic-fraction notation".into(),
            );
        }
        notation = Some(current_notation);

        if current_notation != CompositionNotation::BareEquiatomic {
            let token = &compact[fraction_start..index];
            let value = token
                .parse::<f64>()
                .map_err(|_| format!("invalid fraction '{token}' for element {symbol}"))?;
            if !value.is_finite() {
                return Err(format!("fraction for element {symbol} must be finite"));
            }
            if value <= 0.0 {
                return Err(format!(
                    "fraction for element {symbol} must be strictly positive"
                ));
            }
            values.push(value);
        }

        if index < bytes.len() && !bytes[index].is_ascii_uppercase() {
            return Err(format!(
                "unexpected character after fraction for element {symbol}"
            ));
        }
    }

    let fractions = match notation {
        Some(CompositionNotation::BareEquiatomic) => {
            if element_names.len() < 2 {
                return Err("composition must contain at least two elements".into());
            }
            vec![1.0 / element_names.len() as f64; element_names.len()]
        }
        Some(CompositionNotation::AtomicPercent) => {
            let sum: f64 = values.iter().sum();
            let tolerance = COMPOSITION_SUM_TOLERANCE * 100.0;
            if (sum - 100.0).abs() > tolerance {
                return Err(format!(
                    "atomic-percent suffixes must sum to 100.0 ± {tolerance:.6}; got {sum:.6}"
                ));
            }
            values.into_iter().map(|value| value / 100.0).collect()
        }
        Some(CompositionNotation::AtomicFraction) => values,
        None => return Err("composition is empty".into()),
    };

    let sum: f64 = fractions.iter().sum();
    if !sum.is_finite() {
        return Err("composition fraction sum must be finite".into());
    }
    if (sum - 1.0).abs() > COMPOSITION_SUM_TOLERANCE {
        return Err(format!(
            "composition fractions must sum to 1.0 ± {COMPOSITION_SUM_TOLERANCE:.6}; got {sum:.6}"
        ));
    }
    let expanded = element_names
        .iter()
        .zip(&fractions)
        .map(|(element, fraction)| format!("{element}{fraction}"))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(ExpandedComposition {
        original: original.to_string(),
        expanded,
        fractions,
    })
}

#[cfg(test)]
pub(crate) fn validate_composition(
    composition: &str,
    allowed_elements: &[String],
) -> std::result::Result<(), String> {
    parse_and_expand_composition(composition, allowed_elements).map(|_| ())
}

fn validate_allowed_elements(elements: &[String]) -> std::result::Result<(), String> {
    let mut seen = BTreeSet::new();
    for symbol in elements {
        if !is_element_symbol(symbol) {
            return Err(format!("'{symbol}' is not a real allowed element symbol"));
        }
        if !seen.insert(symbol) {
            return Err(format!("allowed element {symbol} appears more than once"));
        }
    }
    Ok(())
}

/// Permissive RHEA definition: at least four principal elements and a mixing
/// entropy of at least `R`. This admits equiatomic NbMoTaW (`R ln(4)`) from
/// the founding refractory-HEA study.
///
/// Source: O. N. Senkov et al., "Refractory high-entropy alloys,"
/// Intermetallics 18 (2010) 1758-1765,
/// <https://doi.org/10.1016/j.intermet.2010.05.014>.
pub const DEFAULT_HEA_MIN_CONFIG_ENTROPY_J_PER_MOL_K: f64 = 8.314;
pub const DEFAULT_HEA_MIN_PRINCIPAL_ELEMENTS: usize = 4;

/// Strict Yeh definition: at least five principal elements and `ΔS_mix >=
/// 1.5R`, conventionally rounded to 12.47 J/(mol K).
///
/// Source: J.-W. Yeh et al., "Nanostructured High-Entropy Alloys with Multiple
/// Principal Elements: Novel Alloy Design Concepts and Outcomes," Advanced
/// Engineering Materials 6 (2004) 299-303,
/// <https://doi.org/10.1002/adem.200300567>.
pub const STRICT_YEH_MIN_CONFIG_ENTROPY_J_PER_MOL_K: f64 = 12.47;
pub const STRICT_YEH_MIN_PRINCIPAL_ELEMENTS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeaDefinition {
    /// RHEA-inclusive policy based on Senkov's four-principal-element NbMoTaW.
    PermissiveRhea,
    /// Original strict multiple-principal-element policy from Yeh et al.
    StrictYeh,
    /// Explicit per-campaign thresholds that differ from either named preset.
    Custom,
}

impl HeaDefinition {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::PermissiveRhea => "permissive_rhea",
            Self::StrictYeh => "strict_yeh",
            Self::Custom => "custom",
        }
    }

    pub(crate) fn source(self) -> &'static str {
        match self {
            Self::PermissiveRhea => {
                "Senkov et al., Intermetallics 18 (2010), DOI 10.1016/j.intermet.2010.05.014"
            }
            Self::StrictYeh => {
                "Yeh et al., Advanced Engineering Materials 6 (2004), DOI 10.1002/adem.200300567"
            }
            Self::Custom => {
                "campaign-configured thresholds (permissive RHEA baseline: Senkov et al., Intermetallics 18 (2010), DOI 10.1016/j.intermet.2010.05.014)"
            }
        }
    }

    fn defaults(self) -> (f64, usize) {
        match self {
            Self::PermissiveRhea | Self::Custom => (
                DEFAULT_HEA_MIN_CONFIG_ENTROPY_J_PER_MOL_K,
                DEFAULT_HEA_MIN_PRINCIPAL_ELEMENTS,
            ),
            Self::StrictYeh => (
                STRICT_YEH_MIN_CONFIG_ENTROPY_J_PER_MOL_K,
                STRICT_YEH_MIN_PRINCIPAL_ELEMENTS,
            ),
        }
    }
}

pub(crate) fn goal_explicitly_requests_hea(goal: &CampaignGoal) -> bool {
    let text = format!(
        "{} {} {}",
        goal.description,
        goal.objective,
        goal.constraints.join(" ")
    )
    .to_ascii_lowercase();
    let words = text
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();

    words.iter().any(|word| matches!(*word, "hea" | "heas"))
        || words.windows(3).any(|window| {
            window[0] == "high" && window[1] == "entropy" && matches!(window[2], "alloy" | "alloys")
        })
}

const ALLOY_EVALUATOR_TIERS: [EvaluatorTier; 1] = [EvaluatorTier {
    tier: 0,
    name: "empirical_hea_descriptors",
    method: "Yang omega/radius-mismatch and Guo-Liu VEC empirical screening",
    citation: "Yang & Zhang, Materials Chemistry and Physics 132 (2012); Guo & Liu, Intermetallics 19 (2011)",
}];

pub(crate) struct AlloyDomain;

impl Domain for AlloyDomain {
    fn name(&self) -> &'static str {
        "alloy"
    }

    fn candidate_plural(&self) -> &'static str {
        "compositions"
    }

    fn validate_goal(&self, goal: &CampaignGoal) -> std::result::Result<(), String> {
        validate_allowed_elements(&goal.elements)
    }

    fn apply_goal_implied_constraints(&self, config: &mut CampaignConfig, goal: &CampaignGoal) {
        let has_explicit_hea_constraint = config.hea_definition.is_some()
            || config.min_configurational_entropy_j_per_mol_k.is_some()
            || config.min_principal_elements.is_some();
        if !goal_explicitly_requests_hea(goal) && !has_explicit_hea_constraint {
            return;
        }

        let requested = config.hea_definition.unwrap_or(
            match (
                config.min_configurational_entropy_j_per_mol_k,
                config.min_principal_elements,
            ) {
                (Some(entropy), Some(principal_elements))
                    if entropy == STRICT_YEH_MIN_CONFIG_ENTROPY_J_PER_MOL_K
                        && principal_elements == STRICT_YEH_MIN_PRINCIPAL_ELEMENTS =>
                {
                    HeaDefinition::StrictYeh
                }
                (Some(entropy), Some(principal_elements))
                    if entropy == DEFAULT_HEA_MIN_CONFIG_ENTROPY_J_PER_MOL_K
                        && principal_elements == DEFAULT_HEA_MIN_PRINCIPAL_ELEMENTS =>
                {
                    HeaDefinition::PermissiveRhea
                }
                (Some(_), _) | (_, Some(_)) => HeaDefinition::Custom,
                (None, None) => HeaDefinition::PermissiveRhea,
            },
        );
        let (default_entropy, default_principal_elements) = requested.defaults();
        let entropy = config
            .min_configurational_entropy_j_per_mol_k
            .get_or_insert(default_entropy);
        let principal_elements = config
            .min_principal_elements
            .get_or_insert(default_principal_elements);
        config.hea_definition = Some(if (*entropy, *principal_elements) == requested.defaults() {
            requested
        } else {
            HeaDefinition::Custom
        });
    }

    fn parse_candidate(
        &self,
        candidate: &str,
        goal: &CampaignGoal,
    ) -> std::result::Result<ParsedCandidate, String> {
        parse_and_expand_composition(candidate, &goal.elements).map(|parsed| ParsedCandidate {
            original: parsed.original,
            canonical: parsed.expanded,
        })
    }

    fn definition(&self, config: &CampaignConfig) -> Option<serde_json::Value> {
        let definition = config.hea_definition?;
        let entropy = config.min_configurational_entropy_j_per_mol_k?;
        let principal_elements = config.min_principal_elements?;
        Some(serde_json::json!({
            "name": definition.name(),
            "min_configurational_entropy_j_per_mol_k": entropy,
            "min_principal_elements": principal_elements,
            "principal_element_min_atomic_fraction": PRINCIPAL_ELEMENT_MIN_ATOMIC_FRACTION,
            "source": definition.source(),
        }))
    }

    fn definition_label(&self) -> &'static str {
        "HEA definition"
    }

    fn configured_constraints(&self, config: &CampaignConfig) -> Vec<String> {
        let mut constraints = Vec::new();
        let definition = config
            .hea_definition
            .map(HeaDefinition::name)
            .unwrap_or("unnamed");
        if let Some(minimum) = config.min_configurational_entropy_j_per_mol_k {
            constraints.push(format!(
                "[{definition}] delta_S_mix_J_per_molK >= {minimum:.4} J/(mol K)"
            ));
        }
        if let Some(minimum) = config.min_principal_elements {
            constraints.push(format!(
                "[{definition}] principal elements >= {minimum} (each >= {:.0} at.%)",
                PRINCIPAL_ELEMENT_MIN_ATOMIC_FRACTION * 100.0
            ));
        }
        constraints.extend(structured_constraint_descriptions(
            &config.property_constraints,
        ));
        constraints
    }

    fn constraint_violations(
        &self,
        _parsed: &ParsedCandidate,
        properties: &serde_json::Value,
        config: &CampaignConfig,
    ) -> Vec<String> {
        let mut violations = Vec::new();
        let definition = config
            .hea_definition
            .map(HeaDefinition::name)
            .unwrap_or("unnamed");

        if let Some(minimum) = config.min_configurational_entropy_j_per_mol_k {
            let entropy = properties
                .get("delta_S_mix_J_per_molK")
                .and_then(serde_json::Value::as_f64)
                .or_else(|| {
                    properties
                        .get("mixing_entropy")
                        .or_else(|| properties.get("entropy"))
                        .and_then(serde_json::Value::as_f64)
                        .map(|in_gas_constant_units| in_gas_constant_units * 8.314)
                });
            match entropy {
                Some(value) if value < minimum => violations.push(format!(
                    "[{definition}] delta_S_mix_J_per_molK={value:.4} J/(mol K) is below hard minimum {minimum:.4}"
                )),
                Some(_) => {}
                None => violations.push(format!(
                    "[{definition}] {EVALUATION_TOOL} returned no configurational-entropy descriptor required by hard minimum {minimum:.4} J/(mol K)"
                )),
            }
        }

        if let Some(minimum) = config.min_principal_elements {
            let fractions = properties
                .get("fractions")
                .and_then(serde_json::Value::as_array);
            match fractions {
                Some(fractions) if fractions.iter().all(serde_json::Value::is_number) => {
                    let count = fractions
                        .iter()
                        .filter_map(serde_json::Value::as_f64)
                        .filter(|fraction| *fraction >= PRINCIPAL_ELEMENT_MIN_ATOMIC_FRACTION)
                        .count();
                    if count < minimum {
                        violations.push(format!(
                            "[{definition}] principal-element count {count} is below hard minimum {minimum} ({:.0} at.% cutoff)",
                            PRINCIPAL_ELEMENT_MIN_ATOMIC_FRACTION * 100.0
                        ));
                    }
                }
                _ => violations.push(format!(
                    "[{definition}] {EVALUATION_TOOL} returned no numeric composition fractions required to verify hard minimum {minimum} principal elements"
                )),
            }
        }
        violations.extend(structured_constraint_violations(
            &config.property_constraints,
            properties,
            EVALUATION_TOOL,
        ));
        violations
    }

    fn evaluator_tool(&self) -> &'static str {
        EVALUATION_TOOL
    }

    fn evaluator_tiers(&self) -> &'static [EvaluatorTier] {
        &ALLOY_EVALUATOR_TIERS
    }

    fn evaluator_inputs(&self, parsed: &ParsedCandidate) -> serde_json::Value {
        serde_json::json!({ "composition": parsed.canonical })
    }

    fn decorate_properties(
        &self,
        properties: &mut serde_json::Value,
        parsed: &ParsedCandidate,
        config: &CampaignConfig,
    ) -> Result<()> {
        let object = properties.as_object_mut().ok_or_else(|| {
            anyhow::anyhow!("{EVALUATION_TOOL} returned a non-object descriptor payload")
        })?;
        object.insert(
            "original_composition".into(),
            serde_json::Value::String(parsed.original.clone()),
        );
        object.insert(
            "expanded_composition".into(),
            serde_json::Value::String(parsed.canonical.clone()),
        );
        if let Some(definition) = self.definition(config) {
            object.insert("hea_definition".into(), definition);
        }
        Ok(())
    }

    fn compute_reward(
        &self,
        goal: &CampaignGoal,
        config: &CampaignConfig,
        props: &serde_json::Value,
    ) -> Result<f64> {
        if config.reward_weights.is_empty() {
            let objective = goal.objective.to_ascii_lowercase();
            if objective.contains("melting point") {
                let melting_point = props
                    .get("Tm_estimate_K")
                    .or_else(|| props.get("melting_point_k"))
                    .or_else(|| props.get("melting_point"))
                    .and_then(serde_json::Value::as_f64)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "{EVALUATION_TOOL} returned no numeric melting-point descriptor for objective '{}'",
                            goal.objective
                        )
                    })?;
                return Ok(if objective.contains("minimize") {
                    -melting_point
                } else {
                    melting_point
                });
            }

            // Existing alloy policy: high entropy and, when available, lower
            // density. Missing descriptors are never replaced with defaults.
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

        weighted_reward(config, props, EVALUATION_TOOL)
    }

    fn summarize_properties(&self, properties: &serde_json::Value) -> String {
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

    fn proposal_system_prompt(&self) -> &'static str {
        "You are a materials scientist designing novel alloys. Respond with ONLY a JSON array of composition strings, no explanation. Example: [\"W0.3 Mo0.2 Ta0.3 Nb0.2\", \"Cr0.4 V0.3 Ti0.3\"]"
    }

    fn search_space_prompt(&self, goal: &CampaignGoal) -> String {
        if goal.elements.is_empty() {
            String::new()
        } else {
            format!("Allowed elements: {}\n", goal.elements.join(", "))
        }
    }

    fn improvement_prompt(&self, batch: usize) -> String {
        format!(
            "\nPropose {batch} NEW compositions that improve on these. Vary the ratios and try new element combinations within the allowed set.\n"
        )
    }

    fn initial_prompt(&self, batch: usize) -> String {
        format!("\nPropose {batch} initial candidate compositions.\n")
    }
}

pub(crate) fn weighted_reward(
    config: &CampaignConfig,
    props: &serde_json::Value,
    evaluator_tool: &str,
) -> Result<f64> {
    let mut reward = 0.0;
    for (prop, weight) in &config.reward_weights {
        let value = props
            .get(prop)
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{evaluator_tool} returned no numeric value for weighted property '{prop}'"
                )
            })?;
        reward += value * weight;
    }
    Ok(reward)
}
