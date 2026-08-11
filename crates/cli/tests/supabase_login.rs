//! Black-box Supabase login coverage against the shipped `prism` binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signer, SigningKey};
use prism_core::rbac::{LocalRole, RbacEngine};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::AsyncReadExt as _;
use tokio::process::{Child, Command};
use tokio::time::{Instant, sleep, timeout};
use url::Url;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

const PRISM: &str = env!("CARGO_BIN_EXE_prism");
const EMAIL: &str = "cli-login@example.test";
const SUBJECT: &str = "supabase-user-123";
const KEY_ID: &str = "prism-test-key";
const ANON_KEY: &str = "anon-key-must-not-appear-in-output";
const REFRESH_TOKEN: &str = "refresh-token-must-not-appear-in-output";
const AUTH_CODE: &str = "authorization-code-must-not-appear-in-output";

const ENV_TO_CLEAR: &[&str] = &[
    "PRISM_API_KEY",
    "PRISM_API_URL",
    "PRISM_PLATFORM_URL",
    "PRISM_TOKEN",
    "PRISM_API_TOKEN",
    "PRISM_PROJECT_ID",
    "PRISM_PLATFORM_PROVIDER",
    "PRISM_LOGIN_TOKEN",
    "PRISM_LOGIN_EMAIL",
    "PRISM_SUPABASE_URL",
    "PRISM_SUPABASE_ANON_KEY",
    "PRISM_OFFLINE",
    "MARC27_API_KEY",
    "MARC27_API_URL",
    "MARC27_PLATFORM_URL",
    "MARC27_TOKEN",
    "MARC27_API_TOKEN",
    "MARC27_PROJECT_ID",
    "MARC27_PLATFORM_PROVIDER",
];

/// Timing policy for the black-box child-process harness.
#[derive(Debug, Clone, Copy)]
struct HarnessPolicy {
    /// How long the real CLI may take to send its OTP request.
    otp_request_timeout: Duration,
    /// Delay between wiremock request-recording checks.
    request_poll_interval: Duration,
    /// End-to-end limit for one real login child.
    child_timeout: Duration,
    /// Limit for the browser-equivalent loopback callback request.
    callback_timeout: Duration,
}

