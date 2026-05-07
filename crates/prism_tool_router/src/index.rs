//! Persistent tool index.
//!
//! Two files on disk:
//!   ~/.prism/tool_router/index/catalog.jsonl  — one record per indexed tool,
//!     ordered, addressable by row index. Records carry name + content hash
//!     + the offset/length of the embedding vector in `embeddings.bin`.
//!   ~/.prism/tool_router/index/embeddings.bin — packed `embed_dim * f32`
//!     vectors, contiguous, in the same order as catalog.jsonl.
//!
//! Re-embed only the delta when tool descriptions change. The hash includes
//! everything the embedder sees (name + description + arg schema), so a
//! marketplace tool getting a docstring update produces a new hash and gets
//! re-embedded automatically the next time it shows up in a forge request.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;

/// What we embed for a single tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON schema (OpenAI tool function `parameters` shape) — included in
    /// the hash so schema changes invalidate the embedding too.
    #[serde(default)]
    pub args_schema: serde_json::Value,
}

impl ToolDef {
    /// The text the embedder sees. Format matches what FunctionGemma will
    /// see when it routes — keeps the two stages of the pipeline aligned.
    pub fn embed_text(&self) -> String {
        // EmbeddingGemma is task-aware: prepend the standard
        // "task: search result | query: ..." prefix isn't appropriate here
        // because we want symmetric tool↔query embeddings. Stick with raw
        // name + description; queries embed without a prefix too.
        format!("{}\n{}", self.name, self.description.trim())
    }

    pub fn content_hash(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.name.as_bytes());
        h.update(b"\0");
        h.update(self.description.as_bytes());
        h.update(b"\0");
        let schema_bytes = serde_json::to_vec(&self.args_schema).unwrap_or_default();
        h.update(&schema_bytes);
        let bytes = h.finalize();
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            use std::fmt::Write;
            let _ = write!(s, "{:02x}", b);
        }
        s
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CatalogRow {
    name: String,
    hash: String,
    description: String,
    /// Index into the embeddings.bin file, in vector slots (not bytes).
    slot: u32,
}

pub struct ToolIndex {
    rows: Vec<CatalogRow>,
    /// name → row index lookup. Keeps `rows` ordered by slot.
    by_name: HashMap<String, usize>,
    embeddings: Vec<Vec<f32>>,
    embed_dim: usize,
}

impl ToolIndex {
    pub fn load_or_init(config: &Config) -> Result<Self> {
        std::fs::create_dir_all(&config.index_dir)
            .with_context(|| format!("create {}", config.index_dir.display()))?;

        let catalog_path = config.index_dir.join("catalog.jsonl");
        let embeddings_path = config.index_dir.join("embeddings.bin");

        if !catalog_path.exists() || !embeddings_path.exists() {
            return Ok(Self {
                rows: Vec::new(),
                by_name: HashMap::new(),
                embeddings: Vec::new(),
                embed_dim: config.embed_dim,
            });
        }

        let mut rows: Vec<CatalogRow> = Vec::new();
        let f = File::open(&catalog_path).context("open catalog")?;
        for line in BufReader::new(f).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            rows.push(serde_json::from_str(&line)?);
        }

        // Read embeddings in slot order.
        let mut f = File::open(&embeddings_path).context("open embeddings")?;
        let bytes_per_vec = config.embed_dim * std::mem::size_of::<f32>();
        let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(rows.len());
        for _ in 0..rows.len() {
            let mut buf = vec![0u8; bytes_per_vec];
            f.read_exact(&mut buf).context("read embedding slot")?;
            let mut v = Vec::with_capacity(config.embed_dim);
            for chunk in buf.chunks_exact(4) {
                v.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
            embeddings.push(v);
        }

        let mut by_name = HashMap::with_capacity(rows.len());
        for (i, r) in rows.iter().enumerate() {
            by_name.insert(r.name.clone(), i);
        }

        Ok(Self {
            rows,
            by_name,
            embeddings,
            embed_dim: config.embed_dim,
        })
    }

    /// Returns true if `tool` is already in the index with the same content
    /// hash. Insert / update otherwise.
    pub fn has_current(&self, tool: &ToolDef) -> bool {
        match self.by_name.get(&tool.name) {
            Some(&i) => self.rows[i].hash == tool.content_hash(),
            None => false,
        }
    }

