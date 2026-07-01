//! Tool auto-sync: keep `~/.prism/tools/` aligned with the MARC27 marketplace.
//!
//! Two entry points:
//! - [`sync_tools`] — full pull. Lists marketplace tools, diffs against a
//!   local manifest, re-downloads any whose version changed. Remote wins
//!   silently (per design decision): a locally-edited file is overwritten
//!   if its marketplace version differs.  This keeps field deployments
//!   current without surprising the user with prompts.
//! - [`quick_check`] — lightweight startup probe. Same diff but only
//!   fetches the catalog (`GET /marketplace/resources`); the actual
//!   downloads are fired off as a background tokio task so `prism backend`
//!   / `prism tui` startup isn't blocked on network I/O.
//!
//! The manifest (`~/.prism/tools/.manifest.json`) records the last-known
//! remote version for each installed tool so we can skip unchanged tools
//! instead of re-downloading every file every time.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use prism_client::marketplace::MarketplaceClient;

/// `~/.prism/tools/.manifest.json` — maps tool slug → last-installed metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolManifest {
    #[serde(default)]
    pub tools: HashMap<String, ToolManifestEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolManifestEntry {
    /// Marketplace `version` string at the time we last downloaded this tool.
    #[serde(default)]
    pub version: String,
    /// ISO-8601 timestamp of the last successful install.
    #[serde(default)]
    pub installed_at: String,
    /// Slug (filename stem) the entry corresponds to.
    #[serde(default)]
    pub slug: String,
}

/// Outcome of a sync, for caller reporting.
#[derive(Debug, Default)]
pub struct SyncReport {
    pub updated: Vec<String>,
    pub added: Vec<String>,
    pub unchanged: Vec<String>,
    pub failed: Vec<(String, String)>,
}

impl SyncReport {
    pub fn total_changes(&self) -> usize {
        self.updated.len() + self.added.len()
    }
}

fn tools_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".prism").join("tools"))
}

fn manifest_path() -> Result<PathBuf> {
    Ok(tools_dir()?.join(".manifest.json"))
}

fn load_manifest() -> ToolManifest {
    match manifest_path() {
        Ok(p) if p.exists() => match std::fs::read_to_string(&p) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => ToolManifest::default(),
        },
        _ => ToolManifest::default(),
    }
}

fn save_manifest(m: &ToolManifest) -> Result<()> {
    let p = manifest_path()?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let s = serde_json::to_string_pretty(m)?;
    std::fs::write(&p, s)?;
    Ok(())
}

/// Validate a marketplace slug for safe filesystem use. Rejects path
/// traversal and weird characters — mirrors the check in the install
/// command. Marketplace slugs are simple identifiers in practice.
fn safe_slug(slug: &str) -> Option<&str> {
    if slug.is_empty()
        || slug.contains('/')
        || slug.contains('\\')
        || slug.contains("..")
        || slug.starts_with('.')
    {
        return None;
    }
    Some(slug)
}

/// Full sync: list marketplace tools, diff against the local manifest,
/// re-download any whose version changed (or that aren't installed yet).
/// Remote wins silently — locally-edited files are overwritten.
pub async fn sync_tools(marketplace: &MarketplaceClient<'_>) -> Result<SyncReport> {
    let dir = tools_dir()?;
    std::fs::create_dir_all(&dir)?;
    let mut manifest = load_manifest();
    let mut report = SyncReport::default();

    let remote_tools = marketplace.list_installable_tools().await?;
    debug!(
        count = remote_tools.len(),
        "marketplace tools fetched for sync"
    );

    let http = reqwest::Client::new();
    let now = chrono::Utc::now().to_rfc3339();

    for tool in remote_tools {
        let slug = if !tool.slug.is_empty() {
            tool.slug.clone()
        } else {
            tool.name.clone()
        };
        if safe_slug(&slug).is_none() {
            warn!(%slug, "skipping tool with unsafe slug");
            continue;
        }

        let dest = dir.join(format!("{slug}.py"));
        let prev = manifest.tools.get(&slug);

        // Skip if version is unchanged AND the file already exists.
        // Empty version string on either side → always re-pull (can't diff).
        if !tool.version.is_empty()
            && prev.is_some_and(|e| e.version == tool.version)
            && dest.exists()
        {
            report.unchanged.push(slug);
            continue;
        }

        // Fetch the install URL, then download the tool source.
        let install_url = match marketplace.install_url(&slug).await {
            Ok(u) => u,
            Err(e) => {
                report.failed.push((slug.clone(), e.to_string()));
                continue;
            }
        };
        let content = match http
            .get(&install_url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(r) => r.text().await.unwrap_or_default(),
            Err(e) => {
                report.failed.push((slug.clone(), format!("download: {e}")));
                continue;
            }
        };

        // Validate it looks like Python source, not an HTML 404 page.
        // A minimal heuristic: the first non-empty, non-comment line should
        // contain a Python keyword (def, class, import, TOOL_, or at least
        // not start with `<`).
        if content.trim_start().starts_with('<') {
            report.failed.push((
                slug.clone(),
                "downloaded content looks like HTML, not Python".into(),
            ));
            continue;
        }

        if let Err(e) = std::fs::write(&dest, &content) {
            report.failed.push((slug.clone(), format!("write: {e}")));
            continue;
        }

        let existed_before = manifest.tools.contains_key(&slug);
        manifest.tools.insert(
            slug.clone(),
            ToolManifestEntry {
                version: tool.version.clone(),
                installed_at: now.clone(),
                slug: slug.clone(),
            },
        );

        if existed_before {
            report.updated.push(slug);
        } else {
            report.added.push(slug);
        }
    }

    save_manifest(&manifest)?;
    info!(
        updated = report.updated.len(),
        added = report.added.len(),
        unchanged = report.unchanged.len(),
        failed = report.failed.len(),
        "tool sync complete"
    );
    Ok(report)
}

