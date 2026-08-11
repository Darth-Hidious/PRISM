//! Discovery of OpenAI-compatible model servers already running on this
//! machine.
//!
//! The bug this exists to kill: a user with a perfectly good local model
//! was told PRISM had none. `prism ingest` printed a template command with
//! a `<model>` placeholder the user had to hand-complete with a URL and an
//! exact model id they had to go find — while Ollama was serving two
//! models on `:11434` the whole time. PRISM never looked.
//!
//! So we look. Every local entry in the provider registry is probed
//! concurrently on a short budget, and whatever the endpoint reports is
//! surfaced verbatim.
//!
//! # Three rules this module keeps
//!
//! **Never stall a command.** A user with nothing running must not wait.
//! Loopback connections to a closed port fail instantly (`ECONNREFUSED`),
//! and the connect timeout caps the pathological case. Discovery is
//! best-effort: every failure path yields "not found", never an error the
//! caller has to handle.
//!
//! **Copy model ids verbatim.** vLLM reports the full Hugging Face repo
//! path or the `--served-model-name` (`opendatalab/MinerU2.5-2509-1.2B`),
//! and a truncated or prettified name is a model the server will refuse.
//! We never reformat an id — we hand back exactly the string the endpoint
//! gave us, so the command we print is a command that works.
//!
//! **Never auto-configure.** Discovery reports; the user chooses. Finding
//! a model and silently routing chat through it is a surprise, and a
//! surprise about where inference runs is the worst kind.
//!
//! # On "loading"
//!
//! vLLM answers `/health` while its weights are still being loaded, and
//! answers `/v1/models` only once it can actually serve. A server in that
//! window is neither absent nor broken, so reporting it as either would be
//! a lie the user acts on — they would go start a second server, or
//! conclude PRISM cannot see the one they just launched. We report it as
//! [`ServerState::Loading`] and say so.

use std::time::Duration;

use serde::Deserialize;
use tokio::task::JoinSet;

use crate::providers::Registry;

/// Connect budget for a speculative loopback probe. Nothing on the other
/// end means `ECONNREFUSED` in microseconds; this only bounds the case
/// where something accepts the socket and then goes quiet.
const PROBE_CONNECT_TIMEOUT: Duration = Duration::from_millis(400);
/// Total budget per speculative probe, including reading the model list.
const PROBE_TOTAL_TIMEOUT: Duration = Duration::from_millis(1_500);

/// Budget for a URL the user typed. They asked for this host by name and
/// it may be a GPU box across a network, so the speculative-sweep budget
/// would report a working server as absent.
const EXPLICIT_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Total budget for an explicitly-named endpoint.
const EXPLICIT_TOTAL_TIMEOUT: Duration = Duration::from_secs(6);

/// Additional loopback ports to sweep, by provider id.
///
/// The registry stays the single source of truth for each vendor's URL
/// *shape* — scheme, host and path all come from its `base_url`, and only
/// the port is substituted here. That keeps this from becoming the second
/// hardcoded endpoint list the registry was built to eliminate.
///
/// `llama-server` defaults to 8080 and users routinely move it to 8081
/// because something else already holds 8080. Missing that server is the
/// exact failure this module exists to fix.
const ALT_PORTS: &[(&str, u16)] = &[("llamacpp", 8081)];

/// Whether a discovered server can serve a request right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    /// `/v1/models` answered — the server is serving.
    Ready,
    /// `/health` answered but `/v1/models` did not: up, still loading.
    Loading,
}

/// One OpenAI-compatible server found listening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalServer {
    /// Registry id of the software behind the port (`ollama`, `vllm`, …),
    /// or `local` for an endpoint the user named that matches nothing we
    /// ship.
    pub provider_id: String,
    /// Human-readable name for listings.
    pub name: String,
    /// The base URL probed, including the API path. This is the exact
    /// string to hand to `prism use local --url`.
    pub base_url: String,
    /// Model ids exactly as the endpoint reported them, in its order.
    pub models: Vec<String>,
    pub state: ServerState,
}

impl LocalServer {
    /// `2 models (qwen2.5:3b, llama3.2:1b)` — or the honest alternative
    /// when the server has nothing to offer yet.
    pub fn summary(&self) -> String {
        match self.state {
            ServerState::Loading => "up, still loading a model".to_string(),
            ServerState::Ready if self.models.is_empty() => {
                "running, but serving no models".to_string()
            }
            ServerState::Ready => {
                let plural = if self.models.len() == 1 { "" } else { "s" };
                format!(
                    "{} model{plural} ({})",
                    self.models.len(),
                    self.models.join(", ")
                )
            }
        }
    }

