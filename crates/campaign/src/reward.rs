// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.

//! What "better" means for a campaign, stated so a person never has to guess
//! a scale factor.
//!
//! ## Why this exists
//!
//! The weighted reward multiplied RAW property magnitudes. A refractory
//! campaign was run with `Tm_estimate_K=1` and `delta_S_mix_J_per_molK=100`,
//! which reads as "entropy matters a hundred times more" and actually means
//! the opposite: `Tm ≈ 3200` against `ΔS ≈ 12` makes melting point 72% of the
//! reward. Measured on that run — every step toward more tungsten paid:
//!
//! | candidate | Tm | ΔS | reward |
//! |---|---|---|---|
//! | equimolar, W 20% | 3027.4 | 13.38 | 4365.4 |
//! | W-rich, W 40% | 3233.5 | 12.37 | **4470.5** |
//!
//! Tm gained +206 while entropy lost only −101, so the gradient pointed at
//! tungsten and the loop rode it until the hard HEA constraints stopped it.
//! Entropy weighting had already blocked the DEGENERATE optimum (near-pure W
//! loses to a real HEA, and a test pins that) — but blocking an endpoint does
//! nothing about a gradient.
//!
//! Two changes follow, and they are the same change:
//!
//! 1. **Every term is normalised to a 0–1 desirability before weighting**, so
//!    an importance is a statement about IMPORTANCE and not a guess about
//!    scale. This is the Derringer–Suich desirability formulation, and the
//!    domain's own default heuristic already worked this way
//!    (`entropy / 2.0`, `1 - density / 20.0`); only the operator-facing path
//!    did not, which is what made the flags a footgun.
//!
//! 2. **A term may state a TARGET, not only a direction.** A maximised
//!    monotonic property has its optimum at a corner of the design space —
//!    that is a property of linear functions, not a bug — so "as high as
//!    possible" always ends up at a pure element or a constraint boundary. A
//!    target has an interior optimum, which is what makes a requirement like
//!    "yield ≥ 900 MPa, density ≤ 9 g/cm³" expressible at all.
//!
//! Scores are normalised against DECLARED anchors rather than the batch:
//! rewards are persisted per candidate and compared across iterations, so a
//! batch-relative score would make iteration 3 incomparable with iteration 1.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What counts as good for one property.
///
/// Every variant carries the anchors that make its score dimensionless, so
/// the unit never leaks into the arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "goal", rename_all = "snake_case")]
pub enum Aim {
    /// Higher is better. `poor` scores 0, `good` scores 1, between is linear,
    /// beyond `good` stays 1 — a property cannot earn unbounded credit for
    /// running away, which is precisely how the corner-seeking happened.
    Maximize { poor: f64, good: f64 },
    /// Lower is better, with the same clamping.
    Minimize { poor: f64, good: f64 },
    /// On-target is better. Scores 1 at `value` and falls to 0 at
    /// `± tolerance`. This is the variant a requirement uses.
    Target { value: f64, tolerance: f64 },
}

impl Aim {
    /// Map a measured value onto 0–1. Never NaN, never outside the range: a
    /// reward that can go infinite or undefined ranks candidates by accident.
    #[must_use]
    pub fn desirability(self, value: f64) -> f64 {
        if !value.is_finite() {
            return 0.0;
        }
        let score = match self {
            Self::Maximize { poor, good } => {
                if (good - poor).abs() < f64::EPSILON {
                    return f64::from(u8::from(value >= good));
                }
                (value - poor) / (good - poor)
            }
            Self::Minimize { poor, good } => {
                if (poor - good).abs() < f64::EPSILON {
                    return f64::from(u8::from(value <= good));
                }
                (poor - value) / (poor - good)
            }
            Self::Target {
                value: want,
                tolerance,
            } => {
                if tolerance <= 0.0 {
                    return f64::from(u8::from((value - want).abs() < f64::EPSILON));
                }
                1.0 - ((value - want).abs() / tolerance)
            }
        };
        // inf/inf is NaN, and anchors like poor=-1e308, good=1e308 are finite
        // enough to pass validation while producing exactly that. Production
        // ranking sorts with `partial_cmp(..).unwrap_or(Equal)`, so a NaN
        // reward does not panic — it silently floats to an arbitrary rank,
        // which is worse.
        if !score.is_finite() {
            return 0.0;
        }
        score.clamp(0.0, 1.0)
    }
}

