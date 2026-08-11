// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Explicit, pinned model acquisition for `prism models install`.
//!
//! This module is the only model-weight downloader in the CLI. Normal
//! inference and doctor checks never call it.

use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use fs4::fs_std::FileExt;
use prism_embed::{
    BGE_SMALL_EN_V15_MANIFEST, ModelSnapshotManifest, NativeModelStatus, verify_snapshot_at,
};
use prism_llm::{BUNDLED_GEMMA, ModelArtifactManifest, verify_model_artifact};
use serde::Serialize;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum InstallableModel {
    #[value(name = "bge-small-en-v1.5")]
    BgeSmallEnV15,
    #[value(name = "gemma-4-12b-it-qat-q4_0")]
    Gemma4_12bItQatQ40,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallDisposition {
    Installed,
    AlreadyInstalled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallReport {
    pub status: InstallDisposition,
    pub model: &'static str,
    pub path: PathBuf,
    pub repository: &'static str,
    pub revision: &'static str,
    pub artifact_revision_url: String,
    pub files_verified: usize,
    pub bytes_verified: u64,
    pub sha256: Option<&'static str>,
    pub artifact_license: Option<&'static str>,
    pub artifact_license_evidence_url: Option<&'static str>,
    pub source_model_repository: Option<&'static str>,
    pub source_model_revision: Option<&'static str>,
    pub source_model_revision_url: Option<String>,
    pub source_model_license: Option<&'static str>,
    pub source_model_license_evidence_url: Option<&'static str>,
}

#[derive(Debug, Clone)]
struct InstallLocations {
    generation_dir: PathBuf,
    embedding_cache_dir: PathBuf,
}

impl InstallLocations {
    fn discover() -> Result<Self> {
        Ok(Self {
            generation_dir: prism_llm::default_model_dir()?,
            embedding_cache_dir: prism_embed::default_cache_dir()?,
        })
    }
}

#[derive(Clone, Copy)]
struct FetchRequest<'a> {
    url: &'a str,
    expected_bytes: u64,
}

type FetchFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

trait ArtifactFetcher: Sync {
    fn fetch<'a>(&'a self, request: FetchRequest<'a>, destination: &'a Path) -> FetchFuture<'a>;
}

struct HttpFetcher {
    client: reqwest::Client,
}

impl HttpFetcher {
    fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .user_agent(concat!("prism/", env!("CARGO_PKG_VERSION")))
                .build()
                .context("cannot build the model installer HTTP client")?,
        })
    }
}

impl ArtifactFetcher for HttpFetcher {
    fn fetch<'a>(&'a self, request: FetchRequest<'a>, destination: &'a Path) -> FetchFuture<'a> {
        Box::pin(async move {
            eprintln!(
                "[prism] fetching pinned artifact {} ({} bytes)",
                request.url, request.expected_bytes
            );
            let mut response = self
                .client
                .get(request.url)
                .send()
                .await
                .with_context(|| format!("cannot fetch pinned artifact {}", request.url))?
                .error_for_status()
                .with_context(|| format!("model host refused pinned artifact {}", request.url))?;
            if let Some(length) = response.content_length()
                && length != request.expected_bytes
            {
                bail!(
                    "pinned artifact {} advertised {length} bytes; manifest requires {}",
                    request.url,
                    request.expected_bytes
                );
            }

            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)
                .await
                .with_context(|| format!("cannot create staging file {}", destination.display()))?;
            let mut downloaded = 0_u64;
            while let Some(chunk) = response
                .chunk()
                .await
                .with_context(|| format!("download interrupted for {}", request.url))?
            {
                downloaded = downloaded
                    .checked_add(chunk.len() as u64)
                    .context("download byte count overflowed")?;
                if downloaded > request.expected_bytes {
                    bail!(
                        "pinned artifact {} exceeded its manifest size of {} bytes",
                        request.url,
                        request.expected_bytes
                    );
                }
                file.write_all(&chunk).await.with_context(|| {
                    format!("cannot write staging file {}", destination.display())
                })?;
            }
            file.flush()
                .await
                .with_context(|| format!("cannot flush staging file {}", destination.display()))?;
            file.sync_all()
                .await
                .with_context(|| format!("cannot sync staging file {}", destination.display()))?;
            if downloaded != request.expected_bytes {
                bail!(
                    "pinned artifact {} downloaded {downloaded} bytes; manifest requires {}",
                    request.url,
                    request.expected_bytes
                );
            }
            eprintln!(
                "[prism] staged and size-checked {} ({} bytes)",
                request.url, downloaded
            );
            Ok(())
        })
    }
}

