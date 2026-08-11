//! The single definition of how the hosted platform is named to users.
//!
//! PRISM stands alone as a product; the hosted platform is one provider
//! among many. The shipped strings name **no company** — the service is
//! described by what it is ("hosted platform"), never by who operates it,
//! because a product naming its own operator reads as an endorsement the
//! product has no business making. User-visible text reads from [`Brand`],
//! sourced from `brand.toml` (embedded) with an optional
//! `~/.prism/brand.toml` override — the same data-file pattern as the
//! provider registry, so white-labelling needs no rebuild.
//!
//! ## What this module deliberately does NOT cover
//!
//! **Wire identifiers are compatibility surfaces.** PRISM-native environment
//! variables (`PRISM_API_KEY`, `PRISM_API_URL`, and their peers) are canonical.
//! The historical `MARC27_*` spellings and `m27_` key prefix remain frozen as
//! deprecated aliases because shipped clients and existing installs depend on
//! them. Serialized config values (`mode = "marc27"`), CLI value tokens
//! (`prism use marc27`, `--backend marc27`), and on-disk paths likewise stay
//! stable. Routing any of these through display branding would turn a
//! migration into a breaking cosmetic rename — see `brand.toml`.
//!
//! Internal symbols (crate names, struct names, module paths) are likewise
//! out of scope: churning them inflates diffs for no user benefit.

use serde::{Deserialize, Serialize};

/// Compiled-in brand definition.
const BUILTIN_BRAND_TOML: &str = include_str!("../brand.toml");

/// User-visible names for the hosted platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Brand {
    /// How the service reads mid-sentence, e.g. `the platform`.
    pub display_name: String,
    /// How the platform reads as a chat target, e.g. `hosted platform`.
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
            display_name: "the platform".to_string(),
            platform_name: "hosted platform".to_string(),
            docs_url: "https://marc27.com".to_string(),
            tagline: "hosted models — no key required".to_string(),
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

    /// PRISM names no company in its own UI. Every string here is rendered
    /// to users, so a company name reaching one of them puts the product in
    /// the position of advertising whoever operates the hosted platform.
    ///
    /// `docs_url` is exempt on purpose: it is a live address that has to
    /// resolve, in the same class as `api.marc27.com` and the `MARC27_*`
    /// env vars, and it is never rendered as a name.
    #[test]
    fn no_company_name_in_rendered_strings() {
        let b = Brand::builtin().unwrap();
        for (field, value) in [
            ("display_name", &b.display_name),
            ("platform_name", &b.platform_name),
            ("tagline", &b.tagline),
        ] {
            let lowered = value.to_lowercase();
            for banned in ["marc27", "mirdyne"] {
                assert!(
                    !lowered.contains(banned),
                    "brand.{field} = {value:?} names a company; \
                     describe the service, do not brand it"
                );
            }
        }
    }
}