    /// The exact command that routes PRISM at `model` on this server.
    /// Copy-pasteable by construction: both halves come from the endpoint,
    /// not from a template the user has to complete.
    pub fn use_command(&self, model: &str) -> String {
        format!("prism use local --url {} --model {}", self.base_url, model)
    }

    /// The single model this server obviously means, if there is exactly
    /// one. `None` when it has none (nothing to pick) or several (picking
    /// for the user would be a guess).
    pub fn sole_model(&self) -> Option<&str> {
        match self.models.as_slice() {
            [only] => Some(only.as_str()),
            _ => None,
        }
    }
}

/// Probe every local server in the shipped registry, concurrently.
///
/// Returns registry order so listings are stable run to run. Never fails:
/// an unbuildable HTTP client or a total absence of servers both yield an
/// empty list, because "no local model" is a normal state, not an error.
pub async fn discover() -> Vec<LocalServer> {
    discover_in(&Registry::load()).await
}

/// [`discover`] against an explicit registry. Split out so the sweep is
/// testable against a stub endpoint instead of whatever happens to be
/// running on the developer's machine.
pub async fn discover_in(registry: &Registry) -> Vec<LocalServer> {
    let Ok(client) = probe_client(PROBE_CONNECT_TIMEOUT, PROBE_TOTAL_TIMEOUT) else {
        return Vec::new();
    };

    let mut set = JoinSet::new();
    for (rank, (id, name, url)) in sweep_targets(registry).into_iter().enumerate() {
        let client = client.clone();
        set.spawn(async move { (rank, probe(&client, &id, &name, &url).await) });
    }

    let mut found: Vec<(usize, LocalServer)> = Vec::new();
    while let Some(joined) = set.join_next().await {
        if let Ok((rank, Some(server))) = joined {
            found.push((rank, server));
        }
    }
    found.sort_by_key(|(rank, _)| *rank);
    found.into_iter().map(|(_, server)| server).collect()
}

/// Every `(id, name, base_url)` worth probing: each non-platform registry
/// entry whose base URL is loopback, plus its alternate ports.
///
/// Loopback is the filter because a speculative request to a *remote* host
/// on a user's behalf is a network call they did not ask for. Remote
/// endpoints are reachable through [`probe_url`], which only ever runs on
/// a URL the user typed.
fn sweep_targets(registry: &Registry) -> Vec<(String, String, String)> {
    let mut targets = Vec::new();
    for provider in registry.all().iter().filter(|p| !p.platform) {
        let Some(base_url) = provider.base_url.as_deref() else {
            continue;
        };
        if !is_loopback_url(base_url) {
            continue;
        }
        let name = provider.display_name().to_string();
        targets.push((provider.id.clone(), name.clone(), base_url.to_string()));
        for (id, port) in ALT_PORTS.iter().filter(|(id, _)| *id == provider.id) {
            if let Some(alt) = with_port(base_url, *port) {
                targets.push((id.to_string(), name.clone(), alt));
            }
        }
    }
    targets
}

/// Probe one endpoint the user named. Unlike the sweep this accepts any
/// host — a vLLM server on a GPU box is the normal case — and waits long
/// enough for a real network hop.
///
/// `provider_id`/`name` are resolved from the registry when the URL
/// matches something we ship, so a probe of `http://localhost:11434/v1`
/// reports "Ollama" rather than a bare URL.
pub async fn probe_url(base_url: &str) -> Option<LocalServer> {
    let client = probe_client(EXPLICIT_CONNECT_TIMEOUT, EXPLICIT_TOTAL_TIMEOUT).ok()?;
    let registry = Registry::load();
    let (id, name) = registry
        .all()
        .iter()
        .find(|p| p.base_url.as_deref() == Some(base_url))
        .map(|p| (p.id.clone(), p.display_name().to_string()))
        .unwrap_or_else(|| ("local".to_string(), "Local server".to_string()));
    probe(&client, &id, &name, base_url).await
}

