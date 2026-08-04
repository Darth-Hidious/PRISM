//! Polymer electrical-insulation domain plugin.
//!
//! Candidate identity is structured JSON, not an elemental composition. The
//! local evaluator is optional because chemical validation uses RDKit. This
//! domain never substitutes guessed dielectric, breakdown, or conductivity
//! values when the evaluator reports them unavailable.

use anyhow::{Result, bail};

use super::alloy::weighted_reward;
use super::{
    Domain, EvaluatorTier, ParsedCandidate, structured_constraint_descriptions,
    structured_constraint_violations,
};
use crate::{CampaignConfig, CampaignGoal};

pub(crate) const POLYMER_DOMAIN: PolymerDomain = PolymerDomain;
pub(crate) const EVALUATION_TOOL: &str = "polymer_insulation_properties";
pub const RDKIT_INSTALL_HINT: &str = "Install RDKit in the PRISM Python environment with `python -m pip install rdkit`, then restart the PRISM node; the polymer evaluator is not registered without RDKit.";

const TARGET_PROPERTIES: [&str; 4] = [
    "glass_transition_temperature_k",
    "dielectric_constant",
    "dielectric_breakdown_strength_kv_per_mm",
    "thermal_conductivity_w_per_m_k",
];

const POLYMER_EVALUATOR_TIERS: [EvaluatorTier; 1] = [EvaluatorTier {
    tier: 0,
    name: "fox_flory_and_explicit_unavailability",
    method: "RDKit identity validation; Fox-Flory molecular-weight relation for Tg only when polymer-specific parameters and their source are supplied; no fallback estimates",
    citation: "T. G. Fox and P. J. Flory, Journal of Applied Physics 21 (1950) 581-591, DOI 10.1063/1.1699711",
}];

pub(crate) struct PolymerDomain;

impl PolymerDomain {
    fn validate_structured_identity(
        &self,
        candidate: &str,
    ) -> std::result::Result<ParsedCandidate, String> {
        let original = candidate.trim();
        if original.is_empty() {
            return Err("polymer candidate identity is empty".into());
        }
        let value: serde_json::Value = serde_json::from_str(original).map_err(|error| {
            format!(
                "polymer candidate must be a JSON object with representation 'repeat_unit', 'monomer', or 'smiles': {error}"
            )
        })?;
        let object = value
            .as_object()
            .ok_or_else(|| "polymer candidate identity must be a JSON object".to_string())?;
        let representation = object
            .get("representation")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "polymer candidate needs a string 'representation'".to_string())?;
        match representation {
            "repeat_unit" => {
                let repeat_unit = object
                    .get("repeat_unit")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim();
                if repeat_unit.is_empty() {
                    return Err(
                        "repeat_unit representation needs a non-empty 'repeat_unit' string".into(),
                    );
                }
            }
            "monomer" => {
                let monomer = object
                    .get("monomer")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim();
                if monomer.is_empty() {
                    return Err("monomer representation needs a non-empty 'monomer' string".into());
                }
            }
            "smiles" => {
                let smiles = object
                    .get("smiles")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim();
                if smiles.is_empty() {
                    return Err("SMILES representation needs a non-empty 'smiles' string".into());
                }
                // Chemical validity is deliberately deferred to the registered
                // RDKit evaluator. Reimplementing SMILES chemistry here would
                // create a second, less trustworthy parser.
            }
            other => {
                return Err(format!(
                    "unsupported polymer representation '{other}'; expected repeat_unit, monomer, or smiles"
                ));
            }
        }

        if let Some(fox_flory) = object.get("fox_flory") {
            let parameters = fox_flory
                .as_object()
                .ok_or_else(|| "'fox_flory' must be an object".to_string())?;
            for field in [
                "number_average_molar_mass_g_per_mol",
                "tg_infinity_k",
                "k_k_g_per_mol",
            ] {
                let value = parameters
                    .get(field)
                    .and_then(serde_json::Value::as_f64)
                    .ok_or_else(|| format!("fox_flory.{field} must be numeric"))?;
                if !value.is_finite() || value <= 0.0 {
                    return Err(format!("fox_flory.{field} must be finite and positive"));
                }
            }
            if parameters
                .get("parameter_citation")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                return Err(
                    "fox_flory parameters need a non-empty 'parameter_citation'; PRISM will not treat unsourced fit parameters as physics"
                        .into(),
                );
            }
        }

        let canonical = serde_json::to_string(&value)
            .map_err(|error| format!("failed to canonicalize polymer identity: {error}"))?;
        Ok(ParsedCandidate {
            original: original.to_string(),
            canonical,
        })
    }
}

