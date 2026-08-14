//! Supabase Auth integration for the PRISM CLI.
//!
//! Supabase does not implement RFC 8628. This module therefore owns a real
//! email magic-link PKCE flow and never routes Supabase through
//! [`crate::auth::DeviceFlowAuth`].

use std::collections::HashSet;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use url::Url;

use crate::auth::TokenResponse;

const AUTH_PATH: &str = "auth/v1/";
const CALLBACK_PATH: &str = "/callback";

/// Operational policy for one Supabase authentication attempt.
///
/// Security invariants such as binding only `127.0.0.1`, using an ephemeral
/// port, and requiring PKCE S256 are intentionally not configurable.
#[derive(Debug, Clone)]
pub struct SupabaseAuthPolicy {
    /// Maximum time to wait for the email link to return to the local callback.
    pub callback_timeout: Duration,
    /// Maximum time to wait while reading the browser's callback request.
    pub callback_request_timeout: Duration,
    /// Per-request timeout for Supabase Auth and JWKS HTTP calls.
    pub request_timeout: Duration,
    /// Allowed clock skew while validating `exp`.
    pub clock_skew: Duration,
    /// Exact JWT audience accepted for an authenticated user session.
    pub expected_audience: String,
    /// Maximum callback request-header size accepted from the local browser.
    pub max_callback_request_bytes: usize,
}

impl Default for SupabaseAuthPolicy {
    fn default() -> Self {
        Self {
            callback_timeout: Duration::from_secs(5 * 60),
            callback_request_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
            clock_skew: Duration::from_secs(60),
            expected_audience: "authenticated".to_string(),
            max_callback_request_bytes: 16 * 1024,
        }
    }
}

/// Verified claims used at PRISM's identity and role boundary.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SupabaseClaims {
    pub sub: String,
    pub exp: u64,
    pub iss: String,
    pub aud: JwtAudience,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
}

/// Supabase accepts either the JWT string or array form of `aud`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum JwtAudience {
    One(String),
    Many(Vec<String>),
}

/// Supabase's token response. Secret fields are always redacted from Debug.
#[derive(Clone, Deserialize)]
struct SupabaseTokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

impl fmt::Debug for SupabaseTokenResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SupabaseTokenResponse")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("token_type", &self.token_type)
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

impl From<SupabaseTokenResponse> for TokenResponse {
    fn from(value: SupabaseTokenResponse) -> Self {
        Self {
            access_token: value.access_token,
            refresh_token: value.refresh_token,
            token_type: value.token_type,
            expires_in: value.expires_in,
            config: None,
        }
    }
}

/// A token response whose JWT has already passed signature and claim checks.
#[derive(Clone)]
pub struct SupabaseSession {
    pub tokens: TokenResponse,
    pub claims: SupabaseClaims,
}

impl fmt::Debug for SupabaseSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SupabaseSession")
            .field("tokens", &self.tokens)
            .field("claims", &self.claims)
            .finish()
    }
}

/// Live local callback state for one PKCE attempt.
///
/// This deliberately has no `Debug` implementation: the fresh verifier and
/// state value must not become convenient logging material.
pub struct SupabasePkceAttempt {
    listener: TcpListener,
    redirect_uri: Url,
    state: String,
    code_verifier: String,
}