/// One property and how much it matters relative to the others.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RewardTerm {
    /// The evaluator's own key, e.g. `Tm_estimate_K`. Never English prose:
    /// selection is by declaration, never by substring-matching an objective.
    pub property: String,
    pub aim: Aim,
    /// Relative importance. Only ratios matter — the total is normalised — so
    /// 2 against 1 means twice as important, with no scale to guess.
    pub importance: f64,
    /// Why this term is here, in one line, for the operator who reads back
    /// what the campaign decided to optimise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// The whole objective: what to optimise and how much each part counts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RewardSpec {
    pub terms: Vec<RewardTerm>,
    /// Set when the spec was DERIVED from the goal rather than supplied, so
    /// the provenance of an objective is never guesswork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_by: Option<String>,
}

/// Why a spec could not score a candidate. Missing properties are named, not
/// defaulted: a reward that silently treats an absent descriptor as zero
/// invents physics, and ranks candidates on the invention.
#[derive(Debug, Clone, PartialEq)]
pub enum RewardError {
    NoTerms,
    MissingProperty(String),
    NoImportance,
    /// Anchors that would invert or flatten the objective — reversed
    /// poor/good, a non-positive tolerance, a non-finite bound.
    InvalidAnchors(String),
    /// The same property twice: the importances would sum and the property
    /// would be silently double-weighted.
    DuplicateProperty(String),
}

impl std::fmt::Display for RewardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTerms => write!(f, "the reward specification has no terms"),
            Self::MissingProperty(property) => write!(
                f,
                "the evaluator returned no numeric value for '{property}', which the objective ranks on"
            ),
            Self::NoImportance => {
                write!(
                    f,
                    "every term has zero importance, so nothing is being optimised"
                )
            }
            Self::DuplicateProperty(property) => write!(
                f,
                "'{property}' appears twice; the importances would sum and double-weight it"
            ),
            Self::InvalidAnchors(property) => write!(
                f,
                "'{property}' has anchors that would invert or flatten the objective \
                 (reversed poor/good, a non-positive tolerance, or a non-finite bound)"
            ),
        }
    }
}

impl std::error::Error for RewardError {}

impl RewardSpec {
    /// Score one candidate in 0–1, where 1 satisfies every term.
    ///
    /// The result is comparable ACROSS iterations because every anchor is
    /// declared rather than taken from the batch.
    pub fn score(&self, properties: &serde_json::Value) -> Result<f64, RewardError> {
        if self.terms.is_empty() {
            return Err(RewardError::NoTerms);
        }
        let mut weighted = 0.0;
        let mut total_importance = 0.0;
        for term in &self.terms {
            let value = properties
                .get(&term.property)
                .and_then(serde_json::Value::as_f64)
                .ok_or_else(|| RewardError::MissingProperty(term.property.clone()))?;
            let importance = term.importance.max(0.0);
            weighted += term.aim.desirability(value) * importance;
            total_importance += importance;
        }
        if total_importance <= 0.0 {
            return Err(RewardError::NoImportance);
        }
        Ok(weighted / total_importance)
    }

