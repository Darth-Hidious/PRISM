//! Capability index — neural (embedding) retrieval over the capability catalog.
//!
//! `docs/CAPABILITY_REGISTRY_DESIGN.md`, Phases 0-2:
//! - **P0**: [`CapabilityIndex`] — embed-once, cosine top-K retrieval primitive.
//! - **P1**: [`global_index`] caches the embedded index; `agent_loop` uses
//!   `CapabilityIndex::retrieve` instead of keyword `definitions_for_query`,
//!   with the keyword path kept as fallback.
//! - **P2**: [`capability_menu`] builds the L1 progressive-disclosure menu so the
//!   model is AWARE of capabilities beyond the callable top-K.
//!
//! Neural selection is **on by default**; `PRISM_NEURAL_TOOLS=0/false/off`
//! forces the legacy keyword path. It degrades gracefully (cold turn or no embed
//! backend → keyword), so default-on can never do worse. The unifying model
//! (tool / skill / workflow / MCP-tool / authored → one `Capability`) and the
//! deletion of the parallel registries land in later phases; the retrieval
//! primitives here are the proven ground for that.

use prism_embed::EmbedBackend;

/// One indexed capability: a callable `name` plus the text we embed to find it.
#[derive(Debug, Clone)]
pub struct Capability {
    /// The name the model calls.
    pub name: String,
    /// Text embedded for retrieval. Today `"{name}: {description}"` (RAG-MCP
    /// embeds tool descriptions); embedding example *queries* (Tool2vec) is a
    /// later refinement.
    pub retrieval_text: String,
    /// Cached embedding of `retrieval_text`; `None` until [`CapabilityIndex::embed_all`].
    pub embedding: Option<Vec<f32>>,
}

/// Neural retrieval index. Capability embeddings are computed **once** (the set
/// is static within a session); only the short routing query is embedded per
/// turn, so retrieval costs one small embed call.
#[derive(Debug, Default, Clone)]
pub struct CapabilityIndex {
    caps: Vec<Capability>,
}

impl CapabilityIndex {
    /// Build from `(name, retrieval_text)` pairs. Embeddings start empty — call
    /// [`Self::embed_all`] to populate them before [`Self::retrieve`].
    pub fn from_entries(entries: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            caps: entries
                .into_iter()
                .map(|(name, retrieval_text)| Capability {
                    name,
                    retrieval_text,
                    embedding: None,
                })
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.caps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.caps.is_empty()
    }

    /// How many capabilities have a cached embedding (i.e. are retrievable).
    pub fn embedded_count(&self) -> usize {
        self.caps.iter().filter(|c| c.embedding.is_some()).count()
    }

    /// Populate embeddings for every capability in one batched call. Call once
    /// at index build; cheap to skip when already embedded.
    pub async fn embed_all(&mut self, backend: &dyn EmbedBackend) -> anyhow::Result<()> {
        let pending: Vec<(usize, String)> = self
            .caps
            .iter()
            .enumerate()
            .filter(|(_, c)| c.embedding.is_none())
            .map(|(i, c)| (i, c.retrieval_text.clone()))
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        let texts: Vec<String> = pending.iter().map(|(_, t)| t.clone()).collect();
        let vectors = backend.embed(&texts).await?;
        for ((idx, _), vec) in pending.into_iter().zip(vectors) {
            self.caps[idx].embedding = Some(vec);
        }
        Ok(())
    }

