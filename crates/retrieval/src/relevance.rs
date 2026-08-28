//! Semantic relevance scoring for federated literature results.
//!
//! This stage runs only after exact-identifier deduplication, so merged papers
//! are scored with the richest title and abstract the sources supplied.

use prism_embed::{EmbedBackend, cosine_similarity};
use serde::{Deserialize, Serialize};

use crate::model::Paper;

/// Where the semantic relevance boundary is drawn.
///
/// A similarity threshold is a claim about a corpus, not a fact about a
/// document. With PRISM's shipped BGE-small model, the exact PEEK regression
/// fixture scored the relevant materials paper at `0.871` and three unrelated
/// computer-vision and astronomy papers from `0.461` to `0.568`. The `0.60`
/// default separates that measured mini-corpus. It is not a universal truth:
/// another language, discipline, embedding model, or representative corpus
/// can have a different score distribution. Callers should therefore
/// calibrate and override this policy for their own labeled corpus rather than
/// treating the default as ground truth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RelevancePolicy {
    /// Papers scoring below this cosine similarity are excluded. Equality is
    /// retained so the boundary has one precise, testable meaning.
    pub minimum_similarity: f32,
    /// Maximum number of excluded-paper examples carried in the outcome.
    /// The total dropped count is always complete even when examples are
    /// bounded.
    pub max_off_topic_examples: usize,
}

impl Default for RelevancePolicy {
    fn default() -> Self {
        Self {
            minimum_similarity: 0.60,
            max_off_topic_examples: 3,
        }
    }
}

impl RelevancePolicy {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !self.minimum_similarity.is_finite() || !(-1.0..=1.0).contains(&self.minimum_similarity)
        {
            return Err(format!(
                "minimum_similarity must be finite and within [-1, 1], got {}",
                self.minimum_similarity
            ));
        }
        if self.max_off_topic_examples == 0 {
            return Err("max_off_topic_examples must be at least 1".to_string());
        }
        Ok(())
    }
}

/// Whether relevance filtering ran, and whether returned papers were checked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelevanceStatus {
    /// The caller deliberately kept legacy search behavior.
    #[default]
    Disabled,
    /// Every candidate was scored, ranked, and thresholded.
    Applied,
    /// Filtering was requested but no embedding backend was available.
    Unavailable,
    /// Backend initialization, embedding, or vector validation failed.
    Failed,
}

/// One excluded result shown so callers can audit what the filter removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OffTopicExample {
    pub source: String,
    pub source_id: String,
    pub title: String,
    pub score: f32,
}

/// Honest accounting for the relevance stage of one search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelevanceReport {
    pub status: RelevanceStatus,
    /// Deduplicated papers presented to the relevance stage.
    pub candidates: usize,
    /// Papers successfully assigned a validated similarity score.
    pub evaluated: usize,
    /// Complete count excluded as below-threshold.
    pub dropped: usize,
    /// `None` only when filtering was deliberately disabled.
    pub threshold: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// True means `papers` were returned in their original, unfiltered
    /// order. When the selector stage drops papers afterwards, the engine
    /// clears this flag so it stays a statement about the returned set, not
    /// about this stage alone.
    pub returned_unfiltered: bool,
    #[serde(default)]
    pub off_topic_examples: Vec<OffTopicExample>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// What the optional LLM selector stage did after this embedding stage.
    /// `None` means the selector was not configured for this engine; every
    /// other state (unavailable, applied, failed) is a present report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<crate::selector::SelectorReport>,
}

impl Default for RelevanceReport {
    fn default() -> Self {
        Self::disabled(0)
    }
}

impl RelevanceReport {
    pub(crate) fn disabled(candidates: usize) -> Self {
        Self {
            status: RelevanceStatus::Disabled,
            candidates,
            evaluated: 0,
            dropped: 0,
            threshold: None,
            backend: None,
            returned_unfiltered: true,
            off_topic_examples: Vec::new(),
            message: Some(
                "relevance filtering was disabled; papers were returned unfiltered".to_string(),
            ),
            selector: None,
        }
    }

    pub(crate) fn unavailable(candidates: usize, policy: &RelevancePolicy) -> Self {
        Self {
            status: RelevanceStatus::Unavailable,
            candidates,
            evaluated: 0,
            dropped: 0,
            threshold: Some(policy.minimum_similarity),
            backend: None,
            returned_unfiltered: true,
            off_topic_examples: Vec::new(),
            message: Some(
                "no embedding backend was available; papers were returned unfiltered".to_string(),
            ),
            selector: None,
        }
    }