/// Ask one endpoint what it is serving.
///
/// `/v1/models` is the answer we want; `/health` is the tiebreak between
/// "absent" and "loading". Both are GETs against an OpenAI-compatible
/// surface, so one probe covers Ollama, llama.cpp, LM Studio and vLLM
/// without per-vendor branching.
async fn probe(
    client: &reqwest::Client,
    provider_id: &str,
    name: &str,
    base_url: &str,
) -> Option<LocalServer> {
    if let Some(models) = fetch_models(client, base_url).await {
        return Some(LocalServer {
            provider_id: provider_id.to_string(),
            name: name.to_string(),
            base_url: base_url.to_string(),
            models,
            state: ServerState::Ready,
        });
    }
    // No model list. A healthy `/health` means the server is up and
    // warming, which is a different thing to tell the user than "nothing
    // is there".
    if is_healthy(client, base_url).await {
        return Some(LocalServer {
            provider_id: provider_id.to_string(),
            name: name.to_string(),
            base_url: base_url.to_string(),
            models: Vec::new(),
            state: ServerState::Loading,
        });
    }
    None
}

/// The `data[].id` list from an OpenAI-compatible `/v1/models`, verbatim.
/// `None` for anything that is not a successful, parseable response —
/// including a stray HTTP service that happens to hold the port.
async fn fetch_models(client: &reqwest::Client, base_url: &str) -> Option<Vec<String>> {
    let response = client.get(models_url(base_url)).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: ModelsResponse = response.json().await.ok()?;
    Some(body.data.into_iter().map(|entry| entry.id).collect())
}

async fn is_healthy(client: &reqwest::Client, base_url: &str) -> bool {
    let Some(url) = health_url(base_url) else {
        return false;
    };
    client
        .get(url)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// The OpenAI `/v1/models` envelope. Only `data[].id` matters; every other
/// field differs between servers and none of it is load-bearing.
#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
}

fn probe_client(connect: Duration, total: Duration) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(connect)
        .timeout(total)
        .build()
}

/// `{base}/models`. The base already carries its API path — see
/// `prism_ingest::llm::chat_completions_url` for why PRISM takes a base
/// URL at face value instead of synthesising a `/v1` segment.
fn models_url(base_url: &str) -> String {
    format!("{}/models", base_url.trim_end_matches('/'))
}

/// `/health` on the *origin*, not under the API path: vLLM and llama.cpp
/// both mount it at the server root.
fn health_url(base_url: &str) -> Option<String> {
    let mut url = url::Url::parse(base_url).ok()?;
    url.set_path("/health");
    url.set_query(None);
    Some(url.to_string())
}

/// The same base URL on a different port, or `None` if it will not parse.
fn with_port(base_url: &str, port: u16) -> Option<String> {
    let mut url = url::Url::parse(base_url).ok()?;
    url.set_port(Some(port)).ok()?;
    Some(url.to_string().trim_end_matches('/').to_string())
}

/// Whether the URL targets this machine — i.e. the model runs locally.
///
/// Delegates to `prism_runtime::offline::is_loopback_url`. This module used to
/// carry its own copy — same name, same workspace, a different implementation.
/// Its version was the CORRECT one (typed `url::Host` matching), while the
/// runtime's hand-rolled string check treated any domain starting `127.` as
/// loopback. The weaker one guarded the offline policy and, briefly, release
/// of the platform credential.
///
/// Two same-named functions that disagree is the bug shape, independent of
/// which one is right on any given day. There is now one.
pub use prism_runtime::offline::is_loopback_url;

// ── The bundled ONNX embedder ───────────────────────────────────────

/// State of PRISM's built-in local embedding model.
///
/// This is the strongest existing evidence that PRISM stands alone: BGE-
/// small-en-v1.5 on the bundled ONNX Runtime gives `prism query --semantic`
/// real vector search with no server, no key and no account. A user judging
/// whether PRISM needs a subscription should be told it is there.
///
/// **Embedding only.** The vendored runtime (`fastembed`) exposes text
/// embedding, sparse embedding and reranking — it has no text-generation
/// path, so ONNX cannot back chat or ingest extraction today. That is why
/// this type reports a capability rather than pretending to be a chat
/// backend: a generation route that silently cannot generate would be
/// worse than none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnnxEmbedder {
    /// The exact pinned snapshot is verified. Platform usability is a
    /// separate fact; Intel macOS may cache valid bytes it cannot execute.
    pub cached: bool,
    pub platform_supported: bool,
    /// Exact availability, including an actionable reason when unavailable.
    pub status: prism_embed::NativeModelStatus,
    /// Model identity, as shipped.
    pub model: &'static str,
    pub dimensions: usize,
}