/// Run the explicit acquisition command. This is never called by inference or
/// doctor.
pub async fn install(model: InstallableModel) -> Result<InstallReport> {
    ensure_install_allowed()?;
    let locations = InstallLocations::discover()?;
    let fetcher = HttpFetcher::new()?;
    install_with_fetch(model, &locations, &fetcher).await
}

fn ensure_install_allowed() -> Result<()> {
    if prism_runtime::offline::enabled() {
        bail!(
            "model installation is an explicit network action and is disabled by PRISM offline mode; retry without `--offline`"
        );
    }
    Ok(())
}

async fn install_with_fetch(
    model: InstallableModel,
    locations: &InstallLocations,
    fetcher: &dyn ArtifactFetcher,
) -> Result<InstallReport> {
    match model {
        InstallableModel::BgeSmallEnV15 => {
            install_snapshot(
                &BGE_SMALL_EN_V15_MANIFEST,
                &locations.embedding_cache_dir,
                fetcher,
            )
            .await
        }
        InstallableModel::Gemma4_12bItQatQ40 => {
            install_single_artifact(&BUNDLED_GEMMA, &locations.generation_dir, fetcher).await
        }
    }
}

async fn install_single_artifact(
    manifest: &'static ModelArtifactManifest,
    model_dir: &Path,
    fetcher: &dyn ArtifactFetcher,
) -> Result<InstallReport> {
    std::fs::create_dir_all(model_dir)
        .with_context(|| format!("cannot create model directory {}", model_dir.display()))?;
    let _lock = InstallLock::acquire(model_dir, manifest.id)?;
    let target = model_dir.join(manifest.filename);
    if target.exists() {
        match verify_model_artifact(&target, manifest) {
            Ok(()) => {
                return Ok(generation_report(
                    manifest,
                    target,
                    InstallDisposition::AlreadyInstalled,
                ));
            }
            Err(error) => bail!(
                "{} exists but does not match the pinned {} manifest: {error:#}. It was not modified. Move it aside, then rerun `{}`",
                target.display(),
                manifest.id,
                manifest.install_command
            ),
        }
    }

    let staging = StagingDir::new(model_dir, manifest.id)?;
    let staged_file = staging.path().join(manifest.filename);
    let url = manifest.download_url();
    fetcher
        .fetch(
            FetchRequest {
                url: &url,
                expected_bytes: manifest.size_bytes,
            },
            &staged_file,
        )
        .await
        .with_context(|| {
            format!(
                "failed to acquire {}; nothing was published. Retry `{}`",
                manifest.id, manifest.install_command
            )
        })?;
    sync_file(&staged_file)?;
    verify_model_artifact(&staged_file, manifest).with_context(|| {
        format!(
            "downloaded {} failed manifest verification; nothing was published",
            manifest.id
        )
    })?;
    sync_tree_directories(staging.path())?;
    sync_directory(model_dir)?;
    publish_noreplace(&staged_file, &target).with_context(|| {
        format!(
            "verified {} but could not atomically publish it without replacement to {}; a racing target was never overwritten",
            manifest.id,
            target.display()
        )
    })?;
    sync_directory(model_dir)?;
    Ok(generation_report(
        manifest,
        target,
        InstallDisposition::Installed,
    ))
}