/// Which enterprise SAML connection to start.
///
/// Exactly one of these, never both and never neither — an SSO request that
/// names no connection is ambiguous, and one that names two lets the server
/// pick, which is the caller silently losing control of which organisation
/// authenticates the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SsoSelector<'a> {
    /// Email domain of the organisation, e.g. `arianegroup.com`. This is the
    /// "sign in with your work email" affordance.
    Domain(&'a str),
    /// Explicit connection id, for organisations with several or with none
    /// registered against a domain.
    ProviderId(&'a str),
}

impl<'a> SsoSelector<'a> {
    /// Validate and lower to the wire field name and value.
    fn as_field(self) -> Result<(&'static str, &'a str)> {
        let (field, value) = match self {
            Self::Domain(domain) => ("domain", domain.trim()),
            Self::ProviderId(id) => ("provider_id", id.trim()),
        };
        ensure!(!value.is_empty(), "SAML SSO {field} must not be empty");
        // A domain is a domain, not a URL and not an email address. Accepting
        // `user@acme.com` here would send the local part of someone's address
        // to the IdP directory, and accepting a URL would let a caller aim the
        // lookup somewhere unintended.
        if matches!(self, Self::Domain(_)) {
            ensure!(
                !value.contains('@'),
                "SAML SSO domain must be a bare domain like `acme.com`, not an email address"
            );
            ensure!(
                !value.contains("://") && !value.contains('/'),
                "SAML SSO domain must be a bare domain like `acme.com`, not a URL"
            );
            ensure!(
                value.contains('.'),
                "SAML SSO domain must be a fully qualified domain like `acme.com`"
            );
        }
        Ok((field, value))
    }
}

/// Supabase email magic-link PKCE client.
pub struct SupabaseAuth {
    client: reqwest::Client,
    project_url: Url,
    anon_key: String,
    policy: SupabaseAuthPolicy,
}

impl fmt::Debug for SupabaseAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SupabaseAuth")
            .field("project_url", &self.project_url)
            .field("anon_key", &"[REDACTED]")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl SupabaseAuth {
    /// Construct a client for an explicitly configured Supabase project.
    pub fn new(
        client: reqwest::Client,
        project_url: &str,
        anon_key: &str,
        policy: SupabaseAuthPolicy,
    ) -> Result<Self> {
        let mut project_url = Url::parse(project_url.trim())
            .with_context(|| format!("invalid Supabase project URL: {project_url}"))?;
        ensure!(
            project_url.host_str().is_some(),
            "Supabase project URL must include a host"
        );
        let loopback_http = project_url.scheme() == "http"
            && project_url.host_str().is_some_and(|host| {
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .parse::<IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            });
        ensure!(
            project_url.scheme() == "https" || loopback_http,
            "Supabase project URL must use HTTPS unless it is a loopback development endpoint"
        );
        ensure!(
            project_url.username().is_empty() && project_url.password().is_none(),
            "Supabase project URL must not contain credentials"
        );
        ensure!(
            project_url.query().is_none() && project_url.fragment().is_none(),
            "Supabase project URL must not contain a query or fragment"
        );
        let anon_key = anon_key.trim();
        ensure!(!anon_key.is_empty(), "Supabase anon key is not configured");

        let normalized_path = format!("{}/", project_url.path().trim_end_matches('/'));
        project_url.set_path(&normalized_path);

        Ok(Self {
            client,
            project_url,
            anon_key: anon_key.to_string(),
            policy,
        })
    }

    /// Canonical issuer required from this project's access tokens.
    pub fn expected_issuer(&self) -> Result<String> {
        Ok(self
            .auth_endpoint("")?
            .as_str()
            .trim_end_matches('/')
            .to_string())
    }

    /// Begin a passwordless email magic-link login.
    ///
    /// A magic-link PKCE redirect fits a CLI better than a password grant: the
    /// CLI handles no password, the user confirms identity in their existing
    /// browser, and the one-time code returns only to an ephemeral loopback
    /// listener. Email OTP entry is not used because Supabase projects may
    /// render the same route as a magic link depending on their email template.
    pub async fn begin_email_login(&self, email: &str) -> Result<SupabasePkceAttempt> {
        self.offline_guard("start Supabase login")?;
        let email = email.trim();
        ensure!(!email.is_empty(), "email is required for Supabase login");

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .context("failed to bind the Supabase login callback to 127.0.0.1")?;
        let address = listener
            .local_addr()
            .context("failed to read the Supabase callback address")?;

        let state = random_urlsafe(32);
        let code_verifier = random_urlsafe(32);
        let code_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));

        let mut redirect_uri = Url::parse(&format!("http://127.0.0.1:{}/callback", address.port()))
            .context("failed to build the Supabase callback URL")?;
        redirect_uri.query_pairs_mut().append_pair("state", &state);

        #[derive(Serialize)]
        struct OtpRequest<'a> {
            email: &'a str,
            create_user: bool,
            code_challenge: &'a str,
            code_challenge_method: &'static str,
        }

        let response = self
            .client
            .post(self.auth_endpoint("otp")?)
            .header("apikey", &self.anon_key)
            .query(&[("redirect_to", redirect_uri.as_str())])
            .json(&OtpRequest {
                email,
                create_user: false,
                code_challenge: &code_challenge,
                code_challenge_method: "s256",
            })
            .timeout(self.policy.request_timeout)
            .send()
            .await
            .context("failed to request a Supabase login email")?;
        require_success(response, "Supabase login email request").await?;

        Ok(SupabasePkceAttempt {
            listener,
            redirect_uri,
            state,
            code_verifier,
        })
    }

    /// Begin SAML 2.0 enterprise SSO and return the attempt plus the URL the
    /// user must open at their own identity provider.
    ///
    /// # PRISM IS NOT A SAML SERVICE PROVIDER, DELIBERATELY
    ///
    /// SAML's dangerous half is verifying a signed XML assertion: signature
    /// wrapping, canonicalisation and transform handling have broken real
    /// service providers repeatedly, and the bugs are silent — a wrapped
    /// assertion authenticates as the wrong user against code that looks
    /// correct. Putting an XML-DSIG verifier in a research client would place
    /// that surface on every install, on every laptop.
    ///
    /// So the SAML exchange happens entirely between the customer's IdP and
    /// the identity provider PRISM already trusts. PRISM initiates, the IdP
    /// asserts, and what comes back to PRISM is the SAME signed JWT and the
    /// SAME PKCE code exchange as a passwordless login — verified by
    /// [`Self::verify_access_token`], which already enforces signature, `exp`,
    /// `iss` and `aud`. There is no XML anywhere in this crate.
    ///
    /// Completion is [`Self::complete_email_login`] unchanged: SSO and email
    /// login converge on one code-exchange path, so there is no second
    /// token-handling route to keep correct.
    ///
    /// `selector` picks the enterprise connection — by email domain (the
    /// usual "sign in with your work email" affordance) or by an explicit
    /// provider id.
    pub async fn begin_sso_login(
        &self,
        selector: SsoSelector<'_>,
    ) -> Result<(SupabasePkceAttempt, Url)> {
        self.offline_guard("start SAML SSO login")?;
        let (field, value) = selector.as_field()?;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .context("failed to bind the SSO login callback to 127.0.0.1")?;
        let address = listener
            .local_addr()
            .context("failed to read the SSO callback address")?;

        let state = random_urlsafe(32);
        let code_verifier = random_urlsafe(32);
        let code_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));

        let mut redirect_uri = Url::parse(&format!("http://127.0.0.1:{}/callback", address.port()))
            .context("failed to build the SSO callback URL")?;
        redirect_uri.query_pairs_mut().append_pair("state", &state);

        #[derive(Serialize)]
        struct SsoRequest<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            domain: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            provider_id: Option<&'a str>,
            redirect_to: &'a str,
            code_challenge: &'a str,
            code_challenge_method: &'static str,
        }

        let body = SsoRequest {
            domain: (field == "domain").then_some(value),
            provider_id: (field == "provider_id").then_some(value),
            redirect_to: redirect_uri.as_str(),
            code_challenge: &code_challenge,
            code_challenge_method: "s256",
        };

        let response = self
            .client
            .post(self.auth_endpoint("sso")?)
            .header("apikey", &self.anon_key)
            .json(&body)
            .timeout(self.policy.request_timeout)
            .send()
            .await
            .context("failed to start SAML SSO")?;
        let response = require_success(response, "SAML SSO initiation").await?;

        #[derive(Deserialize)]
        struct SsoResponse {
            url: String,
        }
        let sso: SsoResponse = response
            .json()
            .await
            .context("SAML SSO initiation returned no redirect URL")?;

        // The redirect target is chosen by the IdP configuration, not by us,
        // so it is treated as untrusted input: it must parse, and it must be
        // HTTPS. Handing a user an `http://` or `javascript:` URL to open —
        // or a malformed one — is how an SSO entry point becomes a phishing
        // surface. A plain-HTTP IdP would also carry the relay state in
        // clear text.
        let url = Url::parse(sso.url.trim())
            .context("SAML SSO returned a redirect URL that is not a valid URL")?;
        ensure!(
            url.scheme() == "https",
            "SAML SSO redirect must be https, got `{}` — refusing to open it",
            url.scheme()
        );
        ensure!(
            url.host_str().is_some_and(|host| !host.is_empty()),
            "SAML SSO redirect has no host"
        );

        Ok((
            SupabasePkceAttempt {
                listener,
                redirect_uri,
                state,
                code_verifier,
            },
            url,
        ))
    }

    /// Wait for the loopback redirect, exchange its code, and verify the JWT.
    pub async fn complete_email_login(
        &self,
        attempt: SupabasePkceAttempt,
    ) -> Result<SupabaseSession> {
        let SupabasePkceAttempt {
            listener,
            redirect_uri: _,
            state,
            code_verifier,
        } = attempt;

        let (mut stream, _) = timeout(self.policy.callback_timeout, listener.accept())
            .await
            .context("timed out waiting for the Supabase login callback")?
            .context("failed to accept the Supabase login callback")?;

        let callback = self.read_callback(&mut stream, &state).await;
        let code = match callback {
            Ok(code) => code,
            Err(error) => {
                let _ = write_callback_response(
                    &mut stream,
                    400,
                    "PRISM rejected this login callback. Return to the terminal.",
                )
                .await;
                return Err(error);
            }
        };

        let session = self.exchange_code(&code, &code_verifier).await;
        match session {
            Ok(session) => {
                let _ = write_callback_response(
                    &mut stream,
                    200,
                    "PRISM login complete. You may close this window.",
                )
                .await;
                Ok(session)
            }
            Err(error) => {
                let _ = write_callback_response(
                    &mut stream,
                    400,
                    "PRISM could not verify this login. Return to the terminal.",
                )
                .await;
                Err(error)
            }
        }
    }

    /// Refresh a session through Supabase and verify the replacement JWT.
    pub async fn refresh_session(&self, refresh_token: &str) -> Result<SupabaseSession> {
        self.offline_guard("refresh Supabase login")?;
        ensure!(!refresh_token.trim().is_empty(), "refresh token is empty");

        #[derive(Serialize)]
        struct RefreshRequest<'a> {
            refresh_token: &'a str,
        }

        let response = self
            .client
            .post(self.auth_endpoint("token")?)
            .header("apikey", &self.anon_key)
            .query(&[("grant_type", "refresh_token")])
            .json(&RefreshRequest { refresh_token })
            .timeout(self.policy.request_timeout)
            .send()
            .await
            .context("failed to refresh the Supabase session")?;
        let response = require_success(response, "Supabase token refresh").await?;
        let tokens = response
            .json::<SupabaseTokenResponse>()
            .await
            .context("failed to parse the Supabase refresh response")?;
        self.verify_tokens(tokens).await
    }

    async fn exchange_code(&self, code: &str, code_verifier: &str) -> Result<SupabaseSession> {
        self.offline_guard("exchange Supabase login code")?;

        #[derive(Serialize)]
        struct ExchangeRequest<'a> {
            auth_code: &'a str,
            code_verifier: &'a str,
        }

        let response = self
            .client
            .post(self.auth_endpoint("token")?)
            .header("apikey", &self.anon_key)
            .query(&[("grant_type", "pkce")])
            .json(&ExchangeRequest {
                auth_code: code,
                code_verifier,
            })
            .timeout(self.policy.request_timeout)
            .send()
            .await
            .context("failed to exchange the Supabase login code")?;
        let response = require_success(response, "Supabase PKCE token exchange").await?;
        let tokens = response
            .json::<SupabaseTokenResponse>()
            .await
            .context("failed to parse the Supabase token response")?;
        self.verify_tokens(tokens).await
    }

    async fn verify_tokens(&self, tokens: SupabaseTokenResponse) -> Result<SupabaseSession> {
        let claims = self.verify_access_token(&tokens.access_token).await?;
        Ok(SupabaseSession {
            tokens: tokens.into(),
            claims,
        })
    }

    /// Verify signature, expiration, issuer, and audience against this project.
    pub async fn verify_access_token(&self, token: &str) -> Result<SupabaseClaims> {
        self.offline_guard("fetch Supabase signing keys")?;
        let header = decode_header(token).context("Supabase access token has an invalid header")?;
        let kid = header
            .kid
            .as_deref()
            .context("Supabase access token is missing a signing-key id")?;
        ensure!(
            is_asymmetric_algorithm(header.alg),
            "Supabase access token must use an asymmetric JWKS signing key"
        );

        // Where the keys live is ASKED, not assumed. The issuer publishes it
        // in its discovery document; PRISM no longer hardcodes one vendor's
        // URL layout, which is what previously made the provider choice a
        // code change rather than a configuration one.
        //
        // `OidcDiscovery::fetch` proves the document belongs to this issuer
        // before its `jwks_uri` is used, so the configured issuer stays the
        // root of trust. If discovery fails, verification fails — there is no
        // guessed-path fallback to quietly reinstate the old assumption.
        let issuer = self.expected_issuer()?;
        let discovery =
            crate::oidc::OidcDiscovery::fetch(&self.client, &issuer, self.policy.request_timeout)
                .await?;

        let response = self
            .client
            .get(discovery.jwks_uri().clone())
            .header("apikey", &self.anon_key)
            .timeout(self.policy.request_timeout)
            .send()
            .await
            .with_context(|| {
                format!("failed to fetch signing keys from {}", discovery.jwks_uri())
            })?;
        let response = require_success(response, "JWKS request").await?;
        let jwks = response
            .json::<JwkSet>()
            .await
            .context("failed to parse the Supabase JWKS")?;
        let jwk = jwks
            .find(kid)
            .with_context(|| format!("Supabase JWKS has no key matching kid {kid}"))?;
        let decoding_key = DecodingKey::from_jwk(jwk)
            .context("Supabase JWKS contains an unsupported signing key")?;

        // Validate against the DISCOVERY-VERIFIED issuer. It equals the
        // configured one by construction (fetch refuses a mismatch), so this
        // cannot widen what is accepted — it just makes the single source of
        // the issuer string obvious at the point it is enforced.
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[discovery.issuer()]);
        validation.set_audience(&[self.policy.expected_audience.as_str()]);
        validation.leeway = self.policy.clock_skew.as_secs();
        validation.required_spec_claims = ["exp", "iss", "aud", "sub"]
            .into_iter()
            .map(str::to_string)
            .collect::<HashSet<_>>();
        // Honor the `nbf` ("not before") claim: a token that declares itself not
        // yet valid must be refused, not silently accepted.
        validation.validate_nbf = true;

        let verified = decode::<SupabaseClaims>(token, &decoding_key, &validation)
            .context("Supabase access token verification failed")?;
        ensure!(
            !verified.claims.sub.trim().is_empty(),
            "Supabase access token has an empty subject"
        );
        Ok(verified.claims)
    }

    async fn read_callback(&self, stream: &mut TcpStream, expected_state: &str) -> Result<String> {
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            ensure!(
                request.len() < self.policy.max_callback_request_bytes,
                "Supabase login callback request is too large"
            );
            let read = timeout(
                self.policy.callback_request_timeout,
                stream.read(&mut chunk),
            )
            .await
            .context("timed out reading the Supabase login callback")?
            .context("failed to read the Supabase login callback")?;
            ensure!(
                read != 0,
                "Supabase login callback closed before a request arrived"
            );
            request.extend_from_slice(&chunk[..read]);
            ensure!(
                request.len() <= self.policy.max_callback_request_bytes,
                "Supabase login callback request is too large"
            );
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let request = std::str::from_utf8(&request)
            .context("Supabase login callback was not valid HTTP text")?;
        let request_line = request
            .lines()
            .next()
            .context("Supabase login callback had no request line")?;
        let mut parts = request_line.split_whitespace();
        ensure!(
            parts.next() == Some("GET"),
            "Supabase login callback must use GET"
        );
        let target = parts
            .next()
            .context("Supabase login callback had no request target")?;
        let callback_url = if target.starts_with("http://") || target.starts_with("https://") {
            Url::parse(target)
        } else {
            Url::parse(&format!("http://127.0.0.1{target}"))
        }
        .context("Supabase login callback URL was invalid")?;
        ensure!(
            callback_url.path() == CALLBACK_PATH,
            "Supabase login callback used an unexpected path"
        );

        let query = callback_url.query_pairs().collect::<Vec<_>>();
        if let Some(error) = query
            .iter()
            .find(|(key, _)| key == "error")
            .map(|(_, value)| value.as_ref())
        {
            bail!("Supabase login was rejected: {error}");
        }
        let returned_state = query
            .iter()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.as_ref())
            .context("Supabase login callback is missing state")?;
        ensure!(
            constant_time_eq(returned_state.as_bytes(), expected_state.as_bytes()),
            "Supabase login callback state mismatch"
        );
        let code = query
            .iter()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.to_string())
            .context("Supabase login callback is missing the authorization code")?;
        ensure!(
            !code.trim().is_empty(),
            "Supabase login callback code is empty"
        );
        Ok(code)
    }

    fn auth_endpoint(&self, suffix: &str) -> Result<Url> {
        self.project_url
            .join(&format!("{AUTH_PATH}{suffix}"))
            .context("failed to build a Supabase Auth endpoint")
    }

    fn offline_guard(&self, action: &str) -> Result<()> {
        if prism_runtime::offline::enabled() {
            bail!("offline mode: cannot {action} (remove --offline to continue)");
        }
        Ok(())
    }
}