    pub fn upsert(&mut self, tool: ToolDef, vector: Vec<f32>) {
        let hash = tool.content_hash();
        if let Some(&i) = self.by_name.get(&tool.name) {
            self.rows[i].hash = hash;
            self.rows[i].description = tool.description;
            self.embeddings[i] = vector;
            return;
        }
        let slot = self.rows.len() as u32;
        let row = CatalogRow {
            name: tool.name.clone(),
            hash,
            description: tool.description,
            slot,
        };
        self.by_name.insert(tool.name.clone(), self.rows.len());
        self.rows.push(row);
        self.embeddings.push(vector);
    }

    pub fn persist(&self, config: &Config) -> Result<()> {
        let catalog_path = config.index_dir.join("catalog.jsonl");
        let embeddings_path = config.index_dir.join("embeddings.bin");

        // Atomic-ish write: write to .tmp, rename. Two files, two renames —
        // racy in the worst case but the hash check on next load lets us
        // detect and repair drift if needed.
        let tmp_cat = catalog_path.with_extension("jsonl.tmp");
        let tmp_emb = embeddings_path.with_extension("bin.tmp");

        let mut cat = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_cat)?;
        for row in &self.rows {
            let line = serde_json::to_string(row)?;
            cat.write_all(line.as_bytes())?;
            cat.write_all(b"\n")?;
        }
        cat.sync_all()?;
        drop(cat);

        let mut emb = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_emb)?;
        for v in &self.embeddings {
            for &f in v {
                emb.write_all(&f.to_le_bytes())?;
            }
        }
        emb.sync_all()?;
        drop(emb);

        std::fs::rename(&tmp_cat, &catalog_path)?;
        std::fs::rename(&tmp_emb, &embeddings_path)?;
        Ok(())
    }

    /// Top-K cosine over the index, restricted to the given names. Returns
    /// names ordered most-similar-first. Names not present in the index are
    /// silently dropped (they should be added via index_tools first).
    pub fn top_k_filtered(&self, query: &[f32], available: &[String], k: usize) -> Vec<String> {
        if query.len() != self.embed_dim || self.rows.is_empty() {
            return Vec::new();
        }
        let q_norm = norm(query);
        let mut scored: Vec<(f32, &str)> = available
            .iter()
            .filter_map(|n| {
                let i = *self.by_name.get(n)?;
                let v = &self.embeddings[i];
                let s = cosine(query, v, q_norm);
                Some((s, n.as_str()))
            })
            .collect();
        // descending by score
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored
            .into_iter()
            .take(k)
            .map(|(_, n)| n.to_string())
            .collect()
    }
}

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

fn cosine(a: &[f32], b: &[f32], a_norm: f32) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let bn = norm(b);
    if a_norm == 0.0 || bn == 0.0 {
        return 0.0;
    }
    dot / (a_norm * bn)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(tmp: &Path) -> Config {
        Config {
            embedder_gguf: tmp.join("e.gguf"),
            function_gguf: None,
            llama_server_bin: tmp.join("llama-server"),
            index_dir: tmp.join("idx"),
            port_floor: 9000,
            embed_dim: 3,
            embed_ctx: 512,
        }
    }

    #[test]
    fn upsert_persist_roundtrip() {
        let tmp = tempfile_dir();
        let config = cfg(&tmp);
        std::fs::create_dir_all(&config.index_dir).unwrap();

        let mut idx = ToolIndex::load_or_init(&config).unwrap();
        let t1 = ToolDef {
            name: "alpha".into(),
            description: "first".into(),
            args_schema: serde_json::json!({}),
        };
        let t2 = ToolDef {
            name: "beta".into(),
            description: "second".into(),
            args_schema: serde_json::json!({}),
        };
        idx.upsert(t1.clone(), vec![1.0, 0.0, 0.0]);
        idx.upsert(t2.clone(), vec![0.0, 1.0, 0.0]);
        idx.persist(&config).unwrap();

        let idx2 = ToolIndex::load_or_init(&config).unwrap();
        assert!(idx2.has_current(&t1));
        assert!(idx2.has_current(&t2));

        let q = vec![0.9, 0.1, 0.0];
        let top = idx2.top_k_filtered(&q, &["alpha".into(), "beta".into()], 2);
        assert_eq!(top, vec!["alpha".to_string(), "beta".to_string()]);
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "prism-tool-router-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
