// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Separate inference-context influence index for experimental J-space routing.
//!
//! This module deliberately shares no storage, vectors, distance function, or
//! cache key with [`crate::capability`]. Cosine similarity answers which tool
//! description resembles a query. Influence answers how much inserting the
//! exact full tool definition changes the target model's next-token
//! distribution. Treating those scores as one metric would make neither index
//! truthful.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::{Arc, OnceLock, RwLock};

use prism_llm::ToolDefinition;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const ALGORITHM_VERSION: &str = "prefill_js_per_added_token_v1";

/// V1 tolerance in normalized natural-log units per added prompt token.
/// Scores at or below this scale carry no measured signal; top-score gaps at
/// or below it are ties. Changing this policy requires an algorithm-version
/// bump so an index identity never silently changes its discrimination rule.
pub(crate) const V1_SCORE_DISCRIMINATION_TOLERANCE: f64 = 1.0e-12;

/// Retrieval method actually used for one LLM request.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSelectionMethod {
    Keyword,
    Cosine,
    Influence,
}

/// Caller-readable truth about prompt priming for one LLM request.
///
/// There is intentionally no independent boolean: only `Primed` means an
/// influence-ranked definition survived final packing and reached the model.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ContextPrimingStatus {
    NotRequested {
        applied: ToolSelectionMethod,
    },
    Fallback {
        requested: ToolSelectionMethod,
        applied: ToolSelectionMethod,
        reason: String,
    },
    Primed {
        index_id: String,
        selected_candidates: Vec<String>,
        exact_context_tokens: u64,
        scorer_input_tokens: u64,
        scoring_ms: u64,
        model_sha256: String,
        template_sha256: String,
    },
}

impl ContextPrimingStatus {
    #[must_use]
    pub fn is_primed(&self) -> bool {
        matches!(self, Self::Primed { .. })
    }
}

/// One query-dependent score against the immutable candidate pool.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateInfluence {
    pub name: String,
    pub score_per_added_token: f64,
}

/// Whether the finite score vector can identify one uniquely most influential
/// candidate without falling back to canonical catalog ordering.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum InfluenceScoreDiscrimination {
    NoSignal,
    TopTied,
    Discriminative,
}

impl InfluenceScoreDiscrimination {
    /// Stable caller-readable refusal reason, when influence cannot make a
    /// meaningful selection.
    #[must_use]
    pub(crate) fn refusal_reason(self) -> Option<&'static str> {
        match self {
            Self::NoSignal => Some("influence_no_signal"),
            Self::TopTied => Some("influence_top_tied"),
            Self::Discriminative => None,
        }
    }
}

/// Classify a query's finite influence scores before the index's stable tie
/// breaker can turn a non-result into an apparently meaningful ranking.
#[must_use]
pub(crate) fn classify_score_discrimination(
    scores: &[CandidateInfluence],
) -> InfluenceScoreDiscrimination {
    let top_score = scores
        .iter()
        .map(|score| score.score_per_added_token)
        .filter(|score| score.is_finite())
        .reduce(f64::max);
    let Some(top_score) = top_score else {
        return InfluenceScoreDiscrimination::NoSignal;
    };
    if top_score <= V1_SCORE_DISCRIMINATION_TOLERANCE {
        return InfluenceScoreDiscrimination::NoSignal;
    }

    let top_count = scores
        .iter()
        .map(|score| score.score_per_added_token)
        .filter(|score| {
            score.is_finite() && top_score - *score <= V1_SCORE_DISCRIMINATION_TOLERANCE
        })
        .count();
    if top_count > 1 {
        InfluenceScoreDiscrimination::TopTied
    } else {
        InfluenceScoreDiscrimination::Discriminative
    }
}

/// Return the stable refusal reason when a score vector cannot identify one
/// uniquely influential candidate.
///
/// The live paired benchmark calls this same production policy before it may
/// attribute a retrieval result to J-space, preventing test-only tie breaking
/// from being reported as influence.
#[must_use]
pub fn influence_refusal_reason(scores: &[CandidateInfluence]) -> Option<&'static str> {
    classify_score_discrimination(scores).refusal_reason()
}

/// Immutable identity/provenance for a full-definition intervention pool.
#[derive(Debug)]
pub struct InfluenceIndex {
    definitions: Arc<Vec<ToolDefinition>>,
    ordered_names: Vec<String>,
    ordinal_by_name: HashMap<String, usize>,
    identity: String,
    model_sha256: String,
    template_sha256: String,
}