fn generation_report(
    manifest: &'static ModelArtifactManifest,
    path: PathBuf,
    status: InstallDisposition,
) -> InstallReport {
    InstallReport {
        status,
        model: manifest.id,
        path,
        repository: manifest.repository,
        revision: manifest.revision,
        artifact_revision_url: format!(
            "https://huggingface.co/{}/tree/{}",
            manifest.repository, manifest.revision
        ),
        files_verified: 1,
        bytes_verified: manifest.size_bytes,
        sha256: Some(manifest.sha256),
        artifact_license: Some(manifest.license),
        artifact_license_evidence_url: Some(manifest.license_evidence_url),
        source_model_repository: None,
        source_model_revision: None,
        source_model_revision_url: None,
        source_model_license: None,
        source_model_license_evidence_url: None,
    }
}

async fn install_snapshot(
    manifest: &'static ModelSnapshotManifest,
    cache_dir: &Path,
    fetcher: &dyn ArtifactFetcher,
) -> Result<InstallReport> {
    std::fs::create_dir_all(cache_dir)
        .with_context(|| format!("cannot create model cache {}", cache_dir.display()))?;
    let _lock = InstallLock::acquire(cache_dir, manifest.model)?;
    if let NativeModelStatus::Ready { snapshot_dir, .. } = verify_snapshot_at(cache_dir, manifest) {
        return Ok(snapshot_report(
            manifest,
            snapshot_dir,
            InstallDisposition::AlreadyInstalled,
        ));
    }
    let final_repository = manifest.repository_cache_dir(cache_dir);
    if final_repository.exists() {
        let reason = verify_snapshot_at(cache_dir, manifest)
            .unavailable()
            .map(ToString::to_string)
            .unwrap_or_else(|| "the existing repository is not the requested snapshot".to_string());
        bail!(
            "{} exists but is not a verified {} install: {reason}. It was not modified. Move it aside, then rerun `{}`",
            final_repository.display(),
            manifest.model,
            manifest.install_command
        );
    }

    let staging = StagingDir::new(cache_dir, manifest.model)?;
    let staged_repository = manifest.repository_cache_dir(staging.path());
    let staged_snapshot = manifest.snapshot_dir(staging.path());
    std::fs::create_dir_all(&staged_snapshot).with_context(|| {
        format!(
            "cannot create snapshot staging directory {}",
            staged_snapshot.display()
        )
    })?;

    for artifact in manifest.files {
        let destination = staged_snapshot.join(artifact.path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create staging directory {}", parent.display()))?;
        }
        let url = manifest.artifact_url(artifact);
        fetcher
            .fetch(
                FetchRequest {
                    url: &url,
                    expected_bytes: artifact.size_bytes,
                },
                &destination,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to acquire {} artifact {}; nothing was published. Retry `{}`",
                    manifest.model, artifact.path, manifest.install_command
                )
            })?;
        sync_file(&destination)?;
    }
    let refs = staged_repository.join("refs");
    std::fs::create_dir_all(&refs)
        .with_context(|| format!("cannot create staging refs directory {}", refs.display()))?;
    write_synced(&refs.join("main"), manifest.revision.as_bytes())?;

    match verify_snapshot_at(staging.path(), manifest) {
        NativeModelStatus::Ready { .. } => {}
        NativeModelStatus::Unavailable(reason) => bail!(
            "downloaded {} failed manifest verification: {reason}; nothing was published",
            manifest.model
        ),
    }
    sync_tree_directories(staging.path())?;
    sync_directory(cache_dir)?;
    publish_noreplace(&staged_repository, &final_repository).with_context(|| {
        format!(
            "verified {} but could not atomically publish it without replacement to {}; a racing target was never overwritten",
            manifest.model,
            final_repository.display()
        )
    })?;
    sync_directory(cache_dir)?;
    Ok(snapshot_report(
        manifest,
        manifest.snapshot_dir(cache_dir),
        InstallDisposition::Installed,
    ))
}

