// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.

//! Provider-neutral names for the hosted-platform environment.
//!
//! PRISM is a Mirdyne product. The hosted platform it talks to by default is
//! operated by MARC27, but that is a *service relationship*, not a property of
//! the product: PRISM may point at an alternative provider, a corporate
//! gateway, or a self-hosted backend. `crates/core/providers.toml` already
//! makes that true for LLM routing, where the hosted platform is one row in
//! the same table as everyone else. This module does the same job for the
//! platform surface — endpoints, credentials, project scope.
//!
//! Until now those values were reachable only through variables *named* after
//! one company (`MARC27_PLATFORM_URL`, `MARC27_API_KEY`, …). Renaming them
//! outright would break every shipped client, every deployed node and every
//! CI secret that sets the old name. So both work:
//!
//! ```text
//!   PRISM_PLATFORM_URL   ← preferred; what documentation should say
//!   MARC27_PLATFORM_URL  ← still honoured, indefinitely
//! ```
//!
//! ## Precedence
//!
//! The neutral name wins when both are set and both are non-empty. That is the
//! only sane rule for a migration: an operator adding the new name to a host
//! that already has the old one must be able to predict the outcome, and the
//! name they just deliberately set is the one they meant.
//!
//! ## Blank is not set
//!
//! A variable set to `""` or to whitespace is treated as absent, and falls
//! through to the alias and then to the default. Empty env vars are a common
//! artifact of shell scripts and CI templates (`FOO=$BAR` with `BAR` unset),
//! and treating one as a real value produces a request to the empty string —
//! a failure whose message points nowhere near the cause.

use std::env;

/// One platform setting, addressable under a neutral name with the historical
/// company-scoped name kept as a permanent alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformVar {
    /// Provider-neutral name. Preferred, and what docs should reference.
    pub preferred: &'static str,
    /// Historical name. Honoured indefinitely — shipped clients set it.
    pub alias: &'static str,
}

impl PlatformVar {
    /// Base URL of the hosted platform API.
    pub const PLATFORM_URL: Self = Self {
        preferred: "PRISM_PLATFORM_URL",
        alias: "MARC27_PLATFORM_URL",
    };
    /// Alternate spelling of the API base used by the auth and CLI paths.
    pub const API_URL: Self = Self {
        preferred: "PRISM_API_URL",
        alias: "MARC27_API_URL",
    };
    /// Long-lived API key.
    pub const API_KEY: Self = Self {
        preferred: "PRISM_API_KEY",
        alias: "MARC27_API_KEY",
    };
    /// Short-lived session token.
    pub const TOKEN: Self = Self {
        preferred: "PRISM_TOKEN",
        alias: "MARC27_TOKEN",
    };
    /// Alternate spelling of the token used by the auth path.
    pub const API_TOKEN: Self = Self {
        preferred: "PRISM_API_TOKEN",
        alias: "MARC27_API_TOKEN",
    };
    /// Project scope for billing and graph writes.
    pub const PROJECT_ID: Self = Self {
        preferred: "PRISM_PROJECT_ID",
        alias: "MARC27_PROJECT_ID",
    };

    /// Every platform variable, for diagnostics (`prism doctor`) and for the
    /// test that pins the alias table against drift.
    pub const ALL: [Self; 6] = [
        Self::PLATFORM_URL,
        Self::API_URL,
        Self::API_KEY,
        Self::TOKEN,
        Self::API_TOKEN,
        Self::PROJECT_ID,
    ];

    /// Resolve this setting: preferred name first, then the historical alias.
    ///
    /// A variable that is unset, empty, or whitespace-only is treated as
    /// absent. Returns `None` when neither name carries a usable value, so the
    /// caller keeps its own default and its own error message.
    pub fn get(&self) -> Option<String> {
        non_blank(self.preferred).or_else(|| non_blank(self.alias))
    }

    /// Which name supplied the value, for diagnostics. `None` when unset.
    ///
    /// This is what lets `doctor` say "reading MARC27_API_KEY (deprecated;
    /// prefer PRISM_API_KEY)" instead of leaving an operator to guess which of
    /// two variables the process actually used.
    pub fn source(&self) -> Option<&'static str> {
        if non_blank(self.preferred).is_some() {
            Some(self.preferred)
        } else if non_blank(self.alias).is_some() {
            Some(self.alias)
        } else {
            None
        }
    }

    /// True when the value came from the historical name, so callers can warn
    /// once without hard-coding the company-scoped string at the call site.
    pub fn is_deprecated_source(&self) -> bool {
        self.source() == Some(self.alias)
    }
}

