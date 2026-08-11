// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Pinned artifact manifest and verification for the native embedder.
//!
//! Normal inference only reads an already-installed snapshot. Model
//! acquisition is a separate, explicit setup action.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Explicit command that installs the pinned native embedding snapshot.
pub const BGE_INSTALL_COMMAND: &str = "prism models install bge-small-en-v1.5";

/// One immutable file in a model snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelArtifact {
    pub path: &'static str,
    pub size_bytes: u64,
    pub sha256: &'static str,
}

/// Immutable acquisition metadata for a model snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSnapshotManifest {
    pub model: &'static str,
    pub repository: &'static str,
    pub revision: &'static str,
    /// License declared by the artifact repository itself. `None` means the
    /// conversion repository does not declare one; it must not inherit the
    /// source model's license by implication.
    pub artifact_license: Option<&'static str>,
    pub artifact_license_url: Option<&'static str>,
    pub source_repository: &'static str,
    pub source_revision: &'static str,
    pub source_license: &'static str,
    pub source_license_url: &'static str,
    pub install_command: &'static str,
    pub files: &'static [ModelArtifact],
}

impl ModelSnapshotManifest {
    /// Revision-pinned source URL for one artifact. Calling this performs no
    /// I/O.
    #[must_use]
    pub fn artifact_url(&self, artifact: &ModelArtifact) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repository, self.revision, artifact.path
        )
    }

    /// Immutable repository view used as provenance for this conversion.
    #[must_use]
    pub fn artifact_revision_url(&self) -> String {
        format!(
            "https://huggingface.co/{}/tree/{}",
            self.repository, self.revision
        )
    }

    /// Immutable source-model view used for source license provenance.
    #[must_use]
    pub fn source_revision_url(&self) -> String {
        format!(
            "https://huggingface.co/{}/tree/{}",
            self.source_repository, self.source_revision
        )
    }

    #[must_use]
    pub fn repository_cache_dir(&self, cache_dir: &Path) -> PathBuf {
        repository_dir(cache_dir, self.repository)
    }

    #[must_use]
    pub fn snapshot_dir(&self, cache_dir: &Path) -> PathBuf {
        self.repository_cache_dir(cache_dir)
            .join("snapshots")
            .join(self.revision)
    }
}

const BGE_FILES: &[ModelArtifact] = &[
    ModelArtifact {
        path: "config.json",
        size_bytes: 683,
        sha256: "fa73f90bf92c8cace1fbcb709626306f2bdbc9ea3e5b5f94b440df9b6aa56350",
    },
    ModelArtifact {
        path: "tokenizer.json",
        size_bytes: 711_396,
        sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
    },
    ModelArtifact {
        path: "tokenizer_config.json",
        size_bytes: 366,
        sha256: "9261e7d79b44c8195c1cada2b453e55b00aeb81e907a6664974b4d7776172ab3",
    },
    ModelArtifact {
        path: "special_tokens_map.json",
        size_bytes: 125,
        sha256: "b6d346be366a7d1d48332dbc9fdf3bf8960b5d879522b7799ddba59e76237ee3",
    },
    ModelArtifact {
        path: "onnx/model.onnx",
        size_bytes: 133_093_490,
        sha256: "828e1496d7fabb79cfa4dcd84fa38625c0d3d21da474a00f08db0f559940cf35",
    },
];

/// The only native embedding snapshot accepted by this build.
pub const BGE_SMALL_EN_V15_MANIFEST: ModelSnapshotManifest = ModelSnapshotManifest {
    model: "bge-small-en-v1.5",
    repository: "Xenova/bge-small-en-v1.5",
    revision: "ea104dacec62c0de699686887e3f920caeb4f3e3",
    artifact_license: None,
    artifact_license_url: None,
    source_repository: "BAAI/bge-small-en-v1.5",
    source_revision: "baab320e3049c6c62dd63560765566dd9083985e",
    source_license: "MIT",
    source_license_url: "https://huggingface.co/BAAI/bge-small-en-v1.5/tree/baab320e3049c6c62dd63560765566dd9083985e",
    install_command: BGE_INSTALL_COMMAND,
    files: BGE_FILES,
};