fn snapshot_report(
    manifest: &'static ModelSnapshotManifest,
    path: PathBuf,
    status: InstallDisposition,
) -> InstallReport {
    InstallReport {
        status,
        model: manifest.model,
        path,
        repository: manifest.repository,
        revision: manifest.revision,
        artifact_revision_url: manifest.artifact_revision_url(),
        files_verified: manifest.files.len(),
        bytes_verified: manifest.files.iter().map(|file| file.size_bytes).sum(),
        sha256: None,
        artifact_license: manifest.artifact_license,
        artifact_license_evidence_url: manifest.artifact_license_url,
        source_model_repository: Some(manifest.source_repository),
        source_model_revision: Some(manifest.source_revision),
        source_model_revision_url: Some(manifest.source_revision_url()),
        source_model_license: Some(manifest.source_license),
        source_model_license_evidence_url: Some(manifest.source_license_url),
    }
}

/// Process-coordinating lock for one destination model. The lock file stays
/// in place: deleting it would let a third process lock a different inode
/// while an earlier installer still held the original one.
struct InstallLock {
    file: File,
    path: PathBuf,
}

impl InstallLock {
    fn acquire(parent: &Path, model: &str) -> Result<Self> {
        let safe_model: String = model
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        let path = parent.join(format!(".prism-install-{safe_model}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("cannot open model install lock {}", path.display()))?;
        FileExt::lock_exclusive(&file)
            .with_context(|| format!("cannot lock model installation at {}", path.display()))?;
        Ok(Self { file, path })
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.file) {
            tracing::warn!(
                path = %self.path.display(),
                "could not release model installer lock: {error}"
            );
        }
    }
}

/// Publish a staged file or directory without ever replacing an existing
/// target. Staging is always below the destination parent, so publication is
/// constrained to one filesystem.
fn publish_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    publish_noreplace_with_hook(source, destination, || {})
}

fn publish_noreplace_with_hook(
    source: &Path,
    destination: &Path,
    before_publish: impl FnOnce(),
) -> io::Result<()> {
    before_publish();
    rename_noreplace(source, destination)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn path_c_string(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt as _;

    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path contains an interior NUL byte: {}", path.display()),
        )
    })
}

#[cfg(target_os = "linux")]
fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    let source_c = path_c_string(source)?;
    let destination_c = path_c_string(destination)?;
    // SAFETY: both pointers come from live CStrings, AT_FDCWD requires no
    // directory file descriptor, and RENAME_NOREPLACE is the only flag.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source_c.as_ptr(),
            libc::AT_FDCWD,
            destination_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(libc::ENOSYS) | Some(libc::EINVAL)
        ) {
            portable_publish_noreplace(source, destination)
        } else {
            Err(error)
        }
    }
}

#[cfg(target_os = "macos")]
fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    let source_c = path_c_string(source)?;
    let destination_c = path_c_string(destination)?;
    // SAFETY: both pointers come from live CStrings and RENAME_EXCL requests
    // the no-replacement variant of the platform rename operation.
    let result =
        unsafe { libc::renamex_np(source_c.as_ptr(), destination_c.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(libc::ENOSYS) | Some(libc::ENOTSUP)
        ) {
            portable_publish_noreplace(source, destination)
        } else {
            Err(error)
        }
    }
}

#[cfg(target_os = "windows")]
fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    // std::fs::rename on Windows refuses an existing destination.
    std::fs::rename(source, destination)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn rename_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    portable_publish_noreplace(source, destination)
}

#[cfg(not(target_os = "windows"))]
fn portable_publish_noreplace(source: &Path, destination: &Path) -> io::Result<()> {
    if source.symlink_metadata()?.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace directory publication is unsupported on this platform",
        ));
    }
    // Creating the destination hard link is atomic and fails if it exists.
    // If source cleanup fails both names refer to the same verified bytes;
    // most importantly, no racing target was overwritten.
    std::fs::hard_link(source, destination)?;
    std::fs::remove_file(source)
}

