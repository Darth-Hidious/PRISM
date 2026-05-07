//! Lazy GGUF download from Hugging Face Hub.
//!
//! On first launch the user has neither model on disk. Rather than fail with
//! a manual `huggingface-cli download ...` instruction, we stream the file
//! down ourselves and print a progress line every few MB. Idempotent — if
//! the file already exists with a non-zero size we skip.
//!
//! HF's `resolve` URL pattern is what `huggingface-cli download` ends up
//! hitting too:
//!
//! ```text
//! https://huggingface.co/{repo}/resolve/main/{file}
//! ```
//!
//! Private repos need a token; we look at HF_TOKEN and ~/.cache/huggingface/token
//! in that order.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

use crate::config::ModelRemote;

/// Make sure `dest` exists, downloading from `remote` on HF Hub if not.
/// Returns Ok(true) when a download happened, Ok(false) when the file was
/// already present.
pub async fn ensure_model(remote: &ModelRemote, dest: &Path) -> Result<bool> {
    if dest.exists()
        && let Ok(meta) = std::fs::metadata(dest)
        && meta.len() > 0
    {
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create model dir {}", parent.display()))?;
    }

    let url = format!(
        "https://huggingface.co/{}/resolve/main/{}",
        remote.repo, remote.file
    );

    let mut request = reqwest::Client::builder()
        .timeout(Duration::from_secs(60 * 30))
        .build()
        .context("http client")?
        .get(&url);
    if let Some(token) = read_hf_token() {
        request = request.bearer_auth(token);
    }
    let resp = request.send().await.with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("HF download {url} returned {status}: {body}");
    }
    let total = resp.content_length();

    // Render the progress bar through a small callback so the host CLI
    // controls visual style. We emit the same line repeatedly with carriage
    // return; final newline emitted via the sentinel call after success.
    let prefix = remote.file.clone();
    let print_progress = |done: u64, total: Option<u64>| {
        use std::io::{IsTerminal, Write};
        let cr = if std::io::stderr().is_terminal() {
            "\r"
        } else {
            "\n"
        };
        match total {
            Some(t) if t > 0 => {
                let pct = (done as f64 / t as f64).min(1.0);
                let bar_w = 24usize;
                let filled = (pct * bar_w as f64) as usize;
                let bar: String = "█".repeat(filled) + &"░".repeat(bar_w - filled);
                eprint!(
                    "{cr}\x1b[38;2;100;100;255m \u{2502}   \x1b[38;2;200;200;200m{prefix} \x1b[38;2;0;255;255m{bar} \x1b[38;2;255;255;255m{:>5.1}% \x1b[38;2;100;100;100m{}/{} MB\x1b[0m",
                    pct * 100.0,
                    done / 1_048_576,
                    t / 1_048_576
                );
            }
            _ => {
                eprint!(
                    "{cr}\x1b[38;2;100;100;255m \u{2502}   \x1b[38;2;200;200;200m{prefix} \x1b[38;2;100;100;100m{} MB\x1b[0m",
                    done / 1_048_576
                );
            }
        }
        let _ = std::io::stderr().flush();
    };

    eprintln!(
        "\x1b[38;2;100;100;255m \u{2502}   \x1b[38;2;200;200;200mfetching {} from {}\x1b[0m",
        remote.file, remote.repo
    );

    let tmp = dest.with_extension("download");
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .with_context(|| format!("open {}", tmp.display()))?;

    let mut stream = resp.bytes_stream();
    let mut written: u64 = 0;
    let mut last_render = std::time::Instant::now();
    print_progress(0, total);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("stream chunk")?;
        file.write_all(&chunk).await.context("write chunk")?;
        written += chunk.len() as u64;
        // Render at most every ~80ms to avoid trashing the terminal.
        if last_render.elapsed().as_millis() >= 80 {
            print_progress(written, total);
            last_render = std::time::Instant::now();
        }
    }
    print_progress(written, total);
    eprintln!(); // close the progress line
    file.flush().await.ok();
    drop(file);
    tokio::fs::rename(&tmp, dest)
        .await
        .with_context(|| format!("rename {} → {}", tmp.display(), dest.display()))?;
    Ok(true)
}

fn read_hf_token() -> Option<String> {
    if let Ok(t) = std::env::var("HF_TOKEN")
        && !t.is_empty()
    {
        return Some(t);
    }
    let home = std::env::var_os("HOME")?;
    let p = std::path::PathBuf::from(home).join(".cache/huggingface/token");
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
