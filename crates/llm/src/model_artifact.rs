// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Pinned metadata for explicitly installed local generation models.

use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// Return the lowercase SHA-256 digest for an in-memory byte slice.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

/// Immutable provenance and integrity metadata for one downloadable artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelArtifactManifest {
    pub id: &'static str,
    pub repository: &'static str,
    pub revision: &'static str,
    pub filename: &'static str,
    pub size_bytes: u64,
    pub sha256: &'static str,
    pub license: &'static str,
    pub license_evidence_url: &'static str,
    pub install_command: &'static str,
}

impl ModelArtifactManifest {
    /// Revision-pinned source URL. Calling this does not perform I/O.
    #[must_use]
    pub fn download_url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repository, self.revision, self.filename
        )
    }
}

/// The supported local generation artifact. "Bundled" means this manifest is
/// bundled; the multi-gigabyte weights are never part of the repository and
/// are never fetched by inference.
pub const BUNDLED_GEMMA: ModelArtifactManifest = ModelArtifactManifest {
    id: "gemma-4-12b-it-qat-q4_0",
    repository: "google/gemma-4-12B-it-qat-q4_0-gguf",
    revision: "ef7b15515d7ed7f34305a08edb5717e7989d6dc9",
    filename: "gemma-4-12b-it-qat-q4_0.gguf",
    size_bytes: 6_975_879_296,
    sha256: "93567e57a8fe10b23569b9d9ec38cd005deedf71e29477c421a4b83f418a538b",
    license: "Apache-2.0",
    license_evidence_url: "https://huggingface.co/google/gemma-4-12B-it-qat-q4_0-gguf/tree/ef7b15515d7ed7f34305a08edb5717e7989d6dc9",
    install_command: "prism models install gemma-4-12b-it-qat-q4_0",
};

/// Verify an explicitly acquired artifact without moving or modifying it.
/// This is intentionally not called by normal inference: hashing a 7 GB file
/// belongs in install/status/benchmark workflows, not every chat request.
pub fn verify_model_artifact(path: &Path, manifest: &ModelArtifactManifest) -> Result<()> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("cannot inspect model artifact {}", path.display()))?;
    if metadata.len() != manifest.size_bytes {
        bail!(
            "model artifact {} has {} bytes; manifest {} requires {} bytes",
            path.display(),
            metadata.len(),
            manifest.id,
            manifest.size_bytes
        );
    }

    let mut file = std::fs::File::open(path)
        .with_context(|| format!("cannot open model artifact {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("cannot hash model artifact {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut actual = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut actual, "{byte:02x}").expect("writing to a String cannot fail");
    }
    if actual != manifest.sha256 {
        bail!(
            "model artifact {} has SHA-256 {actual}; manifest {} requires {}",
            path.display(),
            manifest.id,
            manifest.sha256
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_url_is_revision_pinned() {
        let url = BUNDLED_GEMMA.download_url();
        assert!(url.contains(BUNDLED_GEMMA.revision), "{url}");
        assert!(!url.contains("/main/"), "{url}");
        assert!(
            BUNDLED_GEMMA
                .license_evidence_url
                .contains(BUNDLED_GEMMA.revision)
        );
        assert!(!BUNDLED_GEMMA.license_evidence_url.contains("/main"));
    }

    #[test]
    fn verifier_rejects_size_before_digest() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), b"not weights").unwrap();
        let error = verify_model_artifact(temp.path(), &BUNDLED_GEMMA)
            .unwrap_err()
            .to_string();
        assert!(error.contains("has 11 bytes"), "{error}");
        assert!(error.contains("requires 6975879296 bytes"), "{error}");
    }
}