impl Domain for PolymerDomain {
    fn name(&self) -> &'static str {
        "polymer"
    }

    fn candidate_plural(&self) -> &'static str {
        "polymer candidates"
    }

    fn validate_goal(&self, goal: &CampaignGoal) -> std::result::Result<(), String> {
        if goal.elements.is_empty() {
            Ok(())
        } else {
            Err(
                "polymer campaigns do not use CampaignGoal.elements; encode repeat-unit or monomer identity in seeds"
                    .into(),
            )
        }
    }

    fn apply_goal_implied_constraints(&self, _config: &mut CampaignConfig, _goal: &CampaignGoal) {}

    fn parse_candidate(
        &self,
        candidate: &str,
        _goal: &CampaignGoal,
    ) -> std::result::Result<ParsedCandidate, String> {
        self.validate_structured_identity(candidate)
    }

    fn definition(&self, _config: &CampaignConfig) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "name": "abb_electrical_insulation_targets_v1",
            "target_properties": TARGET_PROPERTIES,
            "source": "ABB pilot brief: electrical-insulation target-property set; this names targets only and does not imply physical thresholds",
        }))
    }

    fn definition_label(&self) -> &'static str {
        "Polymer domain definition"
    }

    fn configured_constraints(&self, config: &CampaignConfig) -> Vec<String> {
        structured_constraint_descriptions(&config.property_constraints)
    }

    fn constraint_violations(
        &self,
        _parsed: &ParsedCandidate,
        properties: &serde_json::Value,
        config: &CampaignConfig,
    ) -> Vec<String> {
        let mut violations = config
            .property_constraints
            .iter()
            .filter(|constraint| !TARGET_PROPERTIES.contains(&constraint.property.as_str()))
            .map(|constraint| {
                format!(
                    "[{}] property '{}' is not part of the ABB electrical-insulation target set",
                    constraint.definition, constraint.property
                )
            })
            .collect::<Vec<_>>();
        let supported = config
            .property_constraints
            .iter()
            .filter(|constraint| TARGET_PROPERTIES.contains(&constraint.property.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        violations.extend(structured_constraint_violations(
            &supported,
            properties,
            EVALUATION_TOOL,
        ));
        violations
    }

    fn evaluator_tool(&self) -> &'static str {
        EVALUATION_TOOL
    }

    fn evaluator_tiers(&self) -> &'static [EvaluatorTier] {
        &POLYMER_EVALUATOR_TIERS
    }

    fn evaluator_inputs(&self, parsed: &ParsedCandidate) -> serde_json::Value {
        serde_json::json!({ "candidate_identity": parsed.canonical, "tier": 0 })
    }

    fn dependency_install_hint(&self) -> Option<&'static str> {
        Some(RDKIT_INSTALL_HINT)
    }

    fn decorate_properties(
        &self,
        properties: &mut serde_json::Value,
        parsed: &ParsedCandidate,
        config: &CampaignConfig,
    ) -> Result<()> {
        let object = properties.as_object_mut().ok_or_else(|| {
            anyhow::anyhow!("{EVALUATION_TOOL} returned a non-object property payload")
        })?;
        object.insert(
            "original_candidate".into(),
            serde_json::Value::String(parsed.original.clone()),
        );
        object.insert(
            "canonical_candidate".into(),
            serde_json::Value::String(parsed.canonical.clone()),
        );
        if let Some(definition) = self.definition(config) {
            object.insert("domain_definition".into(), definition);
        }
        object.insert(
            "evaluator_tiers".into(),
            serde_json::to_value(self.evaluator_tiers())?,
        );
        Ok(())
    }

    fn compute_reward(
        &self,
        goal: &CampaignGoal,
        config: &CampaignConfig,
        properties: &serde_json::Value,
    ) -> Result<f64> {
        if !config.reward_weights.is_empty() {
            return weighted_reward(config, properties, EVALUATION_TOOL);
        }

        let objective = goal.objective.to_ascii_lowercase();
        let property = if objective.contains("glass transition") || objective.contains("tg") {
            "glass_transition_temperature_k"
        } else if objective.contains("breakdown") {
            "dielectric_breakdown_strength_kv_per_mm"
        } else if objective.contains("dielectric") {
            "dielectric_constant"
        } else if objective.contains("thermal conductivity") {
            "thermal_conductivity_w_per_m_k"
        } else {
            bail!(
                "polymer reward requires an objective naming one ABB target property or explicit reward_weights; no generic polymer fallback is scientifically defensible"
            )
        };
        let value = properties
            .get(property)
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| {
                let reason = properties
                    .get("property_status")
                    .and_then(|status| status.get(property))
                    .and_then(|status| status.get("reason"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("the evaluator supplied no citable method or measured value");
                anyhow::anyhow!("{EVALUATION_TOOL} reported '{property}' unavailable: {reason}")
            })?;
        Ok(if objective.contains("minimize") {
            -value
        } else {
            value
        })
    }

    fn summarize_properties(&self, properties: &serde_json::Value) -> String {
        let status = properties
            .get("property_status")
            .and_then(serde_json::Value::as_object);
        let entries = TARGET_PROPERTIES
            .iter()
            .filter_map(|property| {
                if let Some(value) = properties.get(property).and_then(serde_json::Value::as_f64) {
                    let method = status
                        .and_then(|items| items.get(*property))
                        .and_then(|item| item.get("method"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("reported by evaluator");
                    Some(format!("{property}={value:.4} ({method})"))
                } else {
                    status
                        .and_then(|items| items.get(*property))
                        .and_then(|item| item.get("reason"))
                        .and_then(serde_json::Value::as_str)
                        .map(|reason| format!("{property}=unavailable ({reason})"))
                }
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            String::new()
        } else {
            format!("; properties: {}", entries.join(", "))
        }
    }

    fn proposal_system_prompt(&self) -> &'static str {
        "You are proposing polymer candidates for electrical-insulation screening. Respond with ONLY a JSON array of strings. Each string must itself be a JSON object using representation repeat_unit, monomer, or smiles. Do not invent any target-property value or Fox-Flory parameter."
    }

    fn search_space_prompt(&self, _goal: &CampaignGoal) -> String {
        "Candidate identity is not an elemental composition. Use a structured repeat-unit/monomer identity; use SMILES only when the RDKit-backed evaluator is available. Target properties are glass transition temperature, dielectric constant, dielectric breakdown strength, and thermal conductivity. Unsupported properties must remain unavailable.\n".into()
    }

    fn improvement_prompt(&self, batch: usize) -> String {
        format!(
            "\nPropose {batch} NEW polymer candidate identities that improve on these without fabricating property values or method parameters.\n"
        )
    }

    fn initial_prompt(&self, batch: usize) -> String {
        format!(
            "\nPropose {batch} initial polymer candidate identities. Do not include estimated electrical properties.\n"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_unit_and_monomer_are_identities_not_composition_vectors() {
        for candidate in [
            r#"{"representation":"repeat_unit","name":"demo","repeat_unit":"[-CH2-CH2-]"}"#,
            r#"{"representation":"monomer","monomer":"ethylene"}"#,
        ] {
            let parsed = POLYMER_DOMAIN
                .parse_candidate(
                    candidate,
                    &CampaignGoal {
                        description: String::new(),
                        elements: Vec::new(),
                        objective: String::new(),
                        constraints: Vec::new(),
                        seeds: Vec::new(),
                    },
                )
                .unwrap();
            assert!(parsed.canonical.contains("representation"));
        }
    }

    #[test]
    fn fox_flory_parameters_without_a_source_are_rejected() {
        let candidate = r#"{"representation":"repeat_unit","repeat_unit":"[-CH2-CH2-]","fox_flory":{"number_average_molar_mass_g_per_mol":50000,"tg_infinity_k":450,"k_k_g_per_mol":100000}}"#;
        let error = POLYMER_DOMAIN
            .validate_structured_identity(candidate)
            .unwrap_err();
        assert!(error.contains("parameter_citation"));
    }
}