/// Read an env var, treating unset / empty / whitespace-only as absent.
fn non_blank(name: &str) -> Option<String> {
    match env::var(name) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Env is process-global; these tests mutate it. Serialize them.
    ///
    /// Deliberately recovers from poisoning: one panicking test must not
    /// convert every later test in this module into a spurious failure that
    /// masks the real one.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let m = LOCK.get_or_init(|| Mutex::new(()));
        m.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Clear both names so a test starts from a known state.
    fn clear(v: PlatformVar) {
        unsafe {
            env::remove_var(v.preferred);
            env::remove_var(v.alias);
        }
    }

    #[test]
    fn preferred_name_is_read() {
        let _g = env_lock();
        let v = PlatformVar::PLATFORM_URL;
        clear(v);
        unsafe { env::set_var(v.preferred, "https://example.test") };
        assert_eq!(v.get().as_deref(), Some("https://example.test"));
        assert_eq!(v.source(), Some(v.preferred));
        assert!(!v.is_deprecated_source());
        clear(v);
    }

    /// The whole point of the alias: a client shipped before the rename keeps
    /// working with no change on the operator's side.
    #[test]
    fn historical_alias_still_resolves() {
        let _g = env_lock();
        let v = PlatformVar::API_KEY;
        clear(v);
        unsafe { env::set_var(v.alias, "legacy-key") };
        assert_eq!(v.get().as_deref(), Some("legacy-key"));
        assert_eq!(v.source(), Some(v.alias));
        assert!(v.is_deprecated_source());
        clear(v);
    }

    /// Precedence is the migration-critical rule: an operator who adds the new
    /// name to a host that already carries the old one gets the new one.
    #[test]
    fn preferred_wins_when_both_are_set() {
        let _g = env_lock();
        let v = PlatformVar::TOKEN;
        clear(v);
        unsafe {
            env::set_var(v.preferred, "new");
            env::set_var(v.alias, "old");
        }
        assert_eq!(v.get().as_deref(), Some("new"));
        assert_eq!(v.source(), Some(v.preferred));
        clear(v);
    }

    /// `FOO=$BAR` with `BAR` unset is a blank, not a value. Falling through to
    /// the alias beats issuing a request to the empty string.
    #[test]
    fn blank_preferred_falls_through_to_alias() {
        let _g = env_lock();
        let v = PlatformVar::PROJECT_ID;
        clear(v);
        unsafe {
            env::set_var(v.preferred, "   ");
            env::set_var(v.alias, "proj-real");
        }
        assert_eq!(v.get().as_deref(), Some("proj-real"));
        assert_eq!(v.source(), Some(v.alias));
        clear(v);
    }

    #[test]
    fn blank_everywhere_is_absent() {
        let _g = env_lock();
        let v = PlatformVar::API_TOKEN;
        clear(v);
        unsafe {
            env::set_var(v.preferred, "");
            env::set_var(v.alias, "\t ");
        }
        assert_eq!(v.get(), None);
        assert_eq!(v.source(), None);
        assert!(!v.is_deprecated_source());
        clear(v);
    }

    #[test]
    fn unset_is_none() {
        let _g = env_lock();
        let v = PlatformVar::API_URL;
        clear(v);
        assert_eq!(v.get(), None);
        assert_eq!(v.source(), None);
    }

    /// Pins the table itself. A new platform variable added without a neutral
    /// name, or an alias that stops being company-scoped, shows up here rather
    /// than silently reintroducing the coupling this module exists to remove.
    #[test]
    fn every_var_has_a_neutral_preferred_and_a_legacy_alias() {
        for v in PlatformVar::ALL {
            assert!(
                v.preferred.starts_with("PRISM_"),
                "{} is not provider-neutral",
                v.preferred
            );
            assert!(
                v.alias.starts_with("MARC27_"),
                "{} is not the historical alias shape",
                v.alias
            );
            assert_ne!(v.preferred, v.alias);
        }
    }

    /// Two settings sharing a name would make resolution order load-bearing
    /// and untestable.
    #[test]
    fn names_are_unique_across_the_table() {
        let mut seen = Vec::new();
        for v in PlatformVar::ALL {
            seen.push(v.preferred);
            seen.push(v.alias);
        }
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(before, seen.len(), "duplicate env var name in the table");
    }
}