impl Default for HarnessPolicy {
    fn default() -> Self {
        Self {
            otp_request_timeout: Duration::from_secs(30),
            request_poll_interval: Duration::from_millis(20),
            child_timeout: Duration::from_secs(30),
            callback_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TokenCase {
    Valid,
    WrongSigningKey,
    Expired,
    WrongIssuer,
    WrongAudience,
}

#[derive(Debug, Clone, Copy)]
struct LoginSpec {
    token_case: TokenCase,
    role: &'static str,
    mismatched_state: bool,
}

impl Default for LoginSpec {
    fn default() -> Self {
        Self {
            token_case: TokenCase::Valid,
            role: "authenticated",
            mismatched_state: false,
        }
    }
}

struct LoginOutcome {
    root: TempDir,
    output: Output,
    callback_status: reqwest::StatusCode,
    redirect_to: Url,
    otp_body: Value,
    exchange_body: Option<Value>,
    otp_requests: usize,
    exchange_requests: usize,
    jwks_requests: usize,
    access_token: String,
    issuer: String,
}

fn prism(root: &Path) -> Command {
    let mut command = Command::new(PRISM);
    command
        .arg("--project-root")
        .arg(root)
        .current_dir(root)
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("NO_COLOR", "1")
        .env_remove("RUST_LOG")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for name in ENV_TO_CLEAR {
        command.env_remove(name);
    }
    command
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after the Unix epoch")
        .as_secs()
}

fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn sign_jwt(signing_key: &SigningKey, claims: &Value) -> String {
    let header = json!({
        "alg": "EdDSA",
        "typ": "JWT",
        "kid": KEY_ID,
    });
    let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).expect("JWT header"));
    let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("JWT claims"));
    let signing_input = format!("{header}.{claims}");
    let signature = signing_key.sign(signing_input.as_bytes());
    format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

fn jwks(signing_key: &SigningKey) -> Value {
    json!({
        "keys": [{
            "kty": "OKP",
            "crv": "Ed25519",
            "x": URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
            "use": "sig",
            "alg": "EdDSA",
            "kid": KEY_ID,
        }]
    })
}

async fn mount_supabase(server: &MockServer, access_token: &str, advertised_key: &SigningKey) {
    Mock::given(method("POST"))
        .and(path("/auth/v1/otp"))
        .and(header("apikey", ANON_KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/v1/token"))
        .and(query_param("grant_type", "pkce"))
        .and(header("apikey", ANON_KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": access_token,
            "refresh_token": REFRESH_TOKEN,
            "token_type": "bearer",
            "expires_in": 3600,
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/v1/.well-known/jwks.json"))
        .and(header("apikey", ANON_KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks(advertised_key)))
        .mount(server)
        .await;
}

async fn panic_with_child_output(child: &mut Child, reason: &str) -> ! {
    let _ = child.kill().await;
    let status = child.wait().await;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_end(&mut stdout).await;
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_end(&mut stderr).await;
    }
    panic!(
        "{reason}; status: {status:?}; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
}

async fn wait_for_otp(
    server: &MockServer,
    child: &mut Child,
    policy: HarnessPolicy,
) -> (Url, Value) {
    let started = Instant::now();
    loop {
        let requests = server
            .received_requests()
            .await
            .expect("wiremock request recording");
        if let Some(request) = requests
            .iter()
            .find(|request| request.url.path() == "/auth/v1/otp")
        {
            let redirect = request
                .url
                .query_pairs()
                .find(|(key, _)| key == "redirect_to")
                .map(|(_, value)| value.into_owned())
                .expect("OTP request must carry redirect_to");
            let redirect = Url::parse(&redirect).expect("redirect_to must be a URL");
            let body = serde_json::from_slice(&request.body).expect("OTP JSON body");
            return (redirect, body);
        }
        if child
            .try_wait()
            .expect("inspect real prism login child")
            .is_some()
        {
            panic_with_child_output(child, "real prism binary exited before its OTP request").await;
        }
        if started.elapsed() >= policy.otp_request_timeout {
            panic_with_child_output(
                child,
                "real prism binary did not send its Supabase OTP request in time",
            )
            .await;
        }
        sleep(policy.request_poll_interval).await;
    }
}

fn callback_url(redirect_to: &Url, mismatched_state: bool) -> Url {
    let expected_state = redirect_to
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .expect("redirect_to must carry state");
    let returned_state = if mismatched_state {
        "deliberately-mismatched-state"
    } else {
        &expected_state
    };
    let mut callback = redirect_to.clone();
    callback.set_query(None);
    callback
        .query_pairs_mut()
        .append_pair("state", returned_state)
        .append_pair("code", AUTH_CODE);
    callback
}

fn query_value(url: &Url, name: &str) -> String {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
        .unwrap_or_else(|| panic!("URL is missing `{name}`: {url}"))
}

async fn wait_for_output(child: Child, policy: HarnessPolicy) -> Output {
    timeout(policy.child_timeout, child.wait_with_output())
        .await
        .expect("real prism login child timed out")
        .expect("failed to wait for real prism login child")
}

fn request_count(requests: &[Request], path: &str) -> usize {
    requests
        .iter()
        .filter(|request| request.url.path() == path)
        .count()
}

fn pkce_exchange(request: &Request) -> bool {
    request.url.path() == "/auth/v1/token"
        && request
            .url
            .query_pairs()
            .any(|(key, value)| key == "grant_type" && value == "pkce")
}

async fn run_login(spec: LoginSpec) -> LoginOutcome {
    let policy = HarnessPolicy::default();
    let server = MockServer::start().await;
    let issuer = format!("{}/auth/v1", server.uri());
    let now = now_unix_seconds();
    let exp = match spec.token_case {
        TokenCase::Expired => now.saturating_sub(3600),
        _ => now + 3600,
    };
    let token_issuer = match spec.token_case {
        TokenCase::WrongIssuer => "https://wrong-project.example/auth/v1",
        _ => &issuer,
    };
    let audience = match spec.token_case {
        TokenCase::WrongAudience => "wrong-audience",
        _ => "authenticated",
    };
    let claims = json!({
        "sub": SUBJECT,
        "exp": exp,
        "iss": token_issuer,
        "aud": audience,
        "email": EMAIL,
        "role": spec.role,
    });
    let advertised_key = signing_key(7);
    let wrong_key = signing_key(9);
    let token_key = match spec.token_case {
        TokenCase::WrongSigningKey => &wrong_key,
        _ => &advertised_key,
    };
    let access_token = sign_jwt(token_key, &claims);
    mount_supabase(&server, &access_token, &advertised_key).await;

    let root = tempfile::tempdir().expect("isolated HOME");
    let mut command = prism(root.path());
    command.args([
        "login",
        "--provider",
        "supabase",
        "--email",
        EMAIL,
        "--supabase-url",
        &server.uri(),
        "--supabase-anon-key",
        ANON_KEY,
        "--no-browser",
    ]);
    let mut child = command.spawn().expect("spawn real prism binary");

    let (redirect_to, otp_body) = wait_for_otp(&server, &mut child, policy).await;
    let callback = callback_url(&redirect_to, spec.mismatched_state);
    let callback_response = reqwest::Client::builder()
        .timeout(policy.callback_timeout)
        .build()
        .expect("callback HTTP client")
        .get(callback)
        .send()
        .await
        .expect("send loopback callback to real prism binary");
    let callback_status = callback_response.status();
    let output = wait_for_output(child, policy).await;
    let requests = server
        .received_requests()
        .await
        .expect("wiremock request recording");
    let otp_requests = request_count(&requests, "/auth/v1/otp");
    let exchanges = requests
        .iter()
        .filter(|request| pkce_exchange(request))
        .collect::<Vec<_>>();
    let exchange_body = exchanges
        .first()
        .map(|request| serde_json::from_slice(&request.body).expect("PKCE exchange JSON body"));
    let jwks_requests = request_count(&requests, "/auth/v1/.well-known/jwks.json");

    LoginOutcome {
        root,
        output,
        callback_status,
        redirect_to,
        otp_body,
        exchange_body,
        otp_requests,
        exchange_requests: exchanges.len(),
        jwks_requests,
        access_token,
        issuer,
    }
}

fn output_text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn assert_secrets_absent(outcome: &LoginOutcome) {
    let (stdout, stderr) = output_text(&outcome.output);
    for secret in [
        outcome.access_token.as_str(),
        REFRESH_TOKEN,
        ANON_KEY,
        AUTH_CODE,
    ] {
        assert!(
            !stdout.contains(secret),
            "secret leaked to stdout: {stdout}"
        );
        assert!(
            !stderr.contains(secret),
            "secret leaked to stderr: {stderr}"
        );
    }
}

fn credentials_path(root: &Path) -> PathBuf {
    root.join(".prism/credentials.json")
}

fn persisted_credentials(outcome: &LoginOutcome) -> Value {
    let path = credentials_path(outcome.root.path());
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&contents).expect("stored credentials JSON")
}

fn find_named_file(root: &Path, name: &str) -> Option<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|file_name| file_name == name) {
                return Some(path);
            }
        }
    }
    None
}