/// Stable reason codes callers can present without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeModelUnavailableCode {
    CacheUnavailable,
    MissingReference,
    RevisionMismatch,
    MissingArtifact,
    SizeMismatch,
    DigestMismatch,
    ArtifactUnreadable,
}

/// Why the pinned snapshot cannot be used, plus the explicit recovery action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeModelUnavailable {
    pub code: NativeModelUnavailableCode,
    pub detail: String,
    pub install_command: &'static str,
}

impl fmt::Display for NativeModelUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}; install the pinned model explicitly with `{}` (normal inference never downloads weights)",
            self.detail, self.install_command
        )
    }
}

impl std::error::Error for NativeModelUnavailable {}

/// Caller-readable state of the native embedding snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeModelStatus {
    Ready {
        snapshot_dir: PathBuf,
        revision: &'static str,
    },
    Unavailable(NativeModelUnavailable),
}

impl NativeModelStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    pub fn unavailable(&self) -> Option<&NativeModelUnavailable> {
        match self {
            Self::Ready { .. } => None,
            Self::Unavailable(reason) => Some(reason),
        }
    }
}

fn unavailable_reason(
    code: NativeModelUnavailableCode,
    detail: impl Into<String>,
    install_command: &'static str,
) -> NativeModelUnavailable {
    NativeModelUnavailable {
        code,
        detail: detail.into(),
        install_command,
    }
}

fn repository_dir(cache_dir: &Path, repository: &str) -> PathBuf {
    cache_dir.join(format!("models--{}", repository.replace('/', "--")))
}

/// Verified bytes from the exact file handles opened during integrity
/// checking. Runtime consumes these buffers directly instead of reopening
/// their paths, so a symlink swap cannot change what fastembed receives.
pub(crate) struct VerifiedSnapshot {
    pub(crate) snapshot_dir: PathBuf,
    files: BTreeMap<&'static str, Vec<u8>>,
}

impl VerifiedSnapshot {
    pub(crate) fn take(&mut self, path: &str) -> Option<Vec<u8>> {
        self.files.remove(path)
    }
}

/// Verify the exact snapshot that runtime inference is allowed to load.
pub fn native_model_status_at(cache_dir: &Path) -> NativeModelStatus {
    verify_snapshot_at(cache_dir, &BGE_SMALL_EN_V15_MANIFEST)
}

/// Verify any snapshot manifest in a Hugging Face-compatible cache layout.
/// Installers use this on staging directories before publishing them.
pub fn verify_snapshot_at(cache_dir: &Path, manifest: &ModelSnapshotManifest) -> NativeModelStatus {
    match open_verified_snapshot(cache_dir, manifest) {
        Ok(snapshot) => NativeModelStatus::Ready {
            snapshot_dir: snapshot.snapshot_dir,
            revision: manifest.revision,
        },
        Err(reason) => NativeModelStatus::Unavailable(reason),
    }
}

/// Open, read, and verify every artifact, retaining the exact verified bytes
/// for native model construction. Hugging Face cache symlinks are allowed:
/// validation is against bytes read from each opened handle, not symlink
/// metadata or a later traversal of the same path.
pub(crate) fn open_verified_snapshot(
    cache_dir: &Path,
    manifest: &ModelSnapshotManifest,
) -> Result<VerifiedSnapshot, NativeModelUnavailable> {
    open_verified_snapshot_with_hook(cache_dir, manifest, |_, _| {})
}

