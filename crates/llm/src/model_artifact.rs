// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Pinned metadata for explicitly installed local generation models.

use std::fmt::Write as _;
use std::io::{Read, Seek};
use std::path::Path;
#[cfg(feature = "local-inference")]
use std::path::PathBuf;

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

#[derive(Clone, Debug, Eq, PartialEq)]
struct StableFileMetadata {
    size_bytes: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    change_time_seconds: i64,
    #[cfg(unix)]
    change_time_nanoseconds: i64,
}

impl StableFileMetadata {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;

        Self {
            size_bytes: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            change_time_seconds: metadata.ctime(),
            #[cfg(unix)]
            change_time_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileFingerprint {
    sha256: String,
    metadata: StableFileMetadata,
}

fn hash_open_file(file: &mut std::fs::File, display_path: &Path) -> Result<FileFingerprint> {
    let before =
        StableFileMetadata::from_metadata(&file.metadata().with_context(|| {
            format!("cannot inspect model artifact {}", display_path.display())
        })?);
    file.rewind()
        .with_context(|| format!("cannot rewind model artifact {}", display_path.display()))?;

    let mut hasher = Sha256::new();
    let mut bytes_read = 0_u64;
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("cannot hash model artifact {}", display_path.display()))?;
        if read == 0 {
            break;
        }
        bytes_read = bytes_read
            .checked_add(read as u64)
            .context("model artifact byte count overflowed u64")?;
        hasher.update(&buffer[..read]);
    }

    let after = StableFileMetadata::from_metadata(&file.metadata().with_context(|| {
        format!(
            "cannot re-inspect model artifact {}",
            display_path.display()
        )
    })?);
    if before != after || bytes_read != before.size_bytes {
        bail!(
            "model artifact {} changed while its identity was being computed",
            display_path.display()
        );
    }

    let digest = hasher.finalize();
    let mut sha256 = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut sha256, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(FileFingerprint {
        sha256,
        metadata: after,
    })
}

fn fingerprint_model_file(path: &Path) -> Result<FileFingerprint> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("cannot open model artifact {}", path.display()))?;
    hash_open_file(&mut file, path)
}

#[cfg(all(feature = "local-inference", unix))]
fn stable_open_file_path(file: &std::fs::File, _original: &Path) -> PathBuf {
    use std::os::fd::AsRawFd as _;

    #[cfg(any(target_os = "linux", target_os = "android"))]
    let directory = "/proc/self/fd";
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let directory = "/dev/fd";
    Path::new(directory).join(file.as_raw_fd().to_string())
}

#[cfg(all(feature = "local-inference", not(unix)))]
fn stable_open_file_path(_file: &std::fs::File, original: &Path) -> PathBuf {
    original.to_path_buf()
}

/// The open file and stable metadata observed around model initialization.
/// On Unix, descriptor-backed loading makes this the exact source used by
/// llama.cpp, and retaining it lets a later scorer hash that inode. Other
/// targets retain the observation only for diagnostics and cannot mint an
/// identity receipt from it.
#[derive(Debug)]
#[cfg(feature = "local-inference")]
pub(crate) struct LoadedModelFile {
    source: std::fs::File,
    source_path: PathBuf,
    metadata: StableFileMetadata,
}

#[cfg(feature = "local-inference")]
impl LoadedModelFile {
    pub(crate) fn ensure_retained_file_unchanged(&self) -> Result<()> {
        let current =
            StableFileMetadata::from_metadata(&self.source.metadata().with_context(|| {
                format!(
                    "cannot inspect retained model artifact {}",
                    self.source_path.display()
                )
            })?);
        if current != self.metadata {
            bail!(
                "loaded model artifact {} changed after this client initialized it",
                self.source_path.display()
            );
        }
        Ok(())
    }

    /// Hash the retained source inode once and require it, plus the currently
    /// resolved path, to still match the metadata captured around model load.
    #[cfg(unix)]
    pub(crate) fn verify_identity(&self) -> Result<crate::LocalModelIdentity> {
        self.ensure_retained_file_unchanged()?;
        let current_metadata = std::fs::metadata(&self.source_path).with_context(|| {
            format!(
                "cannot re-inspect loaded model artifact {}",
                self.source_path.display()
            )
        })?;
        if StableFileMetadata::from_metadata(&current_metadata) != self.metadata {
            bail!(
                "loaded model artifact path {} no longer identifies the file used to initialize this client",
                self.source_path.display()
            );
        }

        let mut source = self.source.try_clone().with_context(|| {
            format!(
                "cannot duplicate loaded model artifact {} for verification",
                self.source_path.display()
            )
        })?;
        let fingerprint = hash_open_file(&mut source, &self.source_path)?;
        if fingerprint.metadata != self.metadata {
            bail!(
                "loaded model artifact {} changed after this client initialized it",
                self.source_path.display()
            );
        }

        let current_metadata = std::fs::metadata(&self.source_path).with_context(|| {
            format!(
                "cannot re-inspect loaded model artifact {} after verification",
                self.source_path.display()
            )
        })?;
        if StableFileMetadata::from_metadata(&current_metadata) != self.metadata {
            bail!(
                "loaded model artifact path {} was replaced while its identity was being verified",
                self.source_path.display()
            );
        }

        Ok(crate::LocalModelIdentity {
            sha256: fingerprint.sha256,
            size_bytes: fingerprint.metadata.size_bytes,
        })
    }