impl SupabasePkceAttempt {
    /// Loopback URL registered for this attempt. It always contains fresh state.
    pub fn redirect_uri(&self) -> &Url {
        &self.redirect_uri
    }
}

async fn require_success(
    response: reqwest::Response,
    operation: &str,
) -> Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        Ok(response)
    } else {
        bail!("{operation} failed with HTTP {status}")
    }
}

async fn write_callback_response(stream: &mut TcpStream, status: u16, body: &str) -> Result<()> {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .context("failed to write the Supabase callback response")
}

fn random_urlsafe(bytes: usize) -> String {
    let mut random = vec![0_u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut random);
    URL_SAFE_NO_PAD.encode(random)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn is_asymmetric_algorithm(algorithm: Algorithm) -> bool {
    matches!(
        algorithm,
        Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::PS256
            | Algorithm::PS384
            | Algorithm::PS512
            | Algorithm::ES256
            | Algorithm::ES384
            | Algorithm::EdDSA
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_supabase_projects_require_https() {
        let policy = SupabaseAuthPolicy::default();
        SupabaseAuth::new(
            reqwest::Client::new(),
            "https://project.supabase.co",
            "anon-key",
            policy.clone(),
        )
        .expect("remote HTTPS project");
        SupabaseAuth::new(
            reqwest::Client::new(),
            "http://127.0.0.1:54321",
            "anon-key",
            policy.clone(),
        )
        .expect("loopback development project");
        SupabaseAuth::new(
            reqwest::Client::new(),
            "http://[::1]:54321",
            "anon-key",
            policy.clone(),
        )
        .expect("IPv6 loopback development project");

        let error = SupabaseAuth::new(
            reqwest::Client::new(),
            "http://project.supabase.co",
            "anon-key",
            policy,
        )
        .expect_err("remote cleartext project must be rejected")
        .to_string();
        assert!(error.contains("must use HTTPS"), "{error}");
    }

    #[test]
    fn secret_debug_output_is_redacted() {
        let token = SupabaseTokenResponse {
            access_token: "access-secret".to_string(),
            refresh_token: "refresh-secret".to_string(),
            token_type: Some("bearer".to_string()),
            expires_in: Some(3600),
        };
        let rendered = format!("{token:?}");
        assert!(!rendered.contains("access-secret"));
        assert!(!rendered.contains("refresh-secret"));
        assert_eq!(rendered.matches("[REDACTED]").count(), 2);

        let session = SupabaseSession {
            tokens: token.into(),
            claims: SupabaseClaims {
                sub: "user-123".to_string(),
                exp: 4_000_000_000,
                iss: "https://project.supabase.co/auth/v1".to_string(),
                aud: JwtAudience::One("authenticated".to_string()),
                email: Some("user@example.com".to_string()),
                role: Some("authenticated".to_string()),
            },
        };
        let rendered = format!("{session:?}");
        assert!(!rendered.contains("access-secret"));
        assert!(!rendered.contains("refresh-secret"));
    }

    #[test]
    fn pkce_inputs_are_fresh_and_rfc_7636_sized() {
        let first_state = random_urlsafe(32);
        let second_state = random_urlsafe(32);
        let first_verifier = random_urlsafe(32);
        let second_verifier = random_urlsafe(32);
        assert_ne!(first_state, second_state);
        assert_ne!(first_verifier, second_verifier);
        assert!(first_verifier.len() >= 43);
        assert!(second_verifier.len() >= 43);
    }

    #[test]
    fn state_comparison_rejects_any_difference() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"diff"));
        assert!(!constant_time_eq(b"same", b"same-longer"));
    }

    /// An SSO selector names exactly one connection, and a domain is a bare
    /// domain. An email address here would leak the local part of someone's
    /// address into an IdP directory lookup; a URL would aim that lookup
    /// somewhere the caller did not intend.
    #[test]
    fn sso_domain_must_be_a_bare_domain() {
        assert_eq!(
            SsoSelector::Domain("arianegroup.com").as_field().unwrap(),
            ("domain", "arianegroup.com")
        );
        // Whitespace is trimmed, not rejected — pasted input routinely carries it.
        assert_eq!(
            SsoSelector::Domain("  acme.com ").as_field().unwrap(),
            ("domain", "acme.com")
        );
        assert_eq!(
            SsoSelector::ProviderId("conn-123").as_field().unwrap(),
            ("provider_id", "conn-123")
        );

        for bad in [
            "user@acme.com",    // email, not a domain
            "https://acme.com", // URL
            "acme.com/sso",     // path
            "acme",             // not fully qualified
            "",                 // empty
            "   ",              // whitespace only
        ] {
            assert!(
                SsoSelector::Domain(bad).as_field().is_err(),
                "`{bad}` must be refused as an SSO domain"
            );
        }
        assert!(SsoSelector::ProviderId("  ").as_field().is_err());
    }

    /// The IdP redirect is untrusted input. Anything but HTTPS with a host is
    /// refused rather than handed to the user to open — an SSO entry point
    /// that will open arbitrary schemes is a phishing surface, and plain HTTP
    /// would carry the relay state in clear text.
    #[test]
    fn only_https_sso_redirects_are_accepted() {
        let ok = Url::parse("https://login.arianegroup.com/sso/saml").unwrap();
        assert_eq!(ok.scheme(), "https");
        assert!(ok.host_str().is_some_and(|h| !h.is_empty()));

        for bad in [
            "http://login.acme.com/sso",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,<script>",
        ] {
            let parsed = Url::parse(bad);
            let rejected = match parsed {
                Err(_) => true,
                Ok(url) => url.scheme() != "https" || url.host_str().is_none_or(str::is_empty),
            };
            assert!(rejected, "`{bad}` must not be opened as an SSO redirect");
        }
    }
}