fn sync_tree_directories(root: &Path) -> Result<()> {
    fn visit(path: &Path, directories: &mut Vec<PathBuf>) -> io::Result<()> {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                visit(&entry.path(), directories)?;
            }
        }
        directories.push(path.to_path_buf());
        Ok(())
    }

    let mut directories = Vec::new();
    visit(root, &mut directories).with_context(|| {
        format!(
            "cannot enumerate staged directories under {}",
            root.display()
        )
    })?;
    for directory in directories {
        sync_directory(&directory)?;
    }
    Ok(())
}

fn sync_file(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("cannot open staged file for sync {}", path.display()))?
        .sync_all()
        .with_context(|| format!("cannot sync staged file {}", path.display()))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("cannot open directory for sync {}", path.display()))?
        .sync_all()
        .with_context(|| format!("cannot sync directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    // The supported Windows rename primitive flushes directory metadata as
    // part of the filesystem operation; Rust cannot portably open directories
    // for fsync there.
    Ok(())
}

fn write_synced(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("cannot create staging metadata {}", path.display()))?;
    file.write_all(contents)
        .with_context(|| format!("cannot write staging metadata {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("cannot sync staging metadata {}", path.display()))
}

struct StagingDir {
    path: PathBuf,
    parent: PathBuf,
}

impl StagingDir {
    fn new(parent: &Path, label: &str) -> Result<Self> {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for attempt in 0..32_u8 {
            let path = parent.join(format!(
                ".prism-install-{label}-{}-{epoch}-{attempt}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        parent: parent.to_path_buf(),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("cannot create staging directory {}", path.display())
                    });
                }
            }
        }
        bail!(
            "cannot allocate a unique model staging directory below {}",
            parent.display()
        )
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.path.display(),
                "could not clean model installer staging directory: {error}"
            );
        }
        if let Err(error) = sync_directory(&self.parent) {
            tracing::warn!(
                path = %self.parent.display(),
                "could not sync model directory after staging cleanup: {error:#}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_embed::{ModelArtifact, NativeModelUnavailableCode, native_model_status_at};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    const SHA_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const TEST_GGUF: ModelArtifactManifest = ModelArtifactManifest {
        id: "tiny-generation",
        repository: "test/generation",
        revision: "1111111111111111111111111111111111111111",
        filename: "tiny.gguf",
        size_bytes: 3,
        sha256: SHA_ABC,
        license: "Apache-2.0",
        license_evidence_url: "https://example.invalid/test/generation/tree/1111111111111111111111111111111111111111",
        install_command: "prism models install tiny-generation",
    };
    const TEST_SNAPSHOT_FILES: &[ModelArtifact] = &[
        ModelArtifact {
            path: "config.json",
            size_bytes: 3,
            sha256: SHA_ABC,
        },
        ModelArtifact {
            path: "onnx/model.onnx",
            size_bytes: 3,
            sha256: SHA_ABC,
        },
    ];
    const TEST_SNAPSHOT: ModelSnapshotManifest = ModelSnapshotManifest {
        model: "tiny-embedding",
        repository: "test/embedding",
        revision: "2222222222222222222222222222222222222222",
        artifact_license: None,
        artifact_license_url: None,
        source_repository: "source/embedding",
        source_revision: "3333333333333333333333333333333333333333",
        source_license: "MIT",
        source_license_url: "https://example.invalid/source/embedding/tree/3333333333333333333333333333333333333333",
        install_command: "prism models install tiny-embedding",
        files: TEST_SNAPSHOT_FILES,
    };

    #[derive(Default)]
    struct FakeFetcher {
        artifacts: BTreeMap<String, Vec<u8>>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeFetcher {
        fn with(mut self, url: String, bytes: &[u8]) -> Self {
            self.artifacts.insert(url, bytes.to_vec());
            self
        }
    }

    impl ArtifactFetcher for FakeFetcher {
        fn fetch<'a>(
            &'a self,
            request: FetchRequest<'a>,
            destination: &'a Path,
        ) -> FetchFuture<'a> {
            self.calls.lock().unwrap().push(request.url.to_string());
            let content = self.artifacts.get(request.url).cloned();
            Box::pin(async move {
                let content =
                    content.with_context(|| format!("fake has no artifact for {}", request.url))?;
                if content.len() as u64 != request.expected_bytes {
                    bail!("fake artifact length mismatch");
                }
                tokio::fs::write(destination, content).await?;
                Ok(())
            })
        }
    }

    fn locations(root: &Path) -> InstallLocations {
        InstallLocations {
            generation_dir: root.join("models"),
            embedding_cache_dir: root.join("models/embed"),
        }
    }

    #[tokio::test]
    async fn single_artifact_is_verified_before_atomic_publish() {
        let root = tempfile::tempdir().unwrap();
        let fetcher = FakeFetcher::default().with(TEST_GGUF.download_url(), b"abc");
        let report =
            install_single_artifact(&TEST_GGUF, &locations(root.path()).generation_dir, &fetcher)
                .await
                .unwrap();
        assert_eq!(report.status, InstallDisposition::Installed);
        assert_eq!(std::fs::read(&report.path).unwrap(), b"abc");
        verify_model_artifact(&report.path, &TEST_GGUF).unwrap();

        let no_fetch = FakeFetcher::default();
        let second = install_single_artifact(
            &TEST_GGUF,
            &locations(root.path()).generation_dir,
            &no_fetch,
        )
        .await
        .unwrap();
        assert_eq!(second.status, InstallDisposition::AlreadyInstalled);
        assert!(no_fetch.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn corrupt_single_artifact_is_never_published() {
        let root = tempfile::tempdir().unwrap();
        let locations = locations(root.path());
        let fetcher = FakeFetcher::default().with(TEST_GGUF.download_url(), b"abd");
        let error = install_single_artifact(&TEST_GGUF, &locations.generation_dir, &fetcher)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("failed manifest verification"));
        assert!(!locations.generation_dir.join(TEST_GGUF.filename).exists());
        assert!(
            std::fs::read_dir(&locations.generation_dir)
                .unwrap()
                .all(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".lock"))
        );
    }

    #[tokio::test]
    async fn snapshot_is_verified_before_repository_publish() {
        let root = tempfile::tempdir().unwrap();
        let locations = locations(root.path());
        let mut fetcher = FakeFetcher::default();
        for artifact in TEST_SNAPSHOT.files {
            fetcher = fetcher.with(TEST_SNAPSHOT.artifact_url(artifact), b"abc");
        }
        let report = install_snapshot(&TEST_SNAPSHOT, &locations.embedding_cache_dir, &fetcher)
            .await
            .unwrap();
        assert_eq!(report.status, InstallDisposition::Installed);
        assert!(matches!(
            verify_snapshot_at(&locations.embedding_cache_dir, &TEST_SNAPSHOT),
            NativeModelStatus::Ready { .. }
        ));
        assert_eq!(report.artifact_license, None);
        assert_eq!(report.source_model_license, Some("MIT"));

        let no_fetch = FakeFetcher::default();
        let second = install_snapshot(&TEST_SNAPSHOT, &locations.embedding_cache_dir, &no_fetch)
            .await
            .unwrap();
        assert_eq!(second.status, InstallDisposition::AlreadyInstalled);
        assert!(no_fetch.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_existing_repository_is_left_untouched() {
        let root = tempfile::tempdir().unwrap();
        let locations = locations(root.path());
        let existing = TEST_SNAPSHOT.repository_cache_dir(&locations.embedding_cache_dir);
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::write(existing.join("keep"), b"user data").unwrap();
        let error = install_snapshot(
            &TEST_SNAPSHOT,
            &locations.embedding_cache_dir,
            &FakeFetcher::default(),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("was not modified"));
        assert_eq!(std::fs::read(existing.join("keep")).unwrap(), b"user data");
    }

    #[test]
    fn production_urls_are_revision_pinned_and_licenses_are_not_conflated() {
        for artifact in BGE_SMALL_EN_V15_MANIFEST.files {
            let url = BGE_SMALL_EN_V15_MANIFEST.artifact_url(artifact);
            assert!(url.contains(BGE_SMALL_EN_V15_MANIFEST.revision), "{url}");
            assert!(!url.contains("/main/"), "{url}");
        }
        assert_eq!(BGE_SMALL_EN_V15_MANIFEST.artifact_license, None);
        assert_eq!(BGE_SMALL_EN_V15_MANIFEST.source_license, "MIT");
        assert!(
            BGE_SMALL_EN_V15_MANIFEST
                .source_license_url
                .contains(BGE_SMALL_EN_V15_MANIFEST.source_revision)
        );
        assert!(
            BUNDLED_GEMMA
                .license_evidence_url
                .contains(BUNDLED_GEMMA.revision)
        );
        let report = snapshot_report(
            &BGE_SMALL_EN_V15_MANIFEST,
            PathBuf::from("/verified/snapshot"),
            InstallDisposition::AlreadyInstalled,
        );
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains(BGE_SMALL_EN_V15_MANIFEST.source_revision));
        assert!(json.contains(BGE_SMALL_EN_V15_MANIFEST.source_license_url));
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            native_model_status_at(&root.path().join("missing"))
                .unavailable()
                .map(|reason| reason.code),
            Some(NativeModelUnavailableCode::MissingReference)
        );
    }

    #[test]
    fn explicit_installer_still_honors_offline_mode() {
        let _lock = prism_runtime::offline::test_support::env_lock();
        let _offline = prism_runtime::offline::test_support::OfflineEnvGuard::set("1");
        let error = ensure_install_allowed().unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("disabled by PRISM offline mode"),
            "{message}"
        );
    }

    #[test]
    fn per_model_advisory_lock_excludes_a_second_installer() {
        let root = tempfile::tempdir().unwrap();
        let held = InstallLock::acquire(root.path(), "same-model").unwrap();
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&held.path)
            .unwrap();
        assert!(!FileExt::try_lock_exclusive(&contender).unwrap());
        drop(held);
        assert!(FileExt::try_lock_exclusive(&contender).unwrap());
        FileExt::unlock(&contender).unwrap();
    }

    #[test]
    fn racing_file_target_is_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("staged");
        let destination = root.path().join("published");
        std::fs::write(&source, b"verified-new").unwrap();

        let error = publish_noreplace_with_hook(&source, &destination, || {
            std::fs::write(&destination, b"racing-existing").unwrap();
        })
        .unwrap_err();
        assert!(
            matches!(
                error.kind(),
                io::ErrorKind::AlreadyExists
                    | io::ErrorKind::IsADirectory
                    | io::ErrorKind::PermissionDenied
            ),
            "unexpected no-replace error: {error}"
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"racing-existing");
        assert_eq!(std::fs::read(&source).unwrap(), b"verified-new");
    }

    #[test]
    fn racing_directory_target_is_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("staged-repository");
        let destination = root.path().join("published-repository");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("verified"), b"new").unwrap();

        let error = publish_noreplace_with_hook(&source, &destination, || {
            std::fs::create_dir(&destination).unwrap();
            std::fs::write(destination.join("keep"), b"racing-existing").unwrap();
        })
        .unwrap_err();
        assert!(
            matches!(
                error.kind(),
                io::ErrorKind::AlreadyExists
                    | io::ErrorKind::IsADirectory
                    | io::ErrorKind::PermissionDenied
            ),
            "unexpected no-replace error: {error}"
        );
        assert_eq!(
            std::fs::read(destination.join("keep")).unwrap(),
            b"racing-existing"
        );
        assert_eq!(std::fs::read(source.join("verified")).unwrap(), b"new");
    }
}