/// Report the bundled embedder. This hashes the pinned files but performs no
/// model load and no acquisition.
pub fn onnx_embedder() -> OnnxEmbedder {
    let status = prism_embed::native_model_status();
    let cached = status.is_ready();
    OnnxEmbedder {
        cached,
        platform_supported: prism_embed::native_backend_supported(),
        status,
        model: "bge-small-en-v1.5",
        dimensions: 384,
    }
}

impl OnnxEmbedder {
    /// One line for `prism use list`. Says what works now and gives the
    /// explicit setup action when the pinned snapshot is unavailable.
    pub fn summary(&self) -> String {
        if !self.platform_supported {
            let integrity = if self.cached {
                "snapshot integrity verified"
            } else {
                "snapshot not installed or invalid"
            };
            format!(
                "local ONNX · {} ({}-dim), unavailable on Intel macOS ({integrity}) — configure PRISM_EMBED_BACKEND=openai",
                self.model, self.dimensions
            )
        } else if self.cached {
            format!(
                "local ONNX · {} ({}-dim), cached — offline semantic search, no account",
                self.model, self.dimensions
            )
        } else {
            let reason = self
                .status
                .unavailable()
                .map(|reason| reason.code)
                .map(|code| format!("{code:?}"))
                .unwrap_or_else(|| "Unavailable".to_string());
            format!(
                "local ONNX · {} ({}-dim), unavailable ({reason}) — install explicitly with `{}`",
                self.model,
                self.dimensions,
                prism_embed::BGE_INSTALL_COMMAND
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Provider;

    fn provider(id: &str, base_url: &str) -> Provider {
        Provider {
            id: id.to_string(),
            name: Some(format!("{id} (test)")),
            base_url: Some(base_url.to_string()),
            api_key_env: None,
            docs: None,
            platform: false,
        }
    }

    // ── URL composition ─────────────────────────────────────────────

    #[test]
    fn models_url_appends_to_the_declared_api_path() {
        assert_eq!(
            models_url("http://localhost:11434/v1"),
            "http://localhost:11434/v1/models"
        );
        // A trailing slash must not produce a doubled one.
        assert_eq!(
            models_url("http://localhost:8000/v1/"),
            "http://localhost:8000/v1/models"
        );
        // A base that mounts elsewhere is honoured, not rewritten.
        assert_eq!(
            models_url("http://gpu-box:8000/api/v1"),
            "http://gpu-box:8000/api/v1/models"
        );
    }

    /// vLLM and llama.cpp serve `/health` from the server root, so the
    /// API path must be replaced, never appended to.
    #[test]
    fn health_url_is_on_the_origin_not_under_the_api_path() {
        assert_eq!(
            health_url("http://localhost:8000/v1").as_deref(),
            Some("http://localhost:8000/health")
        );
        assert_eq!(
            health_url("http://gpu-box:8000/api/v1?x=1").as_deref(),
            Some("http://gpu-box:8000/health")
        );
        assert_eq!(health_url("not a url"), None);
    }

    #[test]
    fn with_port_swaps_only_the_port() {
        assert_eq!(
            with_port("http://localhost:8080/v1", 8081).as_deref(),
            Some("http://localhost:8081/v1")
        );
        assert_eq!(with_port("nonsense", 1).as_deref(), None);
    }

    #[test]
    fn is_loopback_url_recognizes_on_device_endpoints() {
        assert!(is_loopback_url("http://localhost:8080"));
        assert!(is_loopback_url("http://LOCALHOST:11434/v1"));
        assert!(is_loopback_url("http://127.0.0.1:8090"));
        assert!(is_loopback_url("http://[::1]:8080/v1"));
        assert!(!is_loopback_url("https://api.openai.com/v1"));
        assert!(!is_loopback_url("http://gpu-box:8000/v1"));
        assert!(!is_loopback_url("not a url"));
    }

    // ── Sweep target selection ──────────────────────────────────────

    #[test]
    fn sweep_covers_every_local_provider_the_registry_ships() {
        let registry = Registry::builtin().unwrap();
        let ids: Vec<String> = sweep_targets(&registry)
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        for expected in ["ollama", "llamacpp", "lmstudio", "vllm"] {
            assert!(
                ids.contains(&expected.to_string()),
                "{expected} must be probed, got {ids:?}"
            );
        }
    }

    /// The sweep must never speculatively contact a host that is not this
    /// machine — that is a network call the user did not ask for.
    #[test]
    fn sweep_never_reaches_off_this_machine() {
        let registry = Registry::builtin().unwrap();
        for (id, _, url) in sweep_targets(&registry) {
            assert!(
                is_loopback_url(&url),
                "{id}: sweep would contact {url}, which is not loopback"
            );
        }
    }

    #[test]
    fn sweep_includes_the_llama_cpp_alternate_port() {
        let registry = Registry::builtin().unwrap();
        let urls: Vec<String> = sweep_targets(&registry)
            .into_iter()
            .map(|(_, _, url)| url)
            .collect();
        assert!(urls.contains(&"http://localhost:8080/v1".to_string()));
        assert!(
            urls.contains(&"http://localhost:8081/v1".to_string()),
            "8081 is where llama-server lands when 8080 is taken, got {urls:?}"
        );
    }

    // ── Probing a stub endpoint ─────────────────────────────────────

    #[tokio::test]
    async fn probe_reports_models_verbatim() {
        let mut server = mockito::Server::new_async().await;
        // A vLLM-shaped id: full HF repo path. Handing back anything but
        // this exact string yields a model the server will refuse.
        let mock = server
            .mock("GET", "/v1/models")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"object":"list","data":[
                     {"id":"opendatalab/MinerU2.5-2509-1.2B","object":"model"},
                     {"id":"Qwen/Qwen2.5-7B-Instruct","object":"model"}
                   ]}"#,
            )
            .create_async()
            .await;

        let client = probe_client(EXPLICIT_CONNECT_TIMEOUT, EXPLICIT_TOTAL_TIMEOUT).unwrap();
        let base = format!("{}/v1", server.url());
        let found = probe(&client, "vllm", "vLLM (local)", &base)
            .await
            .expect("stub server must be discovered");

        assert_eq!(found.state, ServerState::Ready);
        assert_eq!(
            found.models,
            vec![
                "opendatalab/MinerU2.5-2509-1.2B".to_string(),
                "Qwen/Qwen2.5-7B-Instruct".to_string()
            ]
        );
        assert_eq!(found.base_url, base);
        assert_eq!(found.provider_id, "vllm");
        mock.assert_async().await;
    }

    /// The command we print must be runnable as-is — no `<model>`
    /// placeholder, and the id spelled exactly as the server spells it.
    #[test]
    fn use_command_is_ready_to_run() {
        let server = LocalServer {
            provider_id: "vllm".into(),
            name: "vLLM (local)".into(),
            base_url: "http://gpu-box:8000/v1".into(),
            models: vec!["opendatalab/MinerU2.5-2509-1.2B".into()],
            state: ServerState::Ready,
        };
        let cmd = server.use_command(server.sole_model().unwrap());
        assert_eq!(
            cmd,
            "prism use local --url http://gpu-box:8000/v1 \
             --model opendatalab/MinerU2.5-2509-1.2B"
        );
        assert!(!cmd.contains('<'), "no placeholder may survive: {cmd}");
    }

    /// vLLM answers `/health` while weights load. Reporting that server as
    /// absent sends the user off to start another one.
    #[tokio::test]
    async fn probe_reports_loading_when_health_is_up_but_models_is_not() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/v1/models")
            .with_status(503)
            .create_async()
            .await;
        let health = server
            .mock("GET", "/health")
            .with_status(200)
            .create_async()
            .await;

        let client = probe_client(EXPLICIT_CONNECT_TIMEOUT, EXPLICIT_TOTAL_TIMEOUT).unwrap();
        let found = probe(
            &client,
            "vllm",
            "vLLM (local)",
            &format!("{}/v1", server.url()),
        )
        .await
        .expect("a loading server is present, not absent");

        assert_eq!(found.state, ServerState::Loading);
        assert!(found.models.is_empty());
        assert!(found.summary().contains("loading"), "{}", found.summary());
        models.assert_async().await;
        health.assert_async().await;
    }

