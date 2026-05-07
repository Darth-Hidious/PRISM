//! Configuration for the tool router. Resolves model file paths, ports,
//! and llama.cpp binary location.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Config {
    /// Path to the EmbeddingGemma GGUF.
    pub embedder_gguf: PathBuf,
    /// Path to the FunctionGemma GGUF (used in stage 2.2; optional today).
    pub function_gguf: Option<PathBuf>,
    /// Path to llama.cpp `llama-server` binary.
    pub llama_server_bin: PathBuf,
    /// Directory holding the tool index (catalog.jsonl + embeddings.bin).
    pub index_dir: PathBuf,
    /// Port range start; the router will pick the first free port at/above this.
    pub port_floor: u16,
    /// Embedding dimensionality. EmbeddingGemma-300M is 768 by default but
    /// supports Matryoshka truncation; we lock 768 for now.
    pub embed_dim: usize,
    /// How many tokens of context to reserve for the embedder.
    pub embed_ctx: usize,
    /// Where to fetch the embedder GGUF from when missing locally. The
    /// remote layout is HF Hub's `resolve` URL:
    /// `https://huggingface.co/{repo}/resolve/main/{file}`.
    pub embedder_remote: ModelRemote,
    /// Same for FunctionGemma. Users who fine-tune on Colab change this
    /// (or the env var override) to point at their private repo.
    pub function_remote: ModelRemote,
}

#[derive(Debug, Clone)]
pub struct ModelRemote {
    /// HF repo id, e.g. `unsloth/embeddinggemma-300m-GGUF`.
    pub repo: String,
    /// File name in the repo, e.g. `embeddinggemma-300M-Q8_0.gguf`.
    pub file: String,
}

impl Config {
    /// Default config rooted at `~/.prism/`. Models are expected at
    ///   ~/.prism/models/embeddinggemma-300m.gguf
    ///   ~/.prism/models/functiongemma-270m.gguf
    /// (downloaded by a separate bootstrap step the first time PRISM
    /// launches on this machine).
    pub fn default_for_home(home: &Path) -> Self {
        let prism_dir = home.join(".prism");
        Self {
            embedder_gguf: prism_dir.join("models/embeddinggemma-300m.gguf"),
            function_gguf: Some(prism_dir.join("models/functiongemma-270m.gguf")),
            llama_server_bin: which_llama_server(),
            index_dir: prism_dir.join("tool_router/index"),
            port_floor: 18800,
            embed_dim: 768,
            embed_ctx: 2048,
            embedder_remote: ModelRemote {
                repo: env_or("PRISM_EMBEDDER_REPO", "unsloth/embeddinggemma-300m-GGUF"),
                file: env_or("PRISM_EMBEDDER_FILE", "embeddinggemma-300M-Q8_0.gguf"),
            },
            // Defaults to the upstream Unsloth GGUF; once a user finishes
            // their Colab fine-tune they set PRISM_FUNCTION_REPO to their
            // private repo and PRISM picks up the fine-tuned weights on
            // next boot.
            function_remote: ModelRemote {
                repo: env_or("PRISM_FUNCTION_REPO", "unsloth/functiongemma-270m-it-GGUF"),
                file: env_or("PRISM_FUNCTION_FILE", "functiongemma-270m-it-Q4_K_M.gguf"),
            },
        }
    }
}

fn env_or(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_string())
}

fn which_llama_server() -> PathBuf {
    // Try common locations in priority order. Falls back to "llama-server"
    // (relying on PATH). The bootstrap step in the "good app" phase will
    // either download a vendored binary into ~/.prism/bin or surface a
    // clear "install llama.cpp" message.
    for candidate in [
        "/opt/homebrew/bin/llama-server",
        "/usr/local/bin/llama-server",
    ] {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return p;
        }
    }
    PathBuf::from("llama-server")
}
