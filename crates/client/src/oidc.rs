//! OpenID Connect discovery.
//!
//! # Why this exists
//!
//! PRISM used to build its JWKS URL by assuming one vendor's URL layout —
//! `{project}/auth/v1/.well-known/jwks.json` — and its issuer the same way.
//! That is Supabase's shape and nobody else's:
//!
//! | provider  | JWKS path                                      |
//! |-----------|------------------------------------------------|
//! | Supabase  | `/auth/v1/.well-known/jwks.json`               |
//! | Firebase  | `googleapis.com/service_accounts/v1/jwk/…`     |
//! | Keycloak  | `/realms/{realm}/protocol/openid-connect/certs`|
//! | Zitadel   | `/oauth/v2/keys`                               |
//! | Authentik | `/application/o/{app}/jwks/`                   |
//!
//! So the choice of identity provider was pinned by a hardcoded path rather
//! than by policy. Discovery removes that: every compliant provider publishes
//! `/.well-known/openid-configuration`, and the document names its own
//! `issuer` and `jwks_uri`.
//!
//! # The security property that matters
//!
//! A discovery document is fetched from the network, so it is untrusted input
//! that describes *where to fetch signing keys from*. If it could nominate an
//! arbitrary issuer, an attacker who controlled it could point verification at
//! keys they own and mint tokens PRISM would accept.
//!
//! OIDC Discovery §4.3 requires the document's `issuer` to equal the issuer it
//! was fetched for, and [`OidcDiscovery::fetch`] enforces exactly that. The
//! configured issuer therefore remains the root of trust; discovery may only
//! tell PRISM where that issuer's keys live, never who the issuer is.
//!
//! There is deliberately **no fallback**. If discovery fails, verification
//! fails. Guessing a JWKS path on failure would reinstate the very assumption
//! this module exists to remove — and would do it silently, at the moment
//! something is already wrong.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use url::Url;

/// Discovery path defined by OpenID Connect Discovery 1.0 §4.
const DISCOVERY_PATH: &str = ".well-known/openid-configuration";

/// How long a fetched document is reused before being re-fetched.
///
/// Discovery documents are near-static — providers rotate signing KEYS often
/// and the `jwks_uri` almost never. An hour keeps a login from making two
/// network round trips while still picking up a provider migration the same
/// working day.
const CACHE_TTL: Duration = Duration::from_secs(60 * 60);

/// The two fields PRISM needs from a discovery document.
///
/// Deliberately not the whole document: parsing only what is used means an
/// unexpected or hostile field cannot influence behaviour, and the type says
/// plainly what PRISM's trust actually rests on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcDiscovery {
    issuer: String,
    jwks_uri: Url,
}

#[derive(Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    jwks_uri: String,
}

impl OidcDiscovery {
    /// The verified issuer. Equal to the issuer this was fetched for.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// Where this issuer publishes its signing keys.
    #[must_use]
    pub fn jwks_uri(&self) -> &Url {
        &self.jwks_uri
    }

    /// Fetch and validate the discovery document for `issuer`.
    ///
    /// `issuer` is the identity provider's base URL — `https://…/auth/v1` for
    /// Supabase, `https://securetoken.google.com/{project}` for Firebase,
    /// `https://…/realms/{realm}` for Keycloak. The document is fetched from
    /// `{issuer}/.well-known/openid-configuration`.
    pub async fn fetch(client: &reqwest::Client, issuer: &str, timeout: Duration) -> Result<Self> {
        let issuer = issuer.trim().trim_end_matches('/');
        ensure!(
            !issuer.is_empty(),
            "identity provider issuer is not configured"
        );
        let base = Url::parse(&format!("{issuer}/"))
            .with_context(|| format!("identity provider issuer is not a valid URL: {issuer}"))?;
        require_safe_transport(&base, "issuer")?;

        let url = base
            .join(DISCOVERY_PATH)
            .context("failed to build the OIDC discovery URL")?;

        let response = client
            .get(url.clone())
            .timeout(timeout)
            .send()
            .await
            .with_context(|| format!("failed to fetch the OIDC discovery document from {url}"))?;
        ensure!(
            response.status().is_success(),
            "OIDC discovery failed for {issuer}: HTTP {}. PRISM does not guess a \
             JWKS path when discovery fails.",
            response.status()
        );
        let document: DiscoveryDocument = response
            .json()
            .await
            .with_context(|| format!("{url} did not return a valid OIDC discovery document"))?;

        // THE check. OIDC Discovery §4.3: the document's issuer MUST equal the
        // issuer it was requested for. Without this a document could nominate
        // any issuer, and PRISM would verify tokens against keys chosen by
        // whoever served the document rather than by configuration.
        let advertised = document.issuer.trim().trim_end_matches('/');
        ensure!(
            advertised == issuer,
            "OIDC discovery issuer mismatch: configured `{issuer}` but the document at \
             {url} claims `{advertised}`. Refusing — a discovery document may say where \
             an issuer's keys are, never who the issuer is."
        );

        let jwks_uri = Url::parse(document.jwks_uri.trim())
            .with_context(|| format!("{url} advertised an invalid jwks_uri"))?;
        require_safe_transport(&jwks_uri, "jwks_uri")?;

        Ok(Self {
            issuer: issuer.to_string(),
            jwks_uri,
        })
    }
}

