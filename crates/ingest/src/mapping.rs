//! Ontology mapping rules — user-provided YAML that customizes how the LLM
//! extraction pipeline maps raw data to graph entities and relationships.
//!
//! Example `mappings/materials.yaml`:
//! ```yaml
//! entity_rules:
//!   - column_pattern: "Composition|Alloy|Material"
//!     entity_type: Alloy
//!   - column_pattern: "_at%|_wt%|fraction"
//!     entity_type: Element
//!     relationship: CONTAINS
//!     weight_column: true
//!   - column_pattern: "Hardness|Strength|Modulus|Density"
//!     entity_type: Property
//!     relationship: HAS_PROPERTY
//!   - column_pattern: "Phase|Structure|Crystal"
//!     entity_type: Phase
//!     relationship: OBSERVED_IN
//!   - column_pattern: "Process|Treatment|Anneal|Sinter"
//!     entity_type: Process
//!     relationship: PROCESSED_BY
//!
//! aliases:
//!   Nb: Niobium
//!   Mo: Molybdenum
//!   Ta: Tantalum
//!   W: Tungsten
//!   HV: Vickers Hardness
//!   BCC: Body-Centered Cubic
//!
//! ignore_columns:
//!   - "id"
//!   - "row_number"
//!   - "notes"
//! ```

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// User-provided ontology mapping rules.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OntologyMapping {
    /// Rules for mapping columns → entity types.
    #[serde(default)]
    pub entity_rules: Vec<EntityRule>,
    /// Alias expansions (abbreviation → full name).
    #[serde(default)]
    pub aliases: std::collections::HashMap<String, String>,
    /// Columns to exclude from extraction.
    #[serde(default)]
    pub ignore_columns: Vec<String>,
}

/// A single column → entity mapping rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityRule {
    /// Regex pattern matched against column names (case-insensitive).
    pub column_pattern: String,
    /// The entity type to assign (Alloy, Element, Property, Phase, Process).
    pub entity_type: String,
    /// Relationship type to create from the material to this entity.
    #[serde(default)]
    pub relationship: Option<String>,
    /// Whether this column contains weight/fraction values.
    #[serde(default)]
    pub weight_column: bool,
}

impl OntologyMapping {
    /// Load mapping rules from a YAML file.
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read mapping file {}", path.display()))?;
        let mapping: Self = serde_yaml::from_str(&text)
            .with_context(|| format!("failed to parse mapping YAML from {}", path.display()))?;
        tracing::info!(
            rules = mapping.entity_rules.len(),
            aliases = mapping.aliases.len(),
            ignored = mapping.ignore_columns.len(),
            "loaded ontology mapping from {}",
            path.display()
        );
        Ok(mapping)
    }

    /// Check if a column name should be ignored.
    pub fn should_ignore(&self, column: &str) -> bool {
        let lower = column.to_lowercase();
        self.ignore_columns
            .iter()
            .any(|ic| lower == ic.to_lowercase())
    }

    /// Find the matching entity rule for a column name. Returns None if no rule matches.
    pub fn match_column(&self, column: &str) -> Option<&EntityRule> {
        let lower = column.to_lowercase();
        self.entity_rules.iter().find(|rule| {
            rule.column_pattern
                .split('|')
                .any(|pat| lower.contains(&pat.to_lowercase()))
        })
    }

    /// Expand aliases in an entity name.
    pub fn expand_alias(&self, name: &str) -> String {
        self.aliases
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// Generate a prompt supplement describing the mapping rules to the LLM.
    pub fn to_prompt_supplement(&self) -> String {
        if self.entity_rules.is_empty() && self.aliases.is_empty() {
            return String::new();
        }

        let mut prompt = String::from("\n## Custom Mapping Rules\n");

        if !self.entity_rules.is_empty() {
            prompt.push_str("Column mapping hints:\n");
            for rule in &self.entity_rules {
                prompt.push_str(&format!(
                    "- Columns matching '{}' → entity type '{}'\n",
                    rule.column_pattern, rule.entity_type
                ));
                if let Some(ref rel) = rule.relationship {
                    prompt.push_str(&format!("  → relationship type: {rel}\n"));
                }
                if rule.weight_column {
                    prompt.push_str("  → this column contains weight/fraction values\n");
                }
            }
        }

        if !self.aliases.is_empty() {
            prompt.push_str("\nKnown aliases:\n");
            for (abbr, full) in &self.aliases {
                prompt.push_str(&format!("- {abbr} = {full}\n"));
            }
        }

        if !self.ignore_columns.is_empty() {
            prompt.push_str(&format!(
                "\nIgnore these columns: {}\n",
                self.ignore_columns.join(", ")
            ));
        }

        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mapping_yaml() {
        let yaml = r#"
entity_rules:
  - column_pattern: "Composition|Alloy"
    entity_type: Alloy
  - column_pattern: "_at%|_wt%"
    entity_type: Element
    relationship: CONTAINS
    weight_column: true

aliases:
  Nb: Niobium
  Mo: Molybdenum

ignore_columns:
  - id
  - notes
"#;
        let mapping: OntologyMapping = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(mapping.entity_rules.len(), 2);
        assert_eq!(mapping.aliases.len(), 2);
        assert_eq!(mapping.ignore_columns.len(), 2);
    }

    #[test]
    fn match_column_works() {
        let yaml = r#"
entity_rules:
  - column_pattern: "Hardness|Strength"
    entity_type: Property
    relationship: HAS_PROPERTY
"#;
        let mapping: OntologyMapping = serde_yaml::from_str(yaml).unwrap();
        assert!(mapping.match_column("Hardness_HV").is_some());
        assert!(mapping.match_column("Tensile_Strength").is_some());
        assert!(mapping.match_column("Composition").is_none());
    }

    #[test]
    fn should_ignore_case_insensitive() {
        let mapping = OntologyMapping {
            ignore_columns: vec!["ID".into(), "Notes".into()],
            ..Default::default()
        };
        assert!(mapping.should_ignore("id"));
        assert!(mapping.should_ignore("notes"));
        assert!(!mapping.should_ignore("Hardness"));
    }

    #[test]
    fn expand_alias() {
        let mut mapping = OntologyMapping::default();
        mapping.aliases.insert("BCC".into(), "Body-Centered Cubic".into());
        assert_eq!(mapping.expand_alias("BCC"), "Body-Centered Cubic");
        assert_eq!(mapping.expand_alias("FCC"), "FCC"); // no alias = passthrough
    }

    #[test]
    fn prompt_supplement_empty_for_no_rules() {
        let mapping = OntologyMapping::default();
        assert!(mapping.to_prompt_supplement().is_empty());
    }

    #[test]
    fn prompt_supplement_includes_rules() {
        let yaml = r#"
entity_rules:
  - column_pattern: "Hardness"
    entity_type: Property
aliases:
  Nb: Niobium
ignore_columns:
  - id
"#;
        let mapping: OntologyMapping = serde_yaml::from_str(yaml).unwrap();
        let prompt = mapping.to_prompt_supplement();
        assert!(prompt.contains("Hardness"));
        assert!(prompt.contains("Niobium"));
        assert!(prompt.contains("id"));
    }
}