    /// Refuse a spec whose anchors would misrank, wherever it came from.
    ///
    /// `parse_derived_spec` already refuses these at the door, but a spec
    /// that arrives by DESERIALIZATION — a checkpoint, a config file, an API
    /// payload — skips that path entirely. A persisted
    /// `{"goal":"maximize","poor":3500,"good":2000}` loads without complaint
    /// and silently inverts the objective for the rest of the campaign.
    pub fn validate(&self) -> Result<(), RewardError> {
        if self.terms.is_empty() {
            return Err(RewardError::NoTerms);
        }
        for term in &self.terms {
            let bad = match term.aim {
                Aim::Maximize { poor, good } => {
                    !poor.is_finite() || !good.is_finite() || good <= poor
                }
                Aim::Minimize { poor, good } => {
                    !poor.is_finite() || !good.is_finite() || good >= poor
                }
                Aim::Target { value, tolerance } => {
                    !value.is_finite() || !tolerance.is_finite() || tolerance <= 0.0
                }
            };
            if bad {
                return Err(RewardError::InvalidAnchors(term.property.clone()));
            }
            if !term.importance.is_finite() || term.importance < 0.0 {
                return Err(RewardError::InvalidAnchors(term.property.clone()));
            }
            // Duplicates too. `parse_derived_spec` refuses them, but a spec
            // that arrives by DESERIALIZATION skips that path — which is the
            // whole reason this function exists — and two terms on one
            // property SUM their importances, silently double-weighting it
            // with neither stated aim winning.
            if self
                .terms
                .iter()
                .filter(|other| other.property == term.property)
                .count()
                > 1
            {
                return Err(RewardError::DuplicateProperty(term.property.clone()));
            }
        }
        if self.terms.iter().all(|term| term.importance <= 0.0) {
            return Err(RewardError::NoImportance);
        }
        Ok(())
    }

    /// Per-term desirabilities, for showing an operator WHY a candidate
    /// scored what it scored. A single scalar cannot be argued with.
    #[must_use]
    pub fn breakdown(&self, properties: &serde_json::Value) -> BTreeMap<String, f64> {
        self.terms
            .iter()
            .filter_map(|term| {
                properties
                    .get(&term.property)
                    .and_then(serde_json::Value::as_f64)
                    .map(|value| (term.property.clone(), term.aim.desirability(value)))
            })
            .collect()
    }

    /// One human-readable line per term, printed when a campaign starts so
    /// the objective is visible before compute is spent on it.
    #[must_use]
    pub fn describe(&self) -> Vec<String> {
        let total: f64 = self.terms.iter().map(|t| t.importance.max(0.0)).sum();
        self.terms
            .iter()
            .map(|term| {
                let share = if total > 0.0 {
                    term.importance.max(0.0) / total * 100.0
                } else {
                    0.0
                };
                let aim = match term.aim {
                    Aim::Maximize { poor, good } => format!("maximise ({poor} poor → {good} good)"),
                    Aim::Minimize { poor, good } => format!("minimise ({poor} poor → {good} good)"),
                    Aim::Target { value, tolerance } => format!("target {value} ± {tolerance}"),
                };
                let why = term
                    .rationale
                    .as_deref()
                    .map(|r| format!(" — {r}"))
                    .unwrap_or_default();
                format!("{:<28} {aim}  [{share:.0}%]{why}", term.property)
            })
            .collect()
    }
}