    /// Retrieve up to `top_k` capability names most semantically relevant to
    /// `query`, in descending relevance. Returns empty (so callers fall back to
    /// the keyword path) when there are no embeddings, `top_k == 0`, the query
    /// is blank, or the query can't be embedded.
    pub async fn retrieve(
        &self,
        query: &str,
        top_k: usize,
        backend: &dyn EmbedBackend,
    ) -> Vec<String> {
        let query = query.trim();
        if self.caps.is_empty() || top_k == 0 || query.is_empty() {
            return Vec::new();
        }
        let query_owned = query.to_string();
        let qvec = match backend.embed(std::slice::from_ref(&query_owned)).await {
            Ok(mut v) if !v.is_empty() => v.remove(0),
            _ => return Vec::new(),
        };

        let mut scored: Vec<(f32, &str)> = self
            .caps
            .iter()
            .filter_map(|c| {
                c.embedding
                    .as_ref()
                    .map(|e| (cosine(&qvec, e), c.name.as_str()))
            })
            .collect();
        // Descending by score; NaN sorts last.
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored
            .into_iter()
            .take(top_k)
            .map(|(_, name)| name.to_string())
            .collect()
    }
}

/// Cosine similarity. Returns `0.0` for length-mismatched or zero-norm vectors.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Cache slot for the process-global index: `(names-hash, index)`, or empty.
type IndexCacheSlot = std::sync::Mutex<Option<(u64, std::sync::Arc<CapabilityIndex>)>>;

/// Process-global embedded index. Building it embeds EVERY capability, which on
/// CPU takes seconds for a large catalog — so it must never happen on the turn
/// path (see [`global_index_if_ready`] + the background warm in `agent_loop`).
static INDEX_CACHE: std::sync::OnceLock<IndexCacheSlot> = std::sync::OnceLock::new();

/// The embedded index for `entries` **iff it is already built** — never embeds,
/// never blocks. Returns `None` when the index hasn't been warmed yet (or the
/// capability set changed), so the turn path can fall back to keyword instead of
/// stalling for seconds embedding the whole catalog.
pub fn global_index_if_ready(
    entries: &[(String, String)],
) -> Option<std::sync::Arc<CapabilityIndex>> {
    let key = key_of(entries);
    let cache = INDEX_CACHE.get()?;
    let guard = cache.lock().ok()?;
    let (cached_key, idx) = guard.as_ref()?;
    (*cached_key == key).then(|| idx.clone())
}

/// Build (embed) the index and cache it, reusing the cache if already current.
/// Embeds the whole catalog once, so run this in the BACKGROUND, not on a turn.
/// Best-effort: on backend error the index has no vectors, so
/// [`CapabilityIndex::retrieve`] returns empty and callers fall back to keyword.
pub async fn global_index(
    entries: Vec<(String, String)>,
    backend: &dyn EmbedBackend,
) -> std::sync::Arc<CapabilityIndex> {
    use std::sync::Arc;

    let key = key_of(&entries);
    let cache = INDEX_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    // Fast path — the std Mutex guard is dropped before any await below.
    if let Ok(guard) = cache.lock()
        && let Some((cached_key, idx)) = guard.as_ref()
        && *cached_key == key
    {
        return idx.clone();
    }

    let mut idx = CapabilityIndex::from_entries(entries);
    let _ = idx.embed_all(backend).await; // best-effort; empties → keyword fallback
    let arc = Arc::new(idx);
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((key, arc.clone()));
    }
    arc
}

