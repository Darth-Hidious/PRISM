//! Data-driven LLM provider registry.
//!
//! PRISM ships a provider list as **data** (`providers.toml`, embedded at
//! build time) rather than a Rust `match`, so a new vendor — or a private
//! corporate gateway — is a config edit, not a release. The user extends
//! or overrides it with `~/.prism/providers.toml`: entries whose `id`
//! matches a built-in replace it, new ids are appended.
//!
//! This exists to keep MARC27 **one entry among many**. Before this, the
//! direct-provider chat target derived its endpoint from
//! `format!("https://api.{provider}.com/v1")`, which is only correct for a
//! couple of vendors (Mistral is `.ai`, Groq mounts under `/openai/v1`,
//! Google under `/v1beta/openai`, …) — so "bring your own key" silently
//! pointed at hosts that do not exist. The registry replaces that guess
//! with a declared, overridable URL.
//!
//! A registry entry never carries a key: it names the **env var** to read
//! at request time (`api_key_env`), so rotating a key is `export …` with no
//! PRISM restart and no secret on disk.
//!
//! Scoped deliberately to this route. PRISM as a whole does write a key to
//! disk on one path — `prism use local --api-key sk-…` stores it in the
//! `[chat]` table of `~/.prism/config.toml` (0600), because a local
//! server's token has no env-var convention to fall back on. These docs
//! previously extended the registry's env-only guarantee to the whole
//! product, which that path has always contradicted.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The built-in registry, compiled into the binary. Keeping it as an
/// embedded data file (rather than a Rust literal) means the shipped list
/// and the user-override file are literally the same schema — what we ship
/// is a worked example of what they can write.
const BUILTIN_PROVIDERS_TOML: &str = include_str!("../providers.toml");

/// One provider entry. Every field except `id` is optional so a user
/// override file can be as short as an id plus a base URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    /// Slug passed to `prism use provider <id>`. Matched case-insensitively.
    pub id: String,
    /// Display name for `prism use list`. Falls back to `id`.
    #[serde(default)]
    pub name: Option<String>,
    /// OpenAI-compatible base URL. `None` is only valid with `platform`.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Env var holding the user's key. `None` ⇒ keyless (local servers).
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Where to get a key — surfaced when the key is missing.
    #[serde(default)]
    pub docs: Option<String>,
    /// `true` ⇒ the endpoint comes from the signed-in session, not this
    /// file. Only MARC27 sets it; it is what keeps MARC27 describable in
    /// the same table as everyone else without pretending its URL is
    /// static.
    #[serde(default)]
    pub platform: bool,
}

impl Provider {
    /// Display name, falling back to the id.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    /// One-line description for `prism use list`. The platform entry gets
    /// its pitch from `brand.toml`; everyone else is described by what it
    /// takes to use them — a key, or nothing at all.
    pub fn blurb(&self) -> String {
        if self.platform {
            return crate::brand::brand().tagline.clone();
        }
        match &self.api_key_env {
            Some(var) => format!("your own key · {var}"),
            None => "local server · no key needed".to_string(),
        }
    }

    /// Whether the credential this provider needs is present right now.
    /// A keyless provider (local server) is always "ready" — there is
    /// nothing to be missing. A `platform` provider is not judged here;
    /// its credential is the login session, which lives elsewhere.
    pub fn key_present(&self) -> bool {
        match &self.api_key_env {
            None => true,
            Some(var) => std::env::var(var).map(|v| !v.is_empty()).unwrap_or(false),
        }
    }
}

/// Registry file shape. `provider` is an array-of-tables so the file reads
/// as a list of `[[provider]]` blocks.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProviderFile {
    #[serde(default)]
    provider: Vec<Provider>,
}

/// The merged provider list: built-ins, then any user override applied.
#[derive(Debug, Clone)]
pub struct Registry {
    providers: Vec<Provider>,
}

impl Registry {
    /// Parse only the compiled-in list. Errors are a build-time bug (the
    /// embedded file is ours), so callers generally want [`Registry::load`].
    pub fn builtin() -> anyhow::Result<Self> {
        let file: ProviderFile = toml::from_str(BUILTIN_PROVIDERS_TOML)?;
        let mut registry = Self {
            providers: file.provider,
        };
        registry.apply_brand();
        Ok(registry)
    }