    pub(crate) fn failed(
        candidates: usize,
        policy: &RelevancePolicy,
        backend: Option<String>,
        reason: impl std::fmt::Display,
    ) -> Self {
        Self {
            status: RelevanceStatus::Failed,
            candidates,
            evaluated: 0,
            dropped: 0,
            threshold: Some(policy.minimum_similarity),
            backend,
            returned_unfiltered: true,
            off_topic_examples: Vec::new(),
            message: Some(format!(
                "relevance filtering failed ({reason}); papers were returned unfiltered"
            )),
            selector: None,
        }
    }

    fn applied(
        candidates: usize,
        dropped: usize,
        policy: &RelevancePolicy,
        backend: String,
        off_topic_examples: Vec<OffTopicExample>,
    ) -> Self {
        Self {
            status: RelevanceStatus::Applied,
            candidates,
            evaluated: candidates,
            dropped,
            threshold: Some(policy.minimum_similarity),
            backend: Some(backend),
            returned_unfiltered: false,
            off_topic_examples,
            message: None,
            selector: None,
        }
    }
}

/// Score, rank, and filter in place. Nothing is mutated until the complete
/// embedding batch and every vector have been validated, so any failure can
/// return the original papers honestly and in their original order.
pub(crate) async fn filter_papers(
    query: &str,
    papers: &mut Vec<Paper>,
    policy: &RelevancePolicy,
    backend: &dyn EmbedBackend,
) -> Result<RelevanceReport, String> {
    policy.validate()?;

    let mut inputs = Vec::with_capacity(papers.len() + 1);
    inputs.push(query.to_string());
    inputs.extend(papers.iter().map(paper_embedding_text));

    let embeddings = backend
        .embed(&inputs)
        .await
        .map_err(|error| format!("embedding batch failed: {error:#}"))?;
    validate_embeddings(&embeddings, inputs.len())?;

    let query_embedding = &embeddings[0];
    let mut scored = Vec::with_capacity(papers.len());
    for (index, paper_embedding) in embeddings.iter().skip(1).enumerate() {
        let score = cosine_similarity(query_embedding, paper_embedding);
        if !score.is_finite() {
            return Err(format!(
                "embedding backend produced a non-finite score for candidate {index}"
            ));
        }
        scored.push((index, score));
    }
    scored.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });

    let candidates = papers.len();
    let mut slots: Vec<Option<Paper>> = std::mem::take(papers).into_iter().map(Some).collect();
    let mut retained = Vec::with_capacity(candidates);
    let mut dropped = 0usize;
    let mut examples = Vec::new();

    for (index, score) in scored {
        let paper = slots[index]
            .take()
            .expect("each scored paper index must occur exactly once");
        if score >= policy.minimum_similarity {
            retained.push(paper);
        } else {
            dropped += 1;
            if examples.len() < policy.max_off_topic_examples {
                examples.push(OffTopicExample {
                    source: paper.source,
                    source_id: paper.source_id,
                    title: paper.title,
                    score,
                });
            }
        }
    }

    *papers = retained;
    Ok(RelevanceReport::applied(
        candidates,
        dropped,
        policy,
        backend.id().to_string(),
        examples,
    ))
}

fn paper_embedding_text(paper: &Paper) -> String {
    match paper.abstract_text.as_deref().map(str::trim) {
        Some(abstract_text) if !abstract_text.is_empty() => {
            format!("{}\n\n{abstract_text}", paper.title.trim())
        }
        _ => paper.title.trim().to_string(),
    }
}

fn validate_embeddings(embeddings: &[Vec<f32>], expected_count: usize) -> Result<(), String> {
    if embeddings.len() != expected_count {
        return Err(format!(
            "embedding backend returned {} vectors for {expected_count} inputs",
            embeddings.len()
        ));
    }
    let dimensions = embeddings.first().map(Vec::len).unwrap_or_default();
    if dimensions == 0 {
        return Err("embedding backend returned zero-dimensional vectors".to_string());
    }
    for (index, embedding) in embeddings.iter().enumerate() {
        if embedding.len() != dimensions {
            return Err(format!(
                "embedding {index} has {} dimensions; expected {dimensions}",
                embedding.len()
            ));
        }
        if embedding.iter().any(|value| !value.is_finite()) {
            return Err(format!("embedding {index} contains a non-finite value"));
        }
        if cosine_similarity(embedding, embedding) == 0.0 {
            return Err(format!("embedding {index} is a zero vector"));
        }
    }
    Ok(())
}