/// Lightweight startup probe. Fetches only the catalog and kicks off a
/// background sync task if any versions differ from the manifest. Does
/// NOT block the caller — the actual downloads happen in a detached
/// tokio task so `prism backend` / `prism tui` startup isn't delayed.
///
/// Silently no-ops when the marketplace is unreachable (offline mode).
///
/// Callers must use [`spawn_background_sync_owned`] (the owned-client
/// variant); this borrowed variant is retained for tests and future
/// callers that already hold a `&'static PlatformClient`.
#[allow(dead_code)]
pub fn spawn_background_sync(marketplace: MarketplaceClient<'static>, _token: Option<String>) {
    tokio::spawn(async move {
        match sync_tools(&marketplace).await {
            Ok(r) if r.total_changes() > 0 => {
                info!(
                    updated = r.updated.len(),
                    added = r.added.len(),
                    "background tool sync applied updates"
                );
            }
            Ok(_) => debug!("background tool sync: no changes"),
            Err(e) => warn!(error = %e, "background tool sync failed (non-fatal)"),
        }
    });
}

/// Owned-client variant: build the marketplace client inside the task
/// from an owned `PlatformClient`. This is the one callers should use
/// from the startup path — it avoids lifetime gymnastics with the
/// borrowed `MarketplaceClient<'a>`.
pub fn spawn_background_sync_owned(platform: prism_client::api::PlatformClient) {
    tokio::spawn(async move {
        let marketplace = MarketplaceClient::new(&platform);
        match sync_tools(&marketplace).await {
            Ok(r) if r.total_changes() > 0 => {
                info!(
                    updated = r.updated.len(),
                    added = r.added.len(),
                    "background tool sync applied updates"
                );
            }
            Ok(_) => debug!("background tool sync: no changes"),
            Err(e) => warn!(error = %e, "background tool sync failed (non-fatal)"),
        }
    });
}

/// Print a human-readable summary of a sync report.
pub fn print_report(r: &SyncReport) {
    if r.added.is_empty() && r.updated.is_empty() {
        if r.unchanged.is_empty() {
            println!("No marketplace tools to sync.");
        } else {
            println!("All {} tools up to date.", r.unchanged.len());
        }
    } else {
        if !r.added.is_empty() {
            println!("Installed {} new tool(s):", r.added.len());
            for s in &r.added {
                println!("  + {s}");
            }
        }
        if !r.updated.is_empty() {
            println!("Updated {} tool(s):", r.updated.len());
            for s in &r.updated {
                println!("  ↑ {s}");
            }
        }
    }
    if !r.failed.is_empty() {
        eprintln!("Failed to sync {} tool(s):", r.failed.len());
        for (slug, err) in &r.failed {
            eprintln!("  ! {slug}: {err}");
        }
    }
}

/// Read-only inspection: list what *would* be updated without downloading.
/// Used by `prism tools update --dry-run`.
pub async fn check_for_updates(
    marketplace: &MarketplaceClient<'_>,
) -> Result<Vec<(String, String, String)>> {
    let manifest = load_manifest();
    let remote = marketplace.list_installable_tools().await?;
    let mut out = Vec::new();
    for tool in remote {
        let slug = if !tool.slug.is_empty() {
            tool.slug.clone()
        } else {
            tool.name.clone()
        };
        if safe_slug(&slug).is_none() {
            continue;
        }
        let local_version = manifest
            .tools
            .get(&slug)
            .map(|e| e.version.clone())
            .unwrap_or_default();
        if local_version != tool.version {
            out.push((slug, local_version, tool.version));
        }
    }
    Ok(out)
}

/// Remove manifest entries for tools whose `.py` file no longer exists on
/// disk. Keeps the manifest tidy after manual `rm` of a tool.
pub fn prune_manifest() -> Result<usize> {
    let dir = tools_dir()?;
    let mut manifest = load_manifest();
    let before = manifest.tools.len();
    manifest
        .tools
        .retain(|slug, _| dir.join(format!("{slug}.py")).exists());
    let removed = before - manifest.tools.len();
    if removed > 0 {
        save_manifest(&manifest)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_slug_rejects_traversal() {
        assert!(safe_slug("../etc/passwd").is_none());
        assert!(safe_slug(".hidden").is_none());
        assert!(safe_slug("a/b").is_none());
        assert!(safe_slug("a\\b").is_none());
        assert!(safe_slug("").is_none());
        assert_eq!(
            safe_slug("predict.density.mace"),
            Some("predict.density.mace")
        );
    }

    #[test]
    fn manifest_roundtrip() {
        let mut m = ToolManifest::default();
        m.tools.insert(
            "foo".into(),
            ToolManifestEntry {
                version: "1.2.0".into(),
                installed_at: "2026-06-29T00:00:00Z".into(),
                slug: "foo".into(),
            },
        );
        let s = serde_json::to_string(&m).unwrap();
        let back: ToolManifest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.tools.get("foo").unwrap().version, "1.2.0");
    }

    #[test]
    fn empty_manifest_deserializes_to_default() {
        let m: ToolManifest = serde_json::from_str("{}").unwrap();
        assert!(m.tools.is_empty());
    }
}