impl InfluenceIndex {
    #[must_use]
    pub fn new(
        definitions: Vec<ToolDefinition>,
        model_sha256: &str,
        template_sha256: &str,
    ) -> Self {
        let ordered_names = definitions
            .iter()
            .map(|definition| definition.function.name.clone())
            .collect::<Vec<_>>();
        let ordinal_by_name = ordered_names
            .iter()
            .enumerate()
            .map(|(ordinal, name)| (name.clone(), ordinal))
            .collect();
        let identity = index_identity(&definitions, model_sha256, template_sha256);
        Self {
            definitions: Arc::new(definitions),
            ordered_names,
            ordinal_by_name,
            identity,
            model_sha256: model_sha256.to_string(),
            template_sha256: template_sha256.to_string(),
        }
    }

    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    #[must_use]
    pub fn model_sha256(&self) -> &str {
        &self.model_sha256
    }

    #[must_use]
    pub fn template_sha256(&self) -> &str {
        &self.template_sha256
    }

    #[must_use]
    pub fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    /// Rank the complete pool by causal score. Missing/non-finite scores sort
    /// last; ties retain the canonical candidate ordering.
    #[must_use]
    pub fn rank(&self, scores: &[CandidateInfluence]) -> Vec<String> {
        let score_by_name = scores
            .iter()
            .filter(|score| {
                self.ordinal_by_name.contains_key(&score.name)
                    && score.score_per_added_token.is_finite()
            })
            .map(|score| (score.name.as_str(), score.score_per_added_token))
            .collect::<HashMap<_, _>>();
        let mut ranked = self.ordered_names.clone();
        ranked.sort_by(|left, right| {
            let left_score = score_by_name
                .get(left.as_str())
                .copied()
                .unwrap_or(f64::NEG_INFINITY);
            let right_score = score_by_name
                .get(right.as_str())
                .copied()
                .unwrap_or(f64::NEG_INFINITY);
            right_score
                .total_cmp(&left_score)
                .then_with(|| self.ordinal_by_name[left].cmp(&self.ordinal_by_name[right]))
        });
        ranked
    }
}

fn index_identity(
    definitions: &[ToolDefinition],
    model_sha256: &str,
    template_sha256: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ALGORITHM_VERSION.as_bytes());
    hasher.update([0]);
    hasher.update(model_sha256.as_bytes());
    hasher.update([0]);
    hasher.update(template_sha256.as_bytes());
    hasher.update([0]);
    // Vec + struct serialization preserves candidate order and every full
    // definition field. A same-name schema/description edit invalidates the
    // identity instead of silently reusing a stale influence pool.
    hasher.update(
        serde_json::to_vec(definitions).expect("tool definitions are always JSON serializable"),
    );
    let digest = hasher.finalize();
    let mut identity = format!("{ALGORITHM_VERSION}:");
    for byte in digest {
        write!(&mut identity, "{byte:02x}").expect("writing to a String cannot fail");
    }
    identity
}

static INFLUENCE_INDEX: OnceLock<RwLock<Option<Arc<InfluenceIndex>>>> = OnceLock::new();

/// Return a process-global influence index for this exact full-definition,
/// model, and template identity. This cache is physically distinct from the
/// cosine capability index.
pub fn global_index(
    definitions: Vec<ToolDefinition>,
    model_sha256: &str,
    template_sha256: &str,
) -> Arc<InfluenceIndex> {
    let identity = index_identity(&definitions, model_sha256, template_sha256);
    let slot = INFLUENCE_INDEX.get_or_init(|| RwLock::new(None));
    if let Ok(guard) = slot.read()
        && let Some(index) = guard.as_ref()
        && index.identity == identity
    {
        return Arc::clone(index);
    }
    let built = Arc::new(InfluenceIndex::new(
        definitions,
        model_sha256,
        template_sha256,
    ));
    if let Ok(mut guard) = slot.write() {
        *guard = Some(Arc::clone(&built));
    }
    built
}

