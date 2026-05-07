use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("model file missing at {0}")]
    ModelMissing(std::path::PathBuf),

    #[error("llama-server binary missing at {0}; install llama.cpp (`brew install llama.cpp`) or set the path explicitly")]
    LlamaServerMissing(std::path::PathBuf),

    #[error("llama-server failed to become ready within {timeout_ms}ms: {detail}")]
    ServerTimeout {
        timeout_ms: u64,
        detail: String,
    },

    #[error("embedder dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch {
        expected: usize,
        actual: usize,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}