    /// Non-Unix targets currently have no descriptor path that can be handed
    /// to llama.cpp. Path-only before/after checks cannot prove which bytes the
    /// loader observed, so they must never produce a verified receipt.
    #[cfg(not(unix))]
    pub(crate) fn verify_identity(&self) -> Result<crate::LocalModelIdentity> {
        bail!(
            "descriptor-backed loaded-model identity is unavailable on this target; path-only verification was refused"
        )
    }
}

/// Load through an already-open file while capturing cheap, stable identity
/// metadata. Hashing remains lazy and is only requested by identity/scoring
/// APIs, so ordinary local inference does not pay an extra multi-gigabyte read.
///
/// On Unix, `loader` receives a descriptor-backed path, preventing a rename or
/// symlink swap of `path` from redirecting the model loader between checks.
/// Other targets receive the original path for ordinary generation only;
/// their public identity and influence APIs explicitly return unavailable and
/// `LoadedModelFile::verify_identity` refuses to mint a receipt.
#[cfg(feature = "local-inference")]
pub(crate) fn load_with_stable_identity<T>(
    path: &Path,
    loader: impl FnOnce(&Path) -> Result<T>,
) -> Result<(T, LoadedModelFile)> {
    let source = std::fs::File::open(path)
        .with_context(|| format!("cannot open model artifact {}", path.display()))?;
    let before = StableFileMetadata::from_metadata(
        &source
            .metadata()
            .with_context(|| format!("cannot inspect model artifact {}", path.display()))?,
    );
    let stable_path = stable_open_file_path(&source, path);
    let loaded = loader(&stable_path)?;
    let after = StableFileMetadata::from_metadata(
        &source
            .metadata()
            .with_context(|| format!("cannot re-inspect model artifact {}", path.display()))?,
    );

    if before != after {
        bail!(
            "model artifact {} changed while the local model was loading",
            path.display()
        );
    }

    let current_metadata = std::fs::metadata(path)
        .with_context(|| format!("cannot re-inspect model artifact {}", path.display()))?;
    if StableFileMetadata::from_metadata(&current_metadata) != after {
        bail!(
            "model artifact path {} was replaced while the local model was loading",
            path.display()
        );
    }

    Ok((
        loaded,
        LoadedModelFile {
            source,
            source_path: path.to_path_buf(),
            metadata: after,
        },
    ))
}

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
    let actual = fingerprint_model_file(path)?;
    if actual.sha256 != manifest.sha256 {
        bail!(
            "model artifact {} has SHA-256 {}; manifest {} requires {}",
            path.display(),
            actual.sha256,
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

    #[cfg(all(feature = "local-inference", unix))]
    #[test]
    fn verified_load_returns_the_digest_of_the_loaded_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tiny.gguf");
        std::fs::write(&path, b"first model bytes").unwrap();

        let (loaded, source) =
            load_with_stable_identity(&path, |path| Ok(std::fs::read(path)?)).unwrap();
        let identity = source.verify_identity().unwrap();

        assert_eq!(loaded, b"first model bytes");
        assert_eq!(identity.sha256, sha256_hex(b"first model bytes"));
        assert_eq!(identity.size_bytes, 17);
    }

    #[cfg(all(feature = "local-inference", unix))]
    #[test]
    fn verified_load_rejects_a_same_path_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tiny.gguf");
        let replacement = temp.path().join("replacement.gguf");
        std::fs::write(&path, b"first model bytes").unwrap();
        std::fs::write(&replacement, b"other model bytes").unwrap();

        let error = load_with_stable_identity(&path, |stable_path| {
            let loaded = std::fs::read(stable_path)?;
            std::fs::rename(&replacement, &path)?;
            Ok(loaded)
        })
        .unwrap_err()
        .to_string();

        assert!(error.contains("model artifact"), "{error}");
        assert!(
            error.contains("replaced") || error.contains("changed"),
            "{error}"
        );
    }

    #[cfg(all(feature = "local-inference", unix))]
    #[test]
    fn separate_loads_do_not_reuse_a_stale_same_path_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tiny.gguf");
        std::fs::write(&path, b"first model bytes").unwrap();
        let (_, first_source) =
            load_with_stable_identity(&path, |path| Ok(std::fs::read(path)?)).unwrap();
        let first = first_source.verify_identity().unwrap();

        std::fs::write(&path, b"other model bytes").unwrap();
        let (_, second_source) =
            load_with_stable_identity(&path, |path| Ok(std::fs::read(path)?)).unwrap();
        let second = second_source.verify_identity().unwrap();

        assert_ne!(first.sha256, second.sha256);
        assert_eq!(second.sha256, sha256_hex(b"other model bytes"));
    }

    #[cfg(all(feature = "local-inference", unix))]
    #[test]
    fn loaded_source_rejects_replacement_before_lazy_verification() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tiny.gguf");
        std::fs::write(&path, b"first model bytes").unwrap();
        let (_, source) =
            load_with_stable_identity(&path, |path| Ok(std::fs::read(path)?)).unwrap();

        std::fs::write(&path, b"other model bytes").unwrap();
        let error = source.verify_identity().unwrap_err().to_string();

        assert!(
            error.contains("no longer identifies") || error.contains("changed after"),
            "{error}"
        );
    }

    #[cfg(all(feature = "local-inference", not(unix)))]
    #[test]
    fn path_only_loaded_source_cannot_mint_an_identity_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tiny.gguf");
        std::fs::write(&path, b"first model bytes").unwrap();
        let (_, source) =
            load_with_stable_identity(&path, |path| Ok(std::fs::read(path)?)).unwrap();

        let error = source.verify_identity().unwrap_err().to_string();

        assert!(error.contains("descriptor-backed"), "{error}");
        assert!(error.contains("path-only"), "{error}");
    }
}