    #[tokio::test]
    async fn probe_reports_nothing_when_neither_endpoint_answers() {
        let mut server = mockito::Server::new_async().await;
        let models = server
            .mock("GET", "/v1/models")
            .with_status(404)
            .create_async()
            .await;
        let health = server
            .mock("GET", "/health")
            .with_status(404)
            .create_async()
            .await;

        let client = probe_client(EXPLICIT_CONNECT_TIMEOUT, EXPLICIT_TOTAL_TIMEOUT).unwrap();
        let found = probe(
            &client,
            "vllm",
            "vLLM (local)",
            &format!("{}/v1", server.url()),
        )
        .await;

        assert_eq!(found, None);
        models.assert_async().await;
        health.assert_async().await;
    }

    /// A closed port must not be reported as a server, and must not hang.
    #[tokio::test]
    async fn discovery_on_a_dead_machine_is_empty_and_fast() {
        // Ports nothing sane binds, so this is a real "nothing running".
        let registry = Registry::from_providers(vec![
            provider("dead-a", "http://127.0.0.1:1/v1"),
            provider("dead-b", "http://127.0.0.1:2/v1"),
        ]);
        let started = std::time::Instant::now();
        let found = discover_in(&registry).await;
        assert!(found.is_empty(), "got {found:?}");
        assert!(
            started.elapsed() < PROBE_TOTAL_TIMEOUT * 2,
            "discovery must not stall a command, took {:?}",
            started.elapsed()
        );
    }

