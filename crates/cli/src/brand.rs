//! The single definition of the platform's user-visible brand.
//!
//! PRISM is MIT and stands alone as a product; the company operating the
//! hosted platform is one provider among many. Before this module its name
//! was spelled out in hundreds of literals, so renaming the company was a
//! tree-wide sweep across two languages. Now user-visible text reads from
//! [`Brand`], sourced from `brand.toml` (embedded) with an optional
//! `~/.prism/brand.toml` override — the same data-file pattern as the
//! provider registry, so white-labelling needs no rebuild.
//!
//! ## What this module deliberately does NOT cover
//!
//! **Wire identifiers are frozen.** Env vars (`MARC27_API_KEY`,
//! `MARC27_TOKEN`, `MARC27_API_URL`, `MARC27_PROJECT_ID`), the `m27_` API
//! key prefix, serialized config values (`mode = "marc27"`), CLI value
//! tokens (`prism use marc27`, `--backend marc27`), and on-disk paths are
//! a compatibility surface: shipped clients and existing installs depend
//! on their exact spelling. Routing them through this module would turn a
//! cosmetic rename into a breaking change. They stay hardcoded ON PURPOSE
//! — see the note in `brand.toml`.
//!
//! Internal symbols (crate names, struct names, module paths) are likewise
//! out of scope: churning them inflates diffs for no user benefit.

use serde::{Deserialize, Serialize};

/// Compiled-in brand definition.
const BUILTIN_BRAND_TOML: &str = include_str!("../brand.toml");

/// User-visible names for the hosted platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Brand {
    /// Company/platform display name, e.g. `MARC27`.
    pub display_name: String,
    /// How the platform reads as a chat target, e.g. `MARC27 cloud`.
    pub platform_name: String,
    /// Sign-up / documentation URL.
    pub docs_url: String,
    /// One-line pitch shown beside the platform in provider listings.
    pub tagline: String,
}

impl Default for Brand {
    /// The compiled-in values. Used when `brand.toml` cannot be parsed,
    /// which would be a build-time bug — falling back keeps `prism` usable
    /// rather than panicking over a cosmetic string.
    fn default() -> Self {
        Self {
            display_name: "MARC27".to_string(),
            platform_name: "MARC27 cloud".to_string(),
            docs_url: "https://marc27.com".to_string(),
            tagline: "hosted models, no keys to manage — works out of the box".to_string(),
        }
    }
}

impl Brand {
    /// Parse only the compiled-in definition.
    pub fn builtin() -> anyhow::Result<Self> {
        Ok(toml::from_str(BUILTIN_BRAND_TOML)?)
    }

    /// The effective brand: built-in, overridden by `~/.prism/brand.toml`
    /// if present and parseable. Never fails — a bad override falls back
    /// to the built-in rather than breaking startup over a display string.
    pub fn load() -> Self {
        let builtin = Self::builtin().unwrap_or_default();
        let Some(path) = std::env::var_os("HOME").map(|h| {
            std::path::PathBuf::from(h)
                .join(".prism")
                .join("brand.toml")
        }) else {
            return builtin;
        };
        if !path.exists() {
            return builtin;
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| toml::from_str::<Self>(&raw).ok())
            .unwrap_or(builtin)
    }
}

/// The effective brand, loaded once per process.
///
/// Callers in hot paths should hold the returned reference rather than
/// re-reading; everything user-visible is cold enough that a `OnceLock`
/// read is free.
pub fn brand() -> &'static Brand {
    static BRAND: std::sync::OnceLock<Brand> = std::sync::OnceLock::new();
    BRAND.get_or_init(Brand::load)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_brand_parses() {
        let b = Brand::builtin().expect("embedded brand.toml must parse");
        assert!(!b.display_name.is_empty());
        assert!(!b.platform_name.is_empty());
        assert!(b.docs_url.starts_with("https://"));
        assert!(!b.tagline.is_empty());
    }

    /// The hardcoded `Default` is a fallback for a malformed embedded
    /// file; if it drifts from the file it would silently rename the
    /// product on the failure path. Pin them equal.
    #[test]
    fn default_matches_the_shipped_file() {
        assert_eq!(Brand::builtin().unwrap(), Brand::default());
    }

    /// The platform name should contain the company name — a rename that
    /// updated one but not the other would read as two different products.
    #[test]
    fn platform_name_carries_the_display_name() {
        let b = Brand::builtin().unwrap();
        assert!(
            b.platform_name.contains(&b.display_name),
            "platform_name {:?} should build on display_name {:?}",
            b.platform_name,
            b.display_name
        );
    }
}