    /// Fill the platform entry's user-visible fields from `brand.toml`.
    ///
    /// This is what makes a company rename a one-file edit: the registry
    /// declares only the frozen wire id (`marc27`) plus `platform = true`,
    /// and the human-readable name and docs URL come from the single brand
    /// definition. An override file that sets them explicitly still wins.
    fn apply_brand(&mut self) {
        let brand = crate::brand::brand();
        for p in self.providers.iter_mut().filter(|p| p.platform) {
            if p.name.is_none() {
                p.name = Some(brand.platform_name.clone());
            }
            if p.docs.is_none() {
                p.docs = Some(brand.docs_url.clone());
            }
        }
    }

    /// Built-ins merged with `~/.prism/providers.toml`.
    ///
    /// Never fails: a malformed or unreadable override is reported on
    /// stderr and skipped, exactly like `chat_config::load` does for a
    /// malformed config. Losing a hand-written override silently would be
    /// worse than losing it loudly, and hard-failing here would break
    /// `prism` startup over a stray comma in an optional file.
    pub fn load() -> Self {
        let mut registry = Self::builtin().unwrap_or(Self {
            providers: Vec::new(),
        });
        let Some(path) = user_path() else {
            return registry;
        };
        if !path.exists() {
            return registry;
        }
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|raw| toml::from_str::<ProviderFile>(&raw).map_err(|e| e.to_string()))
        {
            Ok(file) => {
                registry.merge(file.provider);
                // A user-added platform entry gets branded too.
                registry.apply_brand();
                registry
            }
            Err(e) => {
                eprintln!(
                    "\x1b[33m[prism]\x1b[0m provider registry at {} is unreadable ({}), \
                     using built-in providers only.",
                    path.display(),
                    e
                );
                registry
            }
        }
    }

    /// Apply overrides: same `id` replaces in place (keeping position so
    /// the listing order stays stable), new ids append.
    fn merge(&mut self, overrides: Vec<Provider>) {
        for over in overrides {
            match self
                .providers
                .iter_mut()
                .find(|p| p.id.eq_ignore_ascii_case(&over.id))
            {
                Some(existing) => *existing = over,
                None => self.providers.push(over),
            }
        }
    }

    /// Look a provider up by slug, case-insensitively.
    pub fn get(&self, id: &str) -> Option<&Provider> {
        self.providers
            .iter()
            .find(|p| p.id.eq_ignore_ascii_case(id))
    }

    /// Every provider, in file order.
    pub fn all(&self) -> &[Provider] {
        &self.providers
    }

    /// A registry of exactly these providers. Test support: it lets the
    /// local-server sweep run against stub endpoints instead of whatever
    /// happens to be listening on the developer's machine.
    #[doc(hidden)]
    pub fn from_providers(providers: Vec<Provider>) -> Self {
        Self { providers }
    }
}

/// `~/.prism/providers.toml`. `None` when `$HOME` is unset.
pub fn user_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".prism").join("providers.toml"))
}

/// Env var holding the key for `provider`, from the registry.
///
/// Falls back to `PRISM_PROVIDER_API_KEY` for an id we have never heard
/// of, matching the previous catch-all so an unknown slug keeps a usable
/// (if generic) knob instead of erroring.
pub fn default_api_key_env(registry: &Registry, provider: &str) -> String {
    registry
        .get(provider)
        .and_then(|p| p.api_key_env.clone())
        .unwrap_or_else(|| "PRISM_PROVIDER_API_KEY".to_string())
}

/// Base URL for a direct-provider chat target.
///
/// `None` for an unknown id or for the `platform` entry (whose URL is
/// session-derived). Callers decide the fallback: the chat paths keep the
/// historical `https://api.{id}.com/v1` guess so an unknown slug behaves
/// exactly as it did before the registry landed, while `prism use` warns
/// that the slug is unknown and points at the override file.
pub fn base_url_for(registry: &Registry, provider: &str) -> Option<String> {
    registry
        .get(provider)
        .filter(|p| !p.platform)
        .and_then(|p| p.base_url.clone())
}