/// Ask a model to state the objective, given the goal and the properties the
/// evaluator actually reports.
///
/// The registry is passed as `name: unit` pairs and the model is told to use
/// those keys VERBATIM, because reward-property selection is by declaration:
/// an objective matched by English substring ("maximize melting point") never
/// fires for a differently-worded or non-English goal, and that has already
/// been fixed once here.
///
/// The prompt asks for anchors and targets, not weights, because anchors are
/// answerable from domain knowledge ("3500 K is an excellent refractory
/// melting point") while a weight is only meaningful relative to a scale the
/// model cannot see.
#[must_use]
pub fn derivation_prompt(goal: &str, objective: &str, registry: &[(&str, &str)]) -> String {
    let properties = registry
        .iter()
        .map(|(name, unit)| format!("  - {name} (in {unit})"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "A materials campaign has this goal:\n  {goal}\n\nStated objective:\n  {objective}\n\n         The evaluator reports exactly these properties:\n{properties}\n\n         State what \"better\" means, as JSON:\n         {{\"terms\": [{{\"property\": \"<one key from the list, verbatim>\", \
         \"goal\": \"maximize\"|\"minimize\"|\"target\", \
         \"poor\": <value scoring 0>, \"good\": <value scoring 1>, \
         \"value\": <target>, \"tolerance\": <spread scoring 0>, \
         \"importance\": <relative, only ratios matter>, \
         \"rationale\": \"<one line>\"}}]}}\n\n         Use poor/good for maximize and minimize; value/tolerance for target. \
         Prefer a TARGET whenever the goal implies a requirement rather than \
         an extreme, because a maximised property is optimised by a pure \
         element or a constraint boundary rather than by a good alloy. \
         Anchors must be realistic values for this material class. \
         Return only the JSON object."
    )
}

/// Read a derived spec, rejecting anything that would silently misrank.
///
/// Validation is not politeness here: an unknown property key ranks nothing,
/// a reversed anchor pair inverts the objective, and a non-finite anchor
/// makes every score NaN. Each is refused by name rather than repaired,
/// because a quietly repaired objective is one nobody reviewed.
pub fn parse_derived_spec(
    raw: &str,
    allowed: &[&str],
    derived_by: &str,
) -> Result<RewardSpec, String> {
    let start = raw.find('{').ok_or("no JSON object in the model's reply")?;
    let end = raw
        .rfind('}')
        .ok_or("no JSON object in the model's reply")?;
    let value: serde_json::Value =
        serde_json::from_str(&raw[start..=end]).map_err(|e| format!("unparsable JSON: {e}"))?;
    let items = value
        .get("terms")
        .and_then(serde_json::Value::as_array)
        .ok_or("the reply has no `terms` array")?;

    let mut terms = Vec::new();
    for item in items {
        let property = item
            .get("property")
            .and_then(serde_json::Value::as_str)
            .ok_or("a term has no `property`")?;
        if !allowed.contains(&property) {
            return Err(format!(
                "'{property}' is not a property the evaluator reports; it would rank nothing"
            ));
        }
        let number = |key: &str| item.get(key).and_then(serde_json::Value::as_f64);
        let aim = match item.get("goal").and_then(serde_json::Value::as_str) {
            Some("maximize") => {
                let (poor, good) = (
                    number("poor").ok_or("maximize needs `poor`")?,
                    number("good").ok_or("maximize needs `good`")?,
                );
                if !(poor.is_finite() && good.is_finite()) || good <= poor {
                    return Err(format!(
                        "'{property}': maximize needs good > poor, got poor={poor} good={good}"
                    ));
                }
                Aim::Maximize { poor, good }
            }
            Some("minimize") => {
                let (poor, good) = (
                    number("poor").ok_or("minimize needs `poor`")?,
                    number("good").ok_or("minimize needs `good`")?,
                );
                if !(poor.is_finite() && good.is_finite()) || good >= poor {
                    return Err(format!(
                        "'{property}': minimize needs good < poor, got poor={poor} good={good}"
                    ));
                }
                Aim::Minimize { poor, good }
            }
            Some("target") => {
                let value = number("value").ok_or("target needs `value`")?;
                let tolerance = number("tolerance").ok_or("target needs `tolerance`")?;
                if !(value.is_finite() && tolerance.is_finite()) || tolerance <= 0.0 {
                    return Err(format!("'{property}': target needs a positive tolerance"));
                }
                Aim::Target { value, tolerance }
            }
            other => return Err(format!("'{property}': unknown goal {other:?}")),
        };
        // A MISSING importance defaults to 1 — every term equally weighted is
        // a defensible reading of silence. A PRESENT but unreadable one
        // ("high") does not: silently calling it 1 alongside numeric weights
        // of 100 near-ignores the term, and this codebase does not gap-fill a
        // value that was stated.
        let importance = match item.get("importance") {
            None | Some(serde_json::Value::Null) => 1.0,
            Some(value) => value
                .as_f64()
                .ok_or_else(|| format!("'{property}': importance must be a number, got {value}"))?,
        };
        if !importance.is_finite() || importance < 0.0 {
            return Err(format!("'{property}': importance must be finite and >= 0"));
        }
        if terms
            .iter()
            .any(|existing: &RewardTerm| existing.property == property)
        {
            // Both copies are kept and their importances SUM, so the property
            // is silently double-weighted for the whole campaign and neither
            // stated aim wins.
            return Err(format!(
                "'{property}' appears twice; it would be double-weighted"
            ));
        }
        terms.push(RewardTerm {
            property: property.to_string(),
            aim,
            importance,
            rationale: item
                .get("rationale")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        });
    }
    if terms.is_empty() {
        return Err("the reply declared no terms".to_string());
    }
    if terms.iter().all(|term| term.importance <= 0.0) {
        // Otherwise this is refused later, once per candidate, by
        // `RewardError::NoImportance` — after compute has been spent.
        return Err("every term has zero importance, so nothing would be optimised".to_string());
    }
    Ok(RewardSpec {
        terms,
        derived_by: Some(derived_by.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(terms: Vec<RewardTerm>) -> RewardSpec {
        RewardSpec {
            terms,
            derived_by: None,
        }
    }

    fn term(property: &str, aim: Aim, importance: f64) -> RewardTerm {
        RewardTerm {
            property: property.to_string(),
            aim,
            importance,
            rationale: None,
        }
    }

    /// THE REGRESSION. On the real campaign the W-rich candidate outscored
    /// the equimolar one because raw magnitudes were weighted: Tm gained +206
    /// while entropy lost only −101. Normalised, an importance means
    /// importance, and entropy weighted above melting point actually wins.
    #[test]
    fn normalising_stops_the_tungsten_gradient() {
        let objective = spec(vec![
            term(
                "Tm_estimate_K",
                Aim::Maximize {
                    poor: 2000.0,
                    good: 3500.0,
                },
                1.0,
            ),
            term(
                "delta_S_mix_J_per_molK",
                Aim::Maximize {
                    poor: 8.314,
                    good: 14.0,
                },
                3.0,
            ),
        ]);
        let equimolar = json!({"Tm_estimate_K": 3027.4, "delta_S_mix_J_per_molK": 13.38});
        let tungsten_rich = json!({"Tm_estimate_K": 3233.5, "delta_S_mix_J_per_molK": 12.37});

        let equimolar_score = objective.score(&equimolar).unwrap();
        let tungsten_score = objective.score(&tungsten_rich).unwrap();
        assert!(
            equimolar_score > tungsten_score,
            "entropy weighted 3x must beat a melting-point gain: \
             equimolar {equimolar_score:.4} vs W-rich {tungsten_score:.4}"
        );
    }

    /// The raw-magnitude reward the campaign actually ran, kept as the
    /// counter-example: with the SAME intent expressed as raw weights, the
    /// tungsten-rich candidate wins. This is the bug, pinned.
    #[test]
    fn the_raw_weighted_reward_preferred_more_tungsten() {
        let raw = |tm: f64, ds: f64| tm * 1.0 + ds * 100.0;
        assert!(
            raw(3233.5, 12.37) > raw(3027.4, 13.38),
            "the historical reward really did climb toward tungsten"
        );
    }

    /// A target has an INTERIOR optimum, which is the whole reason it can
    /// express a requirement. Running away from the target must lose, in both
    /// directions — a maximised property cannot do this.
    #[test]
    fn a_target_is_beaten_by_neither_extreme() {
        let objective = spec(vec![term(
            "Tm_estimate_K",
            Aim::Target {
                value: 3000.0,
                tolerance: 500.0,
            },
            1.0,
        )]);
        let on = objective.score(&json!({"Tm_estimate_K": 3000.0})).unwrap();
        let over = objective.score(&json!({"Tm_estimate_K": 3600.0})).unwrap();
        let under = objective.score(&json!({"Tm_estimate_K": 2400.0})).unwrap();
        assert!((on - 1.0).abs() < 1e-9, "on target scores 1: {on}");
        assert!(on > over && on > under, "both extremes must lose");
        assert_eq!(over, 0.0, "beyond tolerance scores 0, never negative");
    }

    /// Desirability is bounded, so no single term can dominate by running
    /// away. Unbounded credit for an extreme value IS the corner-seeking.
    #[test]
    fn desirability_is_clamped_and_never_nan() {
        let aim = Aim::Maximize {
            poor: 0.0,
            good: 10.0,
        };
        assert_eq!(aim.desirability(-50.0), 0.0);
        assert_eq!(aim.desirability(1e9), 1.0);
        assert_eq!(aim.desirability(f64::NAN), 0.0);
        assert_eq!(aim.desirability(f64::INFINITY), 0.0);
        let degenerate = Aim::Target {
            value: 1.0,
            tolerance: 0.0,
        };
        assert_eq!(degenerate.desirability(1.0), 1.0);
        assert_eq!(degenerate.desirability(1.5), 0.0);
    }

    /// A missing descriptor is NAMED, never defaulted to zero: a reward that
    /// silently invents a value ranks candidates on the invention.
    #[test]
    fn a_missing_property_is_named_not_defaulted() {
        let objective = spec(vec![term(
            "density",
            Aim::Minimize {
                poor: 20.0,
                good: 5.0,
            },
            1.0,
        )]);
        assert_eq!(
            objective.score(&json!({"Tm_estimate_K": 3000.0})),
            Err(RewardError::MissingProperty("density".to_string()))
        );
        assert_eq!(spec(vec![]).score(&json!({})), Err(RewardError::NoTerms));
    }

    /// Only RATIOS of importance matter, so an operator who writes 2 and 1
    /// gets the same ranking as one who writes 200 and 100. Without this the
    /// weights are a scale guess again, which is the bug.
    #[test]
    fn only_the_ratio_of_importance_matters() {
        let small = spec(vec![
            term(
                "a",
                Aim::Maximize {
                    poor: 0.0,
                    good: 1.0,
                },
                2.0,
            ),
            term(
                "b",
                Aim::Maximize {
                    poor: 0.0,
                    good: 1.0,
                },
                1.0,
            ),
        ]);
        let large = spec(vec![
            term(
                "a",
                Aim::Maximize {
                    poor: 0.0,
                    good: 1.0,
                },
                200.0,
            ),
            term(
                "b",
                Aim::Maximize {
                    poor: 0.0,
                    good: 1.0,
                },
                100.0,
            ),
        ]);
        let props = json!({"a": 0.9, "b": 0.1});
        let small_score = small.score(&props).unwrap();
        assert!((small_score - large.score(&props).unwrap()).abs() < 1e-12);
        // And the score stays in 0-1, so it is comparable across iterations.
        assert!((0.0..=1.0).contains(&small_score));
    }

    const ALLOWED: [&str; 3] = ["Tm_estimate_K", "delta_S_mix_J_per_molK", "density"];

    /// The happy path: a goal in English becomes a normalised objective with
    /// no human arithmetic anywhere.
    #[test]
    fn a_derived_spec_reads_targets_and_directions() {
        let raw = r#"Here you go:
        {"terms": [
          {"property": "Tm_estimate_K", "goal": "target", "value": 3000, "tolerance": 400,
           "importance": 2, "rationale": "service temperature, not a maximum"},
          {"property": "delta_S_mix_J_per_molK", "goal": "maximize", "poor": 8.314, "good": 14,
           "importance": 1}
        ]}"#;
        let spec = parse_derived_spec(raw, &ALLOWED, "model:test").unwrap();
        assert_eq!(spec.terms.len(), 2);
        assert_eq!(spec.derived_by.as_deref(), Some("model:test"));
        assert_eq!(
            spec.terms[0].aim,
            Aim::Target {
                value: 3000.0,
                tolerance: 400.0
            }
        );
        assert_eq!(
            spec.terms[0].rationale.as_deref(),
            Some("service temperature, not a maximum")
        );
        // And it scores without anyone choosing a scale factor.
        let score = spec
            .score(&json!({"Tm_estimate_K": 3000.0, "delta_S_mix_J_per_molK": 14.0}))
            .unwrap();
        assert!((score - 1.0).abs() < 1e-9, "both terms satisfied: {score}");
    }

    /// A property the evaluator does not report would rank NOTHING, so it is
    /// refused by name rather than dropped — a silently shortened objective is
    /// one nobody agreed to.
    #[test]
    fn a_hallucinated_property_is_refused_by_name() {
        let raw = r#"{"terms":[{"property":"yield_strength_MPa","goal":"maximize",
                     "poor":0,"good":900,"importance":1}]}"#;
        let error = parse_derived_spec(raw, &ALLOWED, "model:test").unwrap_err();
        assert!(error.contains("yield_strength_MPa"), "{error}");
        assert!(error.contains("would rank nothing"), "{error}");
    }

    /// Reversed anchors INVERT the objective — a maximise that scores 1 at the
    /// low end optimises for the opposite of what was asked, silently.
    #[test]
    fn reversed_anchors_are_refused_in_both_directions() {
        let backwards_max = r#"{"terms":[{"property":"Tm_estimate_K","goal":"maximize",
                                "poor":3500,"good":2000,"importance":1}]}"#;
        assert!(
            parse_derived_spec(backwards_max, &ALLOWED, "m")
                .unwrap_err()
                .contains("good > poor")
        );
        let backwards_min = r#"{"terms":[{"property":"density","goal":"minimize",
                                "poor":5,"good":20,"importance":1}]}"#;
        assert!(
            parse_derived_spec(backwards_min, &ALLOWED, "m")
                .unwrap_err()
                .contains("good < poor")
        );
        let zero_tolerance = r#"{"terms":[{"property":"density","goal":"target",
                                 "value":9,"tolerance":0,"importance":1}]}"#;
        assert!(
            parse_derived_spec(zero_tolerance, &ALLOWED, "m")
                .unwrap_err()
                .contains("positive tolerance")
        );
    }

    /// Nothing usable must not become an empty objective that ranks every
    /// candidate identically.
    #[test]
    fn an_empty_or_unparsable_reply_is_an_error() {
        assert!(parse_derived_spec("no json here", &ALLOWED, "m").is_err());
        assert!(parse_derived_spec(r#"{"terms":[]}"#, &ALLOWED, "m").is_err());
        assert!(parse_derived_spec(r#"{"nope":1}"#, &ALLOWED, "m").is_err());
    }

    /// The prompt must hand over the evaluator's OWN keys and units, and must
    /// push toward targets — a maximised property is optimised by a pure
    /// element, which is the failure this whole module exists for.
    #[test]
    fn the_derivation_prompt_states_the_keys_and_prefers_targets() {
        let prompt = derivation_prompt(
            "A refractory alloy for LPBF",
            "resist solidification cracking",
            &[("Tm_estimate_K", "K"), ("density", "g/cm3")],
        );
        assert!(prompt.contains("Tm_estimate_K (in K)"), "{prompt}");
        assert!(prompt.contains("density (in g/cm3)"), "{prompt}");
        assert!(prompt.contains("verbatim"), "keys must not be paraphrased");
        assert!(prompt.contains("Prefer a TARGET"), "{prompt}");
        assert!(
            prompt.contains("constraint boundary"),
            "the reason is stated"
        );
    }

    /// Adversarial model output, from a review of the first version. Each
    /// of these previously produced a spec that RANKED CANDIDATES WRONGLY
    /// rather than failing.
    #[test]
    fn a_spec_that_would_silently_misrank_is_refused() {
        // Both copies survive and their importances SUM, so the property is
        // double-weighted and neither stated aim wins.
        let duplicate = r#"{"terms":[
            {"property":"Tm_estimate_K","goal":"target","value":3000,"tolerance":400,"importance":1},
            {"property":"Tm_estimate_K","goal":"maximize","poor":2000,"good":3500,"importance":1}]}"#;
        assert!(
            parse_derived_spec(duplicate, &ALLOWED, "m")
                .unwrap_err()
                .contains("twice"),
        );

        // Present but unreadable. Defaulting it to 1 beside numeric weights
        // of 100 near-ignores the term, silently.
        let worded = r#"{"terms":[{"property":"density","goal":"minimize",
                        "poor":20,"good":5,"importance":"high"}]}"#;
        assert!(
            parse_derived_spec(worded, &ALLOWED, "m")
                .unwrap_err()
                .contains("must be a number"),
        );
        // A MISSING importance is different: silence means "weight them
        // equally", which is a real reading.
        let absent = r#"{"terms":[{"property":"density","goal":"minimize","poor":20,"good":5}]}"#;
        assert_eq!(
            parse_derived_spec(absent, &ALLOWED, "m").unwrap().terms[0].importance,
            1.0
        );

        // Optimising nothing must fail at the door, not once per candidate
        // after compute has been spent.
        let nothing = r#"{"terms":[{"property":"density","goal":"minimize",
                         "poor":20,"good":5,"importance":0}]}"#;
        assert!(
            parse_derived_spec(nothing, &ALLOWED, "m")
                .unwrap_err()
                .contains("zero importance"),
        );
    }

    /// Anchors wide enough to overflow the ratio produce NaN, and production
    /// ranking sorts with `partial_cmp(..).unwrap_or(Equal)` — so a NaN does
    /// not panic, it floats to an arbitrary rank. Worse than a crash.
    #[test]
    fn extreme_anchors_score_zero_rather_than_nan() {
        let aim = Aim::Maximize {
            poor: -1e308,
            good: 1e308,
        };
        let score = aim.desirability(1e308);
        assert!(score.is_finite(), "desirability must never be NaN: {score}");
        assert!((0.0..=1.0).contains(&score));
    }

    /// A spec that arrives by DESERIALIZATION skips `parse_derived_spec`
    /// entirely. A persisted checkpoint with reversed anchors used to load
    /// without complaint and invert the objective for the whole campaign.
    #[test]
    fn a_deserialized_spec_with_reversed_anchors_is_refused() {
        let inverted: RewardSpec = serde_json::from_str(
            r#"{"terms":[{"property":"Tm_estimate_K","aim":{"goal":"maximize",
                "poor":3500,"good":2000},"importance":1}]}"#,
        )
        .expect("it deserializes — that is the problem");
        // It scores happily, ranking a COLD alloy above a hot one.
        let cold = inverted.score(&json!({"Tm_estimate_K": 2000.0})).unwrap();
        let hot = inverted.score(&json!({"Tm_estimate_K": 3500.0})).unwrap();
        assert!(
            cold > hot,
            "the inversion is real: cold {cold} vs hot {hot}"
        );
        // So it must be refused before it is ever scored.
        assert_eq!(
            inverted.validate(),
            Err(RewardError::InvalidAnchors("Tm_estimate_K".to_string()))
        );

        let sane = spec(vec![term(
            "Tm_estimate_K",
            Aim::Maximize {
                poor: 2000.0,
                good: 3500.0,
            },
            1.0,
        )]);
        assert_eq!(sane.validate(), Ok(()));
    }

    /// A DESERIALIZED spec with the same property twice double-weights it —
    /// the importances sum and neither stated aim wins. `parse_derived_spec`
    /// refuses this; `validate()` is the path a checkpoint or config takes,
    /// and it must refuse it too.
    #[test]
    fn a_deserialized_spec_with_a_duplicate_property_is_refused() {
        let doubled: RewardSpec = serde_json::from_str(
            r#"{"terms":[
                {"property":"Tm_estimate_K","aim":{"goal":"target","value":3000,"tolerance":400},
                 "importance":1},
                {"property":"Tm_estimate_K","aim":{"goal":"maximize","poor":2000,"good":3500},
                 "importance":1}]}"#,
        )
        .expect("it deserializes — that is the problem");
        // It scores happily, counting one property twice.
        assert!(doubled.score(&json!({"Tm_estimate_K": 3000.0})).is_ok());
        assert_eq!(
            doubled.validate(),
            Err(RewardError::DuplicateProperty("Tm_estimate_K".to_string()))
        );
    }

    /// The objective must be legible before compute is spent on it.
    #[test]
    fn the_spec_describes_itself_with_shares() {
        let objective = spec(vec![
            term(
                "Tm_estimate_K",
                Aim::Maximize {
                    poor: 2000.0,
                    good: 3500.0,
                },
                1.0,
            ),
            term(
                "density",
                Aim::Minimize {
                    poor: 20.0,
                    good: 5.0,
                },
                3.0,
            ),
        ]);
        let lines = objective.describe();
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].contains("Tm_estimate_K") && lines[0].contains("25%"),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("density") && lines[1].contains("75%"),
            "{lines:?}"
        );
    }
}