    /// Live servers are returned in registry order regardless of which
    /// concurrent probe finishes first, so listings are stable.
    #[tokio::test]
    async fn discovery_returns_registry_order_not_completion_order() {
        let mut slow = mockito::Server::new_async().await;
        let _slow = slow
            .mock("GET", "/v1/models")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_chunked_body(|w| {
                // Finish after the "fast" server, so completion order and
                // registry order genuinely disagree.
                std::thread::sleep(Duration::from_millis(150));
                std::io::Write::write_all(w, br#"{"data":[{"id":"first-in-registry"}]}"#)
            })
            .create_async()
            .await;
        let mut fast = mockito::Server::new_async().await;
        let _fast = fast
            .mock("GET", "/v1/models")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":[{"id":"second-in-registry"}]}"#)
            .create_async()
            .await;

        let registry = Registry::from_providers(vec![
            provider("slow", &format!("{}/v1", slow.url())),
            provider("fast", &format!("{}/v1", fast.url())),
        ]);
        let found = discover_in(&registry).await;
        let ids: Vec<&str> = found.iter().map(|s| s.provider_id.as_str()).collect();
        assert_eq!(ids, vec!["slow", "fast"], "got {found:?}");
    }

    // ── Summaries ───────────────────────────────────────────────────

    #[test]
    fn summary_counts_and_names_the_models() {
        let mut server = LocalServer {
            provider_id: "ollama".into(),
            name: "Ollama (local)".into(),
            base_url: "http://localhost:11434/v1".into(),
            models: vec!["qwen2.5:3b".into(), "llama3.2:1b".into()],
            state: ServerState::Ready,
        };
        assert_eq!(server.summary(), "2 models (qwen2.5:3b, llama3.2:1b)");

        server.models = vec!["qwen2.5:3b".into()];
        assert_eq!(server.summary(), "1 model (qwen2.5:3b)");
        assert_eq!(server.sole_model(), Some("qwen2.5:3b"));

        // A server holding nothing must say so rather than read as ready.
        server.models.clear();
        assert_eq!(server.summary(), "running, but serving no models");
        assert_eq!(server.sole_model(), None);
    }

    /// Two models is not "obviously one" — picking would be a guess.
    #[test]
    fn sole_model_refuses_to_choose() {
        let server = LocalServer {
            provider_id: "ollama".into(),
            name: "Ollama (local)".into(),
            base_url: "http://localhost:11434/v1".into(),
            models: vec!["a".into(), "b".into()],
            state: ServerState::Ready,
        };
        assert_eq!(server.sole_model(), None);
    }

    // ── ONNX ────────────────────────────────────────────────────────

    #[test]
    fn onnx_embedder_is_reported_with_its_real_shape() {
        let onnx = onnx_embedder();
        assert_eq!(onnx.model, "bge-small-en-v1.5");
        assert_eq!(onnx.dimensions, 384);
        let summary = onnx.summary();
        assert!(summary.contains("ONNX"), "{summary}");
        assert!(summary.contains("384"), "{summary}");
        // Either state must be honest about offline availability.
        if !onnx.platform_supported {
            assert!(summary.contains("unavailable on Intel macOS"), "{summary}");
        } else if onnx.cached {
            assert!(summary.contains("cached"), "{summary}");
        } else {
            assert!(summary.contains("install explicitly"), "{summary}");
        }
    }
}