/// The pre-registry endpoint guess, kept as the last-resort fallback for
/// provider slugs that are in nobody's registry. Correct for `openai` and
/// `deepseek`; a guess everywhere else — which is precisely why the
/// registry exists.
pub fn legacy_guess_base_url(provider: &str) -> String {
    format!(
        "https://api.{provider}.com/v1",
        provider = provider.to_ascii_lowercase()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_parses() {
        let reg = Registry::builtin().expect("embedded providers.toml must parse");
        assert!(
            reg.all().len() >= 15,
            "expected the shipped provider list, got {}",
            reg.all().len()
        );
    }

    #[test]
    fn every_builtin_is_well_formed() {
        let reg = Registry::builtin().unwrap();
        for p in reg.all() {
            assert!(!p.id.is_empty(), "provider with empty id");
            assert_eq!(
                p.id.to_ascii_lowercase(),
                p.id,
                "provider id {} must be lowercase (lookup is case-insensitive but \
                 the file should be canonical)",
                p.id
            );
            if p.platform {
                assert!(
                    p.base_url.is_none(),
                    "{}: platform providers resolve their URL from the session",
                    p.id
                );
            } else {
                let url = p
                    .base_url
                    .as_deref()
                    .unwrap_or_else(|| panic!("{}: non-platform provider needs a base_url", p.id));
                assert!(
                    url.starts_with("http://") || url.starts_with("https://"),
                    "{}: base_url must be absolute, got {url}",
                    p.id
                );
                assert!(
                    !url.ends_with('/'),
                    "{}: base_url must not have a trailing slash, got {url}",
                    p.id
                );
                // The LLM client appends `/chat/completions` to a base that
                // already carries a path. A bare host would need `/v1`
                // synthesised, which is a guess we do not want in shipped data.
                assert!(
                    url.matches('/').count() >= 3,
                    "{}: base_url must include the API path, got {url}",
                    p.id
                );
            }
        }
    }

    /// The product position: the hosted platform is the least-friction
    /// option, so it leads the list — but it sits in the same table as
    /// every bring-your-own-key vendor, which is what makes PRISM stand
    /// alone without it.
    #[test]
    fn platform_leads_the_list_among_real_peers() {
        let reg = Registry::builtin().unwrap();
        let first = &reg.all()[0];
        assert_eq!(first.id, "marc27");
        assert!(first.platform);

        let peers: Vec<&str> = reg
            .all()
            .iter()
            .filter(|p| !p.platform)
            .map(|p| p.id.as_str())
            .collect();
        for expected in ["openai", "anthropic", "openrouter", "groq", "ollama"] {
            assert!(peers.contains(&expected), "{expected} must be offered too");
        }
        assert!(
            peers.len() >= 12,
            "the platform must be one of many, got {} peers",
            peers.len()
        );
    }

    /// The rename test. The registry file must NOT spell the company name
    /// — it declares the frozen wire id and takes the human-readable name
    /// from `brand.toml`, so renaming the company is a one-file edit.
    #[test]
    fn platform_name_comes_from_brand_not_the_registry_file() {
        let brand = crate::brand::brand();
        let reg = Registry::builtin().unwrap();
        let platform = reg.get("marc27").unwrap();

        assert_eq!(platform.display_name(), brand.platform_name);
        assert_eq!(platform.docs.as_deref(), Some(brand.docs_url.as_str()));
        assert_eq!(platform.blurb(), brand.tagline);

        // The shipped registry data itself carries no brand text.
        assert!(
            !BUILTIN_PROVIDERS_TOML.contains(&format!("name = \"{}", brand.display_name)),
            "providers.toml must not hardcode the company display name"
        );
    }

    /// `prism use list` renders `display_name()`, so two entries sharing
    /// one name print the same row twice with nothing to choose between
    /// them. `google` and `gemini` are deliberately the same endpoint —
    /// which is exactly why the listing has to say so rather than look
    /// like a duplicate-row bug.
    #[test]
    fn no_two_providers_render_under_the_same_name() {
        let reg = Registry::builtin().unwrap();
        let mut seen: Vec<&str> = Vec::new();
        for p in reg.all() {
            let name = p.display_name();
            assert!(
                !seen.contains(&name),
                "{} and an earlier entry both display as {name:?}",
                p.id
            );
            seen.push(name);
        }
        // The pair that motivated this: same endpoint, distinguishable rows.
        assert_eq!(
            base_url_for(&reg, "gemini"),
            base_url_for(&reg, "google"),
            "the alias must stay an alias"
        );
        assert!(reg.get("gemini").unwrap().display_name().contains("alias"));
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let reg = Registry::builtin().unwrap();
        assert_eq!(reg.get("OpenAI").map(|p| p.id.as_str()), Some("openai"));
        assert_eq!(
            reg.get("ANTHROPIC").map(|p| p.id.as_str()),
            Some("anthropic")
        );
    }

    #[test]
    fn known_endpoints_are_not_the_dot_com_guess() {
        // Regression pin for the bug the registry exists to kill: these
        // four are exactly the providers `https://api.{id}.com/v1` got wrong.
        let reg = Registry::builtin().unwrap();
        assert_eq!(
            base_url_for(&reg, "mistral").as_deref(),
            Some("https://api.mistral.ai/v1")
        );
        assert_eq!(
            base_url_for(&reg, "groq").as_deref(),
            Some("https://api.groq.com/openai/v1")
        );
        assert_eq!(
            base_url_for(&reg, "openrouter").as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(
            base_url_for(&reg, "xai").as_deref(),
            Some("https://api.x.ai/v1")
        );
        for id in ["mistral", "groq", "openrouter", "xai"] {
            assert_ne!(
                base_url_for(&reg, id).as_deref(),
                Some(legacy_guess_base_url(id).as_str()),
                "{id}: registry must differ from the old guess"
            );
        }
    }

    /// Every shipped base URL, pinned to the exact endpoint a chat turn
    /// POSTs to. This is the test that would have caught the 404s: the
    /// client used to synthesise a `/v1` segment for any base that did not
    /// already end in one, so `google`, `gemini` and `zai` — whose vendors
    /// mount their OpenAI-compatible surface elsewhere — resolved to URLs
    /// that do not exist. Nothing pinned the composition, so the suite
    /// stayed green while three shipped providers were unusable.
    ///
    /// Each expectation below is the vendor's documented OpenAI-compatible
    /// chat endpoint. Adding a provider without adding it here fails the
    /// coverage assertion at the bottom.
    #[test]
    fn every_shipped_base_resolves_to_the_vendors_real_endpoint() {
        use prism_ingest::llm::chat_completions_url;

        const EXPECTED: &[(&str, &str)] = &[
            ("openai", "https://api.openai.com/v1/chat/completions"),
            ("anthropic", "https://api.anthropic.com/v1/chat/completions"),
            (
                "google",
                "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
            ),
            (
                "gemini",
                "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
            ),
            (
                "openrouter",
                "https://openrouter.ai/api/v1/chat/completions",
            ),
            ("groq", "https://api.groq.com/openai/v1/chat/completions"),
            ("cerebras", "https://api.cerebras.ai/v1/chat/completions"),
            ("zai", "https://api.z.ai/api/paas/v4/chat/completions"),
            ("mistral", "https://api.mistral.ai/v1/chat/completions"),
            ("deepseek", "https://api.deepseek.com/v1/chat/completions"),
            ("xai", "https://api.x.ai/v1/chat/completions"),
            ("together", "https://api.together.xyz/v1/chat/completions"),
            (
                "fireworks",
                "https://api.fireworks.ai/inference/v1/chat/completions",
            ),
            (
                "cohere",
                "https://api.cohere.ai/compatibility/v1/chat/completions",
            ),
            ("ollama", "http://localhost:11434/v1/chat/completions"),
            ("llamacpp", "http://localhost:8080/v1/chat/completions"),
            ("lmstudio", "http://localhost:1234/v1/chat/completions"),
            ("vllm", "http://localhost:8000/v1/chat/completions"),
        ];

        let reg = Registry::builtin().unwrap();
        for (id, expected) in EXPECTED {
            let base =
                base_url_for(&reg, id).unwrap_or_else(|| panic!("{id} missing from registry"));
            assert_eq!(
                chat_completions_url(&base),
                *expected,
                "{id}: base {base} resolves to the wrong endpoint"
            );
        }

        // Coverage: no shipped provider may go unpinned. A new vendor added
        // to providers.toml without a line above is exactly how an unusable
        // endpoint ships unnoticed.
        let pinned: Vec<&str> = EXPECTED.iter().map(|(id, _)| *id).collect();
        let shipped: Vec<&str> = reg
            .all()
            .iter()
            .filter(|p| !p.platform)
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(
            shipped.len(),
            EXPECTED.len(),
            "pin every shipped provider: {shipped:?} vs pinned {pinned:?}"
        );
        for id in &shipped {
            assert!(
                pinned.contains(id),
                "{id} ships but its endpoint is unpinned"
            );
        }
    }

    #[test]
    fn platform_provider_has_no_static_base_url() {
        let reg = Registry::builtin().unwrap();
        assert_eq!(base_url_for(&reg, "marc27"), None);
    }

    #[test]
    fn unknown_provider_resolves_to_nothing() {
        let reg = Registry::builtin().unwrap();
        assert_eq!(base_url_for(&reg, "no-such-vendor"), None);
        assert_eq!(
            default_api_key_env(&reg, "no-such-vendor"),
            "PRISM_PROVIDER_API_KEY"
        );
    }

    /// Every vendor the old hardcoded `ChatTarget::default_api_key_env`
    /// match knew about must still resolve identically — the registry
    /// replaced that function, and a regression here would silently make
    /// PRISM read the wrong env var for an existing user.
    #[test]
    fn key_env_comes_from_the_registry() {
        let reg = Registry::builtin().unwrap();
        assert_eq!(default_api_key_env(&reg, "anthropic"), "ANTHROPIC_API_KEY");
        assert_eq!(default_api_key_env(&reg, "openai"), "OPENAI_API_KEY");
        assert_eq!(default_api_key_env(&reg, "OpenAI"), "OPENAI_API_KEY");
        assert_eq!(default_api_key_env(&reg, "mistral"), "MISTRAL_API_KEY");
        assert_eq!(default_api_key_env(&reg, "cohere"), "COHERE_API_KEY");
        // Both spellings the old match accepted.
        assert_eq!(default_api_key_env(&reg, "google"), "GEMINI_API_KEY");
        assert_eq!(default_api_key_env(&reg, "gemini"), "GEMINI_API_KEY");
    }

    /// Vendors beyond the old match's five now resolve their own env var
    /// instead of collapsing onto the `PRISM_PROVIDER_API_KEY` catch-all.
    #[test]
    fn key_env_covers_providers_the_old_match_missed() {
        let reg = Registry::builtin().unwrap();
        for (id, expected) in [
            ("groq", "GROQ_API_KEY"),
            ("openrouter", "OPENROUTER_API_KEY"),
            ("deepseek", "DEEPSEEK_API_KEY"),
            ("xai", "XAI_API_KEY"),
            ("cerebras", "CEREBRAS_API_KEY"),
            ("zai", "ZAI_API_KEY"),
            ("together", "TOGETHER_API_KEY"),
            ("fireworks", "FIREWORKS_API_KEY"),
        ] {
            assert_eq!(default_api_key_env(&reg, id), expected, "{id}");
        }
    }

    #[test]
    fn local_providers_are_keyless() {
        let reg = Registry::builtin().unwrap();
        for id in ["ollama", "llamacpp", "lmstudio", "vllm"] {
            let p = reg.get(id).unwrap_or_else(|| panic!("{id} missing"));
            assert!(
                p.api_key_env.is_none(),
                "{id} must need no key — that is the whole point of a local provider"
            );
            // Keyless ⇒ always ready, regardless of environment.
            assert!(p.key_present(), "{id} should report ready with no key set");
        }
    }

    #[test]
    fn override_replaces_in_place_and_appends() {
        let mut reg = Registry::builtin().unwrap();
        let before = reg.all().len();
        let openai_pos = reg.all().iter().position(|p| p.id == "openai").unwrap();

        reg.merge(vec![
            Provider {
                id: "openai".into(),
                name: Some("Corp OpenAI proxy".into()),
                base_url: Some("https://llm.corp.internal/v1".into()),
                api_key_env: Some("CORP_KEY".into()),
                docs: None,
                platform: false,
            },
            Provider {
                id: "brand-new".into(),
                name: None,
                base_url: Some("https://example.invalid/v1".into()),
                api_key_env: None,
                docs: None,
                platform: false,
            },
        ]);

        assert_eq!(reg.all().len(), before + 1, "one replace, one append");
        assert_eq!(
            reg.all().iter().position(|p| p.id == "openai"),
            Some(openai_pos),
            "an override must not reshuffle the listing"
        );
        assert_eq!(
            base_url_for(&reg, "openai").as_deref(),
            Some("https://llm.corp.internal/v1")
        );
        assert_eq!(default_api_key_env(&reg, "openai"), "CORP_KEY");
        assert_eq!(reg.all().last().unwrap().id, "brand-new");
        assert_eq!(reg.get("brand-new").unwrap().display_name(), "brand-new");
    }

    #[test]
    fn key_present_reflects_the_environment() {
        let p = Provider {
            id: "test".into(),
            name: None,
            base_url: Some("https://example.invalid/v1".into()),
            api_key_env: Some("PRISM_TEST_KEY_PRESENCE".into()),
            docs: None,
            platform: false,
        };
        // SAFETY: a uniquely-named var no other test reads or writes.
        unsafe { std::env::remove_var("PRISM_TEST_KEY_PRESENCE") };
        assert!(!p.key_present());
        unsafe { std::env::set_var("PRISM_TEST_KEY_PRESENCE", "") };
        assert!(!p.key_present(), "an empty key is not a key");
        unsafe { std::env::set_var("PRISM_TEST_KEY_PRESENCE", "sk-x") };
        assert!(p.key_present());
        unsafe { std::env::remove_var("PRISM_TEST_KEY_PRESENCE") };
    }
}