fn persisted_role(outcome: &LoginOutcome) -> Option<LocalRole> {
    let credentials = persisted_credentials(outcome);
    let principal = credentials["user_id"]
        .as_str()
        .expect("stored canonical principal");
    let expected_principal =
        prism_node::provider_roles::map_supabase_principal(&outcome.issuer, SUBJECT)
            .expect("canonical Supabase principal");
    assert_eq!(principal, expected_principal);
    let rbac_path = find_named_file(outcome.root.path(), "rbac.db")
        .expect("successful login must create the RBAC database");
    let engine = RbacEngine::new(&rbac_path).expect("open persisted RBAC database");
    assert_eq!(
        engine.get_local_role(principal).expect("read local role"),
        None,
        "provider claims must never create a PRISM-local assignment"
    );
    engine.get_role(principal).expect("read effective role")
}

async fn assert_verified_token_refused(token_case: TokenCase) {
    let outcome = run_login(LoginSpec {
        token_case,
        ..LoginSpec::default()
    })
    .await;
    let (_stdout, stderr) = output_text(&outcome.output);

    assert!(!outcome.output.status.success(), "stderr: {stderr}");
    assert_eq!(outcome.callback_status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(outcome.otp_requests, 1);
    assert_eq!(outcome.exchange_requests, 1);
    assert_eq!(outcome.jwks_requests, 1);
    assert!(
        stderr.contains("Supabase access token verification failed"),
        "stderr: {stderr}"
    );
    assert!(!credentials_path(outcome.root.path()).exists());
    assert_secrets_absent(&outcome);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_signing_key_is_refused_by_the_real_login_path() {
    assert_verified_token_refused(TokenCase::WrongSigningKey).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_token_is_refused_by_the_real_login_path() {
    assert_verified_token_refused(TokenCase::Expired).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_issuer_is_refused_by_the_real_login_path() {
    assert_verified_token_refused(TokenCase::WrongIssuer).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_audience_is_refused_by_the_real_login_path() {
    assert_verified_token_refused(TokenCase::WrongAudience).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mismatched_callback_state_is_refused_before_token_exchange() {
    let outcome = run_login(LoginSpec {
        mismatched_state: true,
        ..LoginSpec::default()
    })
    .await;
    let (_stdout, stderr) = output_text(&outcome.output);

    assert!(!outcome.output.status.success(), "stderr: {stderr}");
    assert_eq!(outcome.callback_status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(outcome.otp_requests, 1);
    assert_eq!(outcome.exchange_requests, 0);
    assert_eq!(outcome.jwks_requests, 0);
    assert!(
        stderr.contains("callback state mismatch"),
        "stderr: {stderr}"
    );
    assert!(!credentials_path(outcome.root.path()).exists());
    assert_secrets_absent(&outcome);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unrecognised_role_succeeds_without_a_privileged_local_role() {
    let outcome = run_login(LoginSpec {
        role: "service_role",
        ..LoginSpec::default()
    })
    .await;
    let (stdout, stderr) = output_text(&outcome.output);

    assert!(
        outcome.output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(outcome.callback_status, reqwest::StatusCode::OK);
    assert_eq!(outcome.exchange_requests, 1);
    assert_eq!(outcome.jwks_requests, 1);
    assert_eq!(persisted_role(&outcome), None);
    assert_secrets_absent(&outcome);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_login_persists_viewer_credentials_with_secure_mode() {
    let outcome = run_login(LoginSpec::default()).await;
    let (stdout, stderr) = output_text(&outcome.output);

    assert!(
        outcome.output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(outcome.callback_status, reqwest::StatusCode::OK);
    assert_eq!(outcome.otp_requests, 1);
    assert_eq!(outcome.exchange_requests, 1);
    assert_eq!(outcome.jwks_requests, 1);
    assert_eq!(outcome.redirect_to.scheme(), "http");
    assert_eq!(outcome.redirect_to.host_str(), Some("127.0.0.1"));
    assert!(outcome.redirect_to.port().is_some_and(|port| port != 0));
    assert_eq!(outcome.redirect_to.path(), "/callback");
    assert!(
        outcome
            .redirect_to
            .query_pairs()
            .any(|(key, value)| key == "state" && !value.is_empty())
    );
    assert_eq!(outcome.otp_body["email"], EMAIL);
    assert_eq!(outcome.otp_body["create_user"], false);
    assert_eq!(outcome.otp_body["code_challenge_method"], "s256");
    assert!(
        outcome.otp_body["code_challenge"]
            .as_str()
            .is_some_and(|challenge| !challenge.is_empty())
    );
    let exchange = outcome
        .exchange_body
        .as_ref()
        .expect("real login must exchange its callback code");
    assert_eq!(exchange["auth_code"], AUTH_CODE);
    let verifier = exchange["code_verifier"]
        .as_str()
        .expect("PKCE exchange must carry its verifier");
    assert!(verifier.len() >= 43);
    let expected_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    assert_eq!(
        outcome.otp_body["code_challenge"], expected_challenge,
        "the real login request must relate its challenge to the fresh verifier through S256"
    );

    let credentials = persisted_credentials(&outcome);
    assert_eq!(credentials["access_token"], outcome.access_token);
    assert_eq!(credentials["refresh_token"], REFRESH_TOKEN);
    assert_eq!(credentials["platform_provider"], "supabase");
    assert_eq!(
        credentials["identity_provider_url"],
        outcome.issuer.trim_end_matches("/auth/v1")
    );
    assert_eq!(credentials["identity_provider_key"], ANON_KEY);
    assert_eq!(persisted_role(&outcome), Some(LocalRole::Viewer));

    #[cfg(unix)]
    {
        let mode = fs::metadata(credentials_path(outcome.root.path()))
            .expect("credentials metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
    assert_secrets_absent(&outcome);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consecutive_real_logins_use_fresh_state_challenge_and_verifier() {
    let first = run_login(LoginSpec::default()).await;
    let second = run_login(LoginSpec::default()).await;
    let (_, first_stderr) = output_text(&first.output);
    let (_, second_stderr) = output_text(&second.output);

    assert!(first.output.status.success(), "stderr: {first_stderr}");
    assert!(second.output.status.success(), "stderr: {second_stderr}");
    assert_ne!(
        query_value(&first.redirect_to, "state"),
        query_value(&second.redirect_to, "state")
    );
    assert_ne!(
        first.otp_body["code_challenge"],
        second.otp_body["code_challenge"]
    );
    assert_ne!(
        first.exchange_body.as_ref().expect("first exchange")["code_verifier"],
        second.exchange_body.as_ref().expect("second exchange")["code_verifier"]
    );
    assert_secrets_absent(&first);
    assert_secrets_absent(&second);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supabase_login_without_project_configuration_fails_without_a_default_host() {
    let policy = HarnessPolicy::default();
    let root = tempfile::tempdir().expect("isolated HOME");
    let mut command = prism(root.path());
    command.args(["login", "--provider", "supabase", "--email", EMAIL]);

    let child = command.spawn().expect("spawn real prism binary");
    let output = wait_for_output(child, policy).await;
    let (stdout, stderr) = output_text(&output);
    let combined = format!("{stdout}\n{stderr}");

    assert!(!output.status.success(), "output: {combined}");
    assert!(
        combined.contains("Supabase is not configured"),
        "{combined}"
    );
    assert!(combined.contains("PRISM_SUPABASE_URL"), "{combined}");
    assert!(!combined.contains(".supabase.co"), "{combined}");
    assert!(!combined.contains("https://"), "{combined}");
    assert!(!credentials_path(root.path()).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_marc27_url_is_never_relabelled_as_a_supabase_project() {
    let policy = HarnessPolicy::default();
    let unrelated = MockServer::start().await;
    let root = tempfile::tempdir().expect("isolated HOME");
    fs::create_dir_all(root.path().join(".prism")).expect("project config directory");
    fs::write(
        root.path().join(".prism/prism.toml"),
        format!(
            "[platform]\nurl = \"{}\"\nprovider = \"marc27\"\n",
            unrelated.uri()
        ),
    )
    .expect("MARC27 project config");
    let mut command = prism(root.path());
    command.env("PRISM_API_KEY", ANON_KEY).args([
        "login",
        "--provider",
        "supabase",
        "--email",
        EMAIL,
    ]);

    let child = command.spawn().expect("spawn real prism binary");
    let output = wait_for_output(child, policy).await;
    let (stdout, stderr) = output_text(&output);
    let combined = format!("{stdout}\n{stderr}");

    assert!(!output.status.success(), "output: {combined}");
    assert!(
        combined.contains("Supabase is not configured"),
        "{combined}"
    );
    assert!(!combined.contains(ANON_KEY), "anon key leaked: {combined}");
    assert_eq!(
        unrelated
            .received_requests()
            .await
            .expect("wiremock request recording")
            .len(),
        0,
        "login must reject the stale MARC27 URL before any Supabase request"
    );
    assert!(!credentials_path(root.path()).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preissued_token_mode_is_refused_for_supabase_before_any_request() {
    let policy = HarnessPolicy::default();
    let project = MockServer::start().await;
    let root = tempfile::tempdir().expect("isolated HOME");
    let secret = "unverified-supabase-token-must-not-leak";
    let mut command = prism(root.path());
    command
        .env("PRISM_API_URL", project.uri())
        .env("PRISM_PLATFORM_PROVIDER", "supabase")
        .env("PRISM_API_KEY", ANON_KEY)
        .args(["login", "--token", secret]);

    let child = command.spawn().expect("spawn real prism binary");
    let output = wait_for_output(child, policy).await;
    let (stdout, stderr) = output_text(&output);
    let combined = format!("{stdout}\n{stderr}");

    assert!(!output.status.success(), "output: {combined}");
    assert!(
        combined.contains("pre-issued token login is not supported for Supabase"),
        "{combined}"
    );
    assert!(!combined.contains(secret), "token leaked: {combined}");
    assert_eq!(
        project
            .received_requests()
            .await
            .expect("wiremock request recording")
            .len(),
        0,
        "Supabase token mode must fail before any request"
    );
    assert!(!credentials_path(root.path()).exists());
}