/// HTTPS, or plain HTTP only on loopback for local development.
///
/// Signing keys fetched over plain HTTP can be replaced in transit, which
/// makes every downstream signature check meaningless. The loopback exception
/// mirrors the project-URL policy so a developer running an IdP container is
/// not forced to terminate TLS.
fn require_safe_transport(url: &Url, what: &str) -> Result<()> {
    ensure!(
        url.host_str().is_some_and(|host| !host.is_empty()),
        "identity provider {what} must include a host"
    );
    let loopback_http = url.scheme() == "http"
        && url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
    ensure!(
        url.scheme() == "https" || loopback_http,
        "identity provider {what} must use HTTPS unless it is a loopback development \
         endpoint (got `{}`)",
        url.scheme()
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "identity provider {what} must not contain credentials"
    );
    Ok(())
}

/// A discovery document plus the moment it was fetched.
///
/// Kept separate from [`OidcDiscovery`] so the cached-ness is not mistaken for
/// part of the verified value.
#[derive(Debug, Clone)]
pub struct CachedDiscovery {
    discovery: OidcDiscovery,
    fetched_at: Instant,
}

impl CachedDiscovery {
    #[must_use]
    pub fn new(discovery: OidcDiscovery) -> Self {
        Self {
            discovery,
            fetched_at: Instant::now(),
        }
    }

    /// The document, or `None` once past [`CACHE_TTL`] or if the issuer no
    /// longer matches — a reconfigured issuer must never be served a cached
    /// document belonging to the previous one.
    #[must_use]
    pub fn get(&self, issuer: &str) -> Option<&OidcDiscovery> {
        let fresh = self.fetched_at.elapsed() < CACHE_TTL;
        let same_issuer = self.discovery.issuer == issuer.trim().trim_end_matches('/');
        (fresh && same_issuer).then_some(&self.discovery)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovery(issuer: &str, jwks: &str) -> OidcDiscovery {
        OidcDiscovery {
            issuer: issuer.to_string(),
            jwks_uri: Url::parse(jwks).unwrap(),
        }
    }

    #[test]
    fn transport_rules_match_the_project_url_policy() {
        assert!(require_safe_transport(&Url::parse("https://a.example/x").unwrap(), "t").is_ok());
        // Loopback HTTP is allowed for local IdP containers.
        assert!(
            require_safe_transport(&Url::parse("http://localhost:8080/x").unwrap(), "t").is_ok()
        );
        assert!(
            require_safe_transport(&Url::parse("http://127.0.0.1:9000/x").unwrap(), "t").is_ok()
        );
        // Everything else over plain HTTP is refused: keys fetched in clear
        // text can be swapped in transit, voiding every signature check.
        assert!(
            require_safe_transport(&Url::parse("http://evil.example/x").unwrap(), "t").is_err()
        );
        // Credentials in the URL are refused rather than transmitted.
        assert!(
            require_safe_transport(&Url::parse("https://u:p@a.example/x").unwrap(), "t").is_err()
        );
    }

    /// The cache is keyed on the issuer, so reconfiguring the provider cannot
    /// be served the previous provider's document.
    #[test]
    fn cache_refuses_a_different_issuer() {
        let cached = CachedDiscovery::new(discovery(
            "https://securetoken.google.com/mirdyne-bose",
            "https://www.googleapis.com/service_accounts/v1/jwk/x",
        ));
        assert!(
            cached
                .get("https://securetoken.google.com/mirdyne-bose")
                .is_some()
        );
        // Trailing slash is the same issuer.
        assert!(
            cached
                .get("https://securetoken.google.com/mirdyne-bose/")
                .is_some()
        );
        // A different project is a different issuer — no cache hit.
        assert!(
            cached
                .get("https://securetoken.google.com/some-other-project")
                .is_none()
        );
        assert!(cached.get("https://acme.supabase.co/auth/v1").is_none());
    }

    /// Every provider PRISM cares about builds the same discovery URL, which
    /// is the whole point — one code path, no per-vendor branches.
    #[test]
    fn discovery_url_is_the_same_shape_for_every_provider() {
        for issuer in [
            "https://securetoken.google.com/mirdyne-bose",
            "https://acme.supabase.co/auth/v1",
            "https://id.example.com/realms/mirdyne",
            "https://id.example.com/oauth/v2",
        ] {
            let base = Url::parse(&format!("{}/", issuer.trim_end_matches('/'))).unwrap();
            let url = base.join(DISCOVERY_PATH).unwrap();
            assert_eq!(
                url.as_str(),
                format!("{issuer}/{DISCOVERY_PATH}"),
                "discovery URL must be issuer + the standard path"
            );
        }
    }
}