fn key_of(entries: &[(String, String)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    entries.len().hash(&mut hasher);
    for (name, _) in entries {
        name.hash(&mut hasher);
    }
    hasher.finish()
}

/// Build the L1 progressive-disclosure menu: a compact `- name: one-line` list
/// of capabilities the model is AWARE of but that aren't in its callable set.
/// Keeps the model from being blind to the wider catalog without paying the
/// full-schema token cost. `entries` are `(name, "name: description")` pairs;
/// `exclude` are already-callable names; bounded by `max_entries` and
/// `max_desc_chars`. Returns `None` when there's nothing to advertise.
pub fn capability_menu(
    entries: &[(String, String)],
    exclude: &std::collections::HashSet<String>,
    max_entries: usize,
    max_desc_chars: usize,
) -> Option<String> {
    let mut lines = Vec::new();
    for (name, text) in entries {
        if exclude.contains(name) {
            continue;
        }
        let desc = text.split_once(": ").map_or(text.as_str(), |(_, d)| d);
        let desc: String = desc.chars().take(max_desc_chars).collect();
        lines.push(format!("- {name}: {desc}"));
        if lines.len() >= max_entries {
            break;
        }
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "Capabilities beyond the tools above also exist. You are AWARE of them \
         but must pull one into your callable set with find_tools(query) before \
         calling it:\n{}",
        lines.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    /// Deterministic 2-dim embedder for the readiness test.
    struct StubEmbed;
    #[async_trait::async_trait]
    impl EmbedBackend for StubEmbed {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0f32, 0.0]).collect())
        }
        fn dimensions(&self) -> usize {
            2
        }
        fn id(&self) -> &str {
            "test:stub-embed"
        }
    }

    #[tokio::test]
    async fn index_is_not_ready_until_warmed_then_is() {
        // Unique names so this test owns its cache key regardless of ordering.
        let entries = vec![
            (
                "warm_probe_alpha_zx".to_string(),
                "warm_probe_alpha_zx: unique probe capability one".to_string(),
            ),
            (
                "warm_probe_beta_zx".to_string(),
                "warm_probe_beta_zx: unique probe capability two".to_string(),
            ),
        ];
        // Cold: the turn path must see "not ready" and fall back to keyword —
        // never embed the catalog on-turn. Robust under parallel tests because
        // these entry names are unique to this test, so the key never matches a
        // slot another test warmed.
        assert!(
            global_index_if_ready(&entries).is_none(),
            "index must not be ready before a background warm"
        );
        // Background warm (what spawn_neural_warm does off the turn path).
        // Assert on the Arc it RETURNS, not on a re-read of the process-global
        // single-slot cache: another test's concurrent spawn_neural_warm can
        // overwrite that slot between the warm and the read, which made the old
        // `global_index_if_ready(..).expect()` re-read intermittently panic.
        let warmed = global_index(entries.clone(), &StubEmbed).await;
        assert_eq!(warmed.embedded_count(), 2);
    }

    #[test]
    fn capability_menu_excludes_callable_and_clips_descriptions() {
        let entries = vec![
            ("web".to_string(), "web: search the web".to_string()),
            (
                "mace".to_string(),
                "mace: predict elastic constants description tail".to_string(),
            ),
        ];
        let mut exclude = std::collections::HashSet::new();
        exclude.insert("web".to_string());
        let menu = capability_menu(&entries, &exclude, 10, 20).unwrap();
        assert!(
            !menu.contains("web:"),
            "excluded capability must not appear"
        );
        assert!(menu.contains("- mace: predict elastic cons")); // 20-char clip
        assert!(!menu.contains("description tail"), "desc must be clipped");
        assert!(menu.contains("find_tools"));
    }

    #[test]
    fn capability_menu_none_when_everything_excluded() {
        let entries = vec![("web".to_string(), "web: search".to_string())];
        let mut exclude = std::collections::HashSet::new();
        exclude.insert("web".to_string());
        assert!(capability_menu(&entries, &exclude, 10, 50).is_none());
    }

    /// Live-verify against the REAL local ONNX embedder (bge-small-en-v1.5),
    /// not the deterministic test stub. Ignored by default (needs the ~128 MB
    /// model cached at `~/.prism/models/embed`); run with:
    ///   `cargo test -p prism-agent --lib -- --ignored real_embedding`
    /// Proves neural retrieval ranks the right capability on real vectors from
    /// paraphrased queries with no literal keyword overlap.
    #[tokio::test]
    #[ignore = "requires the local ONNX embed model; run with --ignored"]
    async fn real_embedding_backend_ranks_the_right_capability() {
        let backend = prism_embed::NativeOnnx::new().expect("load local embed model");
        let mut idx = CapabilityIndex::from_entries([
            (
                "mace_compute_elastic".to_string(),
                "predict the elastic tensor, bulk and shear moduli of a crystal structure"
                    .to_string(),
            ),
            (
                "web".to_string(),
                "search the open web and fetch a url".to_string(),
            ),
            (
                "analyze_phases".to_string(),
                "CALPHAD phase equilibrium and stability at temperature".to_string(),
            ),
            (
                "prior_art_search".to_string(),
                "search arXiv and patents for scientific literature".to_string(),
            ),
        ]);
        idx.embed_all(&backend).await.expect("embed capabilities");
        assert_eq!(idx.embedded_count(), 4);

        // Paraphrases with no literal overlap with the tool text.
        assert_eq!(
            idx.retrieve("how stiff is this alloy, what are its moduli", 1, &backend)
                .await,
            vec!["mace_compute_elastic".to_string()],
            "real embeddings should rank the stiffness/moduli tool first"
        );
        assert_eq!(
            idx.retrieve(
                "find me published research and patents on this",
                1,
                &backend
            )
            .await,
            vec!["prior_art_search".to_string()],
        );
    }

    /// Deterministic 3-axis embedder for tests: each axis fires on a keyword
    /// family, so a query lands next to the capability that shares *meaning*
    /// even without a literal substring match.
    struct AxisEmbed;

    fn axis(text: &str, keywords: &[&str]) -> f32 {
        if keywords.iter().any(|k| text.contains(k)) {
            1.0
        } else {
            0.0
        }
    }

    #[async_trait::async_trait]
    impl EmbedBackend for AxisEmbed {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    let t = t.to_lowercase();
                    vec![
                        axis(&t, &["elastic", "stiffness", "modulus", "tensor"]),
                        axis(&t, &["web", "internet", "online", "url"]),
                        axis(&t, &["phase", "calphad", "equilibrium"]),
                    ]
                })
                .collect())
        }
        fn dimensions(&self) -> usize {
            3
        }
        fn id(&self) -> &str {
            "test:axis-embed"
        }
    }

    fn sample_index() -> CapabilityIndex {
        CapabilityIndex::from_entries([
            (
                "mace_compute_elastic".to_string(),
                "predict the elastic tensor and derived moduli of a structure".to_string(),
            ),
            (
                "web".to_string(),
                "fetch a url or search the internet online".to_string(),
            ),
            (
                "analyze_phases".to_string(),
                "run CALPHAD phase equilibrium stability check".to_string(),
            ),
        ])
    }

    #[tokio::test]
    async fn retrieves_semantically_closest_capability_without_keyword_overlap() {
        let mut idx = sample_index();
        let backend = AxisEmbed;
        idx.embed_all(&backend).await.unwrap();
        assert_eq!(idx.embedded_count(), 3);

        // "stiffness" shares no substring with "elastic tensor" but is the same axis.
        let hits = idx
            .retrieve("compute the stiffness of this alloy", 1, &backend)
            .await;
        assert_eq!(hits, vec!["mace_compute_elastic".to_string()]);

        let hits = idx.retrieve("look this up online", 1, &backend).await;
        assert_eq!(hits, vec!["web".to_string()]);
    }

    #[tokio::test]
    async fn returns_empty_without_embeddings_or_on_blank_query() {
        let idx = sample_index(); // embed_all NOT called
        let backend = AxisEmbed;
        assert!(idx.retrieve("stiffness", 3, &backend).await.is_empty());

        let mut idx = sample_index();
        idx.embed_all(&backend).await.unwrap();
        assert!(idx.retrieve("   ", 3, &backend).await.is_empty());
        assert!(idx.retrieve("stiffness", 0, &backend).await.is_empty());
    }

    #[test]
    fn cosine_handles_zero_and_mismatched_vectors() {
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
        assert_eq!(cosine(&[1.0], &[1.0, 0.0]), 0.0);
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
    }
}