fn open_verified_snapshot_with_hook(
    cache_dir: &Path,
    manifest: &ModelSnapshotManifest,
    mut after_open: impl FnMut(&ModelArtifact, &Path),
) -> Result<VerifiedSnapshot, NativeModelUnavailable> {
    let repository_dir = repository_dir(cache_dir, manifest.repository);
    let reference_path = repository_dir.join("refs/main");
    let revision = match std::fs::read_to_string(&reference_path) {
        Ok(revision) => revision.trim().to_string(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(unavailable_reason(
                NativeModelUnavailableCode::MissingReference,
                format!(
                    "pinned native embedding snapshot is not installed at {}",
                    repository_dir.display()
                ),
                manifest.install_command,
            ));
        }
        Err(err) => {
            return Err(unavailable_reason(
                NativeModelUnavailableCode::CacheUnavailable,
                format!("cannot read {}: {err}", reference_path.display()),
                manifest.install_command,
            ));
        }
    };
    if revision != manifest.revision {
        return Err(unavailable_reason(
            NativeModelUnavailableCode::RevisionMismatch,
            format!(
                "native embedding revision mismatch at {} (expected {}, found {})",
                reference_path.display(),
                manifest.revision,
                revision
            ),
            manifest.install_command,
        ));
    }

    let snapshot_dir = repository_dir.join("snapshots").join(manifest.revision);
    let mut files = BTreeMap::new();
    for artifact in manifest.files {
        let path = snapshot_dir.join(artifact.path);
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(unavailable_reason(
                    NativeModelUnavailableCode::MissingArtifact,
                    format!("native embedding artifact is missing: {}", path.display()),
                    manifest.install_command,
                ));
            }
            Err(err) => {
                return Err(unavailable_reason(
                    NativeModelUnavailableCode::ArtifactUnreadable,
                    format!("cannot open {}: {err}", path.display()),
                    manifest.install_command,
                ));
            }
        };
        after_open(artifact, &path);
        let metadata = file.metadata().map_err(|err| {
            unavailable_reason(
                NativeModelUnavailableCode::ArtifactUnreadable,
                format!("cannot inspect opened artifact {}: {err}", path.display()),
                manifest.install_command,
            )
        })?;
        if metadata.len() != artifact.size_bytes {
            return Err(unavailable_reason(
                NativeModelUnavailableCode::SizeMismatch,
                format!(
                    "native embedding artifact size mismatch for {} (expected {} bytes, found {})",
                    path.display(),
                    artifact.size_bytes,
                    metadata.len()
                ),
                manifest.install_command,
            ));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(artifact.size_bytes).unwrap_or(0));
        file.read_to_end(&mut bytes).map_err(|err| {
            unavailable_reason(
                NativeModelUnavailableCode::ArtifactUnreadable,
                format!("cannot read opened artifact {}: {err}", path.display()),
                manifest.install_command,
            )
        })?;
        if bytes.len() as u64 != artifact.size_bytes {
            return Err(unavailable_reason(
                NativeModelUnavailableCode::SizeMismatch,
                format!(
                    "native embedding artifact size changed while reading {} (expected {} bytes, read {})",
                    path.display(),
                    artifact.size_bytes,
                    bytes.len()
                ),
                manifest.install_command,
            ));
        }
        let digest = sha256_bytes(&bytes);
        if digest != artifact.sha256 {
            return Err(unavailable_reason(
                NativeModelUnavailableCode::DigestMismatch,
                format!(
                    "native embedding artifact digest mismatch for {} (expected {}, found {})",
                    path.display(),
                    artifact.sha256,
                    digest
                ),
                manifest.install_command,
            ));
        }
        files.insert(artifact.path, bytes);
    }

    Ok(VerifiedSnapshot {
        snapshot_dir,
        files,
    })
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_FILES: &[ModelArtifact] = &[ModelArtifact {
        path: "artifact.bin",
        size_bytes: 3,
        sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    }];
    const TEST_MANIFEST: ModelSnapshotManifest = ModelSnapshotManifest {
        model: "test-model",
        repository: "test/model",
        revision: "0123456789abcdef",
        artifact_license: Some("MIT"),
        artifact_license_url: Some("https://example.invalid/artifact-license"),
        source_repository: "test/model",
        source_revision: "fedcba9876543210",
        source_license: "MIT",
        source_license_url: "https://example.invalid/license",
        install_command: "prism models install test-model",
        files: TEST_FILES,
    };

    fn write_snapshot(root: &Path, content: &[u8]) {
        let repository = repository_dir(root, TEST_MANIFEST.repository);
        std::fs::create_dir_all(repository.join("refs")).unwrap();
        std::fs::write(repository.join("refs/main"), TEST_MANIFEST.revision).unwrap();
        let snapshot = repository.join("snapshots").join(TEST_MANIFEST.revision);
        std::fs::create_dir_all(&snapshot).unwrap();
        std::fs::write(snapshot.join("artifact.bin"), content).unwrap();
    }

    #[test]
    fn exact_snapshot_is_ready() {
        let dir = tempfile::tempdir().unwrap();
        write_snapshot(dir.path(), b"abc");
        assert!(matches!(
            verify_snapshot_at(dir.path(), &TEST_MANIFEST),
            NativeModelStatus::Ready { .. }
        ));
    }

    #[test]
    fn wrong_revision_is_explicitly_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        write_snapshot(dir.path(), b"abc");
        let reference = repository_dir(dir.path(), TEST_MANIFEST.repository).join("refs/main");
        std::fs::write(reference, "different").unwrap();
        let status = verify_snapshot_at(dir.path(), &TEST_MANIFEST);
        let reason = status.unavailable().expect("revision must be refused");
        assert_eq!(reason.code, NativeModelUnavailableCode::RevisionMismatch);
        assert!(reason.to_string().contains(TEST_MANIFEST.install_command));
    }

    #[test]
    fn same_size_corruption_is_a_digest_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        write_snapshot(dir.path(), b"abd");
        let status = verify_snapshot_at(dir.path(), &TEST_MANIFEST);
        assert_eq!(
            status.unavailable().map(|reason| reason.code),
            Some(NativeModelUnavailableCode::DigestMismatch)
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_swap_after_open_cannot_change_verified_runtime_bytes() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let repository = repository_dir(dir.path(), TEST_MANIFEST.repository);
        std::fs::create_dir_all(repository.join("refs")).unwrap();
        std::fs::write(repository.join("refs/main"), TEST_MANIFEST.revision).unwrap();
        let snapshot = repository.join("snapshots").join(TEST_MANIFEST.revision);
        std::fs::create_dir_all(&snapshot).unwrap();

        let objects = dir.path().join("objects");
        std::fs::create_dir_all(&objects).unwrap();
        let good = objects.join("good");
        let bad = objects.join("bad");
        std::fs::write(&good, b"abc").unwrap();
        std::fs::write(&bad, b"abd").unwrap();
        let artifact_path = snapshot.join("artifact.bin");
        symlink(&good, &artifact_path).unwrap();

        let mut swapped = false;
        let mut verified =
            open_verified_snapshot_with_hook(dir.path(), &TEST_MANIFEST, |_, opened_path| {
                if !swapped {
                    std::fs::remove_file(opened_path).unwrap();
                    symlink(&bad, opened_path).unwrap();
                    swapped = true;
                }
            })
            .expect("the already-open good object should verify");
        assert_eq!(verified.take("artifact.bin").unwrap(), b"abc");

        let later = verify_snapshot_at(dir.path(), &TEST_MANIFEST);
        assert_eq!(
            later.unavailable().map(|reason| reason.code),
            Some(NativeModelUnavailableCode::DigestMismatch),
            "a later traversal must see and reject the swapped target"
        );
    }

    #[test]
    fn production_provenance_urls_are_revision_pinned() {
        let artifact = BGE_SMALL_EN_V15_MANIFEST.artifact_revision_url();
        let source = BGE_SMALL_EN_V15_MANIFEST.source_revision_url();
        assert!(artifact.contains(BGE_SMALL_EN_V15_MANIFEST.revision));
        assert!(source.contains(BGE_SMALL_EN_V15_MANIFEST.source_revision));
        assert!(
            BGE_SMALL_EN_V15_MANIFEST
                .source_license_url
                .contains(BGE_SMALL_EN_V15_MANIFEST.source_revision)
        );
        assert!(
            !BGE_SMALL_EN_V15_MANIFEST
                .source_license_url
                .contains("/main")
        );
    }
}