/// Names from the influence ranking that were really present in the final
/// request. Meta-tools and pinned tools are excluded because they were not
/// candidate interventions.
#[must_use]
pub fn applied_candidates(ranked: &[String], final_definitions: &[ToolDefinition]) -> Vec<String> {
    let final_names = final_definitions
        .iter()
        .map(|definition| definition.function.name.as_str())
        .collect::<HashSet<_>>();
    ranked
        .iter()
        .filter(|name| final_names.contains(name.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_llm::FunctionDef;

    fn definition(name: &str, description: &str) -> ToolDefinition {
        ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: name.to_string(),
                description: description.to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"],
                    "additionalProperties": false
                }),
            },
        }
    }

    #[test]
    fn identity_covers_order_full_schema_model_template_and_algorithm() {
        let a = definition("a", "first");
        let b = definition("b", "second");
        let baseline = index_identity(&[a.clone(), b.clone()], "model", "template");
        assert_ne!(
            baseline,
            index_identity(&[b.clone(), a.clone()], "model", "template")
        );
        assert_ne!(
            baseline,
            index_identity(
                &[definition("a", "changed"), b.clone()],
                "model",
                "template"
            )
        );
        assert_ne!(
            baseline,
            index_identity(&[a.clone(), b.clone()], "other-model", "template")
        );
        assert_ne!(baseline, index_identity(&[a, b], "model", "other-template"));
        assert!(baseline.starts_with(ALGORITHM_VERSION));
    }

    #[test]
    fn rank_is_score_descending_stable_and_total() {
        let index = InfluenceIndex::new(
            vec![
                definition("a", "a"),
                definition("b", "b"),
                definition("c", "c"),
                definition("d", "d"),
            ],
            "model",
            "template",
        );
        let ranked = index.rank(&[
            CandidateInfluence {
                name: "c".to_string(),
                score_per_added_token: 0.9,
            },
            CandidateInfluence {
                name: "b".to_string(),
                score_per_added_token: 0.4,
            },
            CandidateInfluence {
                name: "a".to_string(),
                score_per_added_token: 0.4,
            },
            CandidateInfluence {
                name: "unknown".to_string(),
                score_per_added_token: 99.0,
            },
            CandidateInfluence {
                name: "d".to_string(),
                score_per_added_token: f64::NAN,
            },
        ]);
        assert_eq!(ranked, ["c", "a", "b", "d"]);
    }

    fn influences(values: &[f64]) -> Vec<CandidateInfluence> {
        values
            .iter()
            .enumerate()
            .map(|(index, score)| CandidateInfluence {
                name: format!("candidate_{index}"),
                score_per_added_token: *score,
            })
            .collect()
    }

    #[test]
    fn all_zero_scores_are_no_signal() {
        let discrimination = classify_score_discrimination(&influences(&[0.0, 0.0, 0.0]));
        assert_eq!(discrimination, InfluenceScoreDiscrimination::NoSignal);
        assert_eq!(discrimination.refusal_reason(), Some("influence_no_signal"));
    }

    #[test]
    fn exactly_equal_finite_scores_are_top_tied() {
        let discrimination = classify_score_discrimination(&influences(&[0.25, 0.25, 0.25]));
        assert_eq!(discrimination, InfluenceScoreDiscrimination::TopTied);
        assert_eq!(discrimination.refusal_reason(), Some("influence_top_tied"));
    }

    #[test]
    fn tiny_top_score_noise_within_v1_tolerance_is_tied() {
        let tolerance = V1_SCORE_DISCRIMINATION_TOLERANCE;
        let discrimination =
            classify_score_discrimination(&influences(&[0.4, 0.4 + tolerance / 2.0, 0.1]));
        assert_eq!(discrimination, InfluenceScoreDiscrimination::TopTied);
    }

    #[test]
    fn top_tie_is_refused_even_when_lower_scores_differ() {
        let discrimination = classify_score_discrimination(&influences(&[0.9, 0.9, 0.2]));
        assert_eq!(discrimination, InfluenceScoreDiscrimination::TopTied);
    }

    #[test]
    fn unique_top_score_is_discriminative() {
        let discrimination = classify_score_discrimination(&influences(&[0.9, 0.7, 0.2]));
        assert_eq!(discrimination, InfluenceScoreDiscrimination::Discriminative);
        assert_eq!(discrimination.refusal_reason(), None);
    }

    #[test]
    fn unprimed_status_cannot_claim_primed() {
        let fallback = ContextPrimingStatus::Fallback {
            requested: ToolSelectionMethod::Influence,
            applied: ToolSelectionMethod::Keyword,
            reason: "target_model_unsupported".to_string(),
        };
        assert!(!fallback.is_primed());
        assert_eq!(
            serde_json::to_value(fallback).unwrap()["status"],
            "fallback"
        );
    }

    #[test]
    fn applied_candidates_only_reports_definitions_reaching_the_request() {
        let final_definitions = vec![definition("meta", "fixed"), definition("b", "candidate")];
        assert_eq!(
            applied_candidates(
                &["a".to_string(), "b".to_string(), "c".to_string()],
                &final_definitions
            ),
            ["b"]
        );
    }
}
