use anyhow::{Context, anyhow};
use forge_app::{HttpResponse, NetFetchService, ResponseContext, is_binary_content_type};
use reqwest::{Client, Url};

/// Retrieves content from URLs as markdown or raw text. Enables access to
/// current online information including websites, APIs and documentation. Use
/// for obtaining up-to-date information beyond training data, verifying facts,
/// or retrieving specific online content. Handles HTTP/HTTPS and converts HTML
/// to readable markdown by default. Cannot access private/restricted resources
/// requiring authentication. Respects robots.txt and may be blocked by
/// anti-scraping measures. For large pages, returns the first 40,000 characters
/// and stores the complete content in a temporary file for subsequent access.
#[derive(Debug)]
pub struct ForgeFetch {
    client: Client,
}

impl Default for ForgeFetch {
    fn default() -> Self {
        Self::new()
    }
}

impl ForgeFetch {
    pub fn new() -> Self {
        // Authoritative research sources (NASA NTRS, journals, Cloudflare-
        // fronted sites) 403 the default reqwest user-agent. Observed live:
        // NTRS GRCop-42 PDFs returned 403 to PRISM's mission research. A
        // realistic browser UA + redirect/timeout policy makes the fetch
        // tool usable for research-grade sources. Falls back to the bare
        // client if the builder ever fails (never panics the tool).
        let client = Client::builder()
            .user_agent(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                 AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/124.0.0.0 Safari/537.36",
            )
            .redirect(reqwest::redirect::Policy::limited(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self { client }
    }
}

impl ForgeFetch {
    async fn check_robots_txt(&self, url: &Url) -> anyhow::Result<()> {
        let robots_url = format!("{}://{}/robots.txt", url.scheme(), url.authority());
        let robots_response = self.client.get(&robots_url).send().await;

        if let Ok(robots) = robots_response
            && robots.status().is_success()
        {
            let robots_content = robots.text().await.unwrap_or_default();
            let path = url.path();
            for line in robots_content.lines() {
                if let Some(disallowed) = line.strip_prefix("Disallow: ") {
                    let disallowed = disallowed.trim();
                    let disallowed = if !disallowed.starts_with('/') {
                        format!("/{disallowed}")
                    } else {
                        disallowed.to_string()
                    };
                    let path = if !path.starts_with('/') {
                        format!("/{path}")
                    } else {
                        path.to_string()
                    };
                    if path.starts_with(&disallowed) {
                        return Err(anyhow!(
                            "URL {url} cannot be fetched due to robots.txt restrictions"
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// PRISM-native research path. Forge's raw `fetch` 403s / binary-rejects
    /// scholarly hosts and is the wrong tool for literature. For those hosts
    /// we *retire* the Forge raw path and route through PRISM's platform
    /// research API (Semantic Scholar / arXiv / PubMed / OpenAlex). PRISM is
    /// Forge with this organ swapped — not a layer on top. Credentials are
    /// pipe-through: env first, else the already-present
    /// `~/.prism/credentials.json`; nothing is persisted here.
    async fn prism_research(&self, url: &Url) -> anyhow::Result<HttpResponse> {
        let mut api_url = std::env::var("MARC27_API_URL")
            .unwrap_or_else(|_| "https://api.marc27.com/api/v1".to_string())
            .trim_end_matches('/')
            .to_string();
        let mut token = std::env::var("MARC27_API_KEY").unwrap_or_default();
        if token.is_empty() {
            if let Ok(home) = std::env::var("HOME") {
                let p = std::path::Path::new(&home).join(".prism/credentials.json");
                if let Ok(txt) = std::fs::read_to_string(&p) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                        token = v
                            .get("access_token")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        if let Some(pu) = v.get("platform_url").and_then(|x| x.as_str()) {
                            let mut base = pu.trim_end_matches('/').to_string();
                            if !base.ends_with("/api/v1") {
                                base.push_str("/api/v1");
                            }
                            api_url = base;
                        }
                    }
                }
            }
        }
        if token.is_empty() {
            return Err(anyhow!(
                "Scholarly source {url} must go through PRISM's platform research API, \
                 but no MARC27 credentials are available. Run `prism login`, then retry."
            ));
        }

        let query = scholarly_query_from_url(url);
        let endpoint = format!("{api_url}/knowledge/research/web-search");
        let resp = self
            .client
            .post(&endpoint)
            .bearer_auth(&token)
            .json(&serde_json::json!({ "query": query, "limit": 8 }))
            .send()
            .await
            .map_err(|e| anyhow!("PRISM research API unreachable ({endpoint}): {e}"))?;
        let code = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "PRISM research API returned {code} for query {query:?}. {}",
                body.chars().take(400).collect::<String>()
            ));
        }
        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow!("PRISM research API returned non-JSON: {e}"))?;

        Ok(HttpResponse {
            content: format_research_results(&query, &data),
            code,
            context: ResponseContext::Parsed,
            content_type: "text/markdown".to_string(),
        })
    }

    async fn fetch_url(&self, url: &Url, force_raw: bool) -> anyhow::Result<HttpResponse> {
        self.check_robots_txt(url).await?;

        let response = self
            .client
            .get(url.as_str())
            .send()
            .await
            .map_err(|e| anyhow!("{}", classify_request_error(url, &e)))?;
        let code = response.status().as_u16();

        if !response.status().is_success() {
            return Err(anyhow!("{}", classify_status_error(url, response.status())));
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        // Detect binary content types before attempting to read as text.
        // The fetch tool is designed for text/HTML content only.
        if is_binary_content_type(&content_type) {
            return Err(anyhow!(
                "URL {} returns binary content (Content-Type: {}). \
                 The fetch tool only handles text content. \
                 Use the shell tool with `curl -fLo <output_file> <url>` to download binary files.",
                url,
                content_type
            ));
        }

        let page_raw = response
            .text()
            .await
            .map_err(|e| anyhow!("Failed to read response content from {url}: {e}"))?;

        // Use floor_char_boundary to avoid panicking on multi-byte UTF-8 chars
        let sniff_end = if page_raw.len() >= 100 {
            // Find the nearest char boundary at or before byte index 100
            let mut end = 100;
            while end > 0 && !page_raw.is_char_boundary(end) {
                end -= 1;
            }
            end
        } else {
            page_raw.len()
        };
        let is_page_html = page_raw
            .get(..sniff_end)
            .map(|s| s.contains("<html"))
            .unwrap_or(false)
            || content_type.contains("text/html")
            || content_type.is_empty();

        if is_page_html && !force_raw {
            let content = html2md::parse_html(&page_raw);
            Ok(HttpResponse {
                content,
                context: ResponseContext::Raw,
                code,
                content_type,
            })
        } else {
            Ok(HttpResponse {
                content: page_raw,
                context: ResponseContext::Parsed,
                code,
                content_type,
            })
        }
    }
}

/// Coerce LLM-emitted URL strings into something the parser will accept.
///
/// Real LLMs in real PRISM sessions emit three recoverable URL shapes
/// that `url::Url::parse` would otherwise reject:
///
/// 1. **Protocol-relative** (`//host/path`) — common when the model
///    cites a URL from a page that used protocol-relative links;
///    the leading `//` means "use the same scheme as the parent."
///    Since we always fetch over HTTPS in this tool, prefix `https:`.
/// 2. **Scheme-less** (`host.tld/path`) — the model wrote a domain
///    without `https://`. Prefix `https://`.
/// 3. **Stray leading dot** (`.host.tld/path`) — observed when the
///    model is mid-typo or has confused itself in a long fetch retry
///    loop. The user's intent is plainly the same domain without the
///    leading dot. Strip a single dot, then fall through to the
///    scheme-less branch.
///
/// We only do this when the input is otherwise unparseable. Inputs
/// with a recognized scheme go through unchanged.
fn normalize_url(input: &str) -> String {
    let trimmed = input.trim();

    // Already parseable as-is → return verbatim.
    if Url::parse(trimmed).is_ok() {
        return trimmed.to_string();
    }

    // Protocol-relative: `//host/path` → `https://host/path`
    if let Some(rest) = trimmed.strip_prefix("//") {
        return format!("https://{rest}");
    }

    // Stray leading dot: `.host.tld/path` → `host.tld/path`. We strip
    // *one* dot only when followed by a domain-shaped char (alnum or
    // a dash). `..foo` or `. foo` keep their original shape so the
    // parser can return a meaningful "this is junk" error to the
    // caller — better to fail loudly than silently fetch garbage.
    let trimmed = if let Some(rest) = trimmed.strip_prefix('.')
        && rest.chars().next().is_some_and(is_domain_lead_char)
    {
        rest
    } else {
        trimmed
    };

    // Scheme-less domain-shaped: `host.tld[/...]` (no scheme, no
    // leading slash). The host segment must:
    //   - contain at least one `.` (otherwise it's a bare word, not a domain),
    //   - start with a domain-lead char (rejects `..foo`, `.foo` already
    //     handled above, and `?#/` already filtered out).
    let host = trimmed.split(['/', '?', '#']).next().unwrap_or("");
    if !trimmed.is_empty()
        && !trimmed.contains("://")
        && host.contains('.')
        && host.chars().next().is_some_and(is_domain_lead_char)
    {
        return format!("https://{trimmed}");
    }

    // Give up — return the original so the caller's parse error is
    // accurate.
    trimmed.to_string()
}

/// Whether `c` could legitimately start a hostname label. Conservative:
/// hostnames RFC-allow letters/digits at the start; we additionally
/// allow underscore (rare but real in dev URLs). Excludes `.` so we
/// never strip the leading dot of `..foo` and pretend it's recoverable.
fn is_domain_lead_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Format a `reqwest::Error` for the agent. The agent reads this string
/// directly, so we phrase the failure mode in a way that nudges it toward
/// the right next move:
///   - DNS / connect failures → the host doesn't exist; don't retry with
///     another guessed URL, fall back to training knowledge.
///   - Timeouts → the server is slow or unreachable; one retry is fine.
///   - Other transport errors → generic message.
fn classify_request_error(url: &Url, e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return format!(
            "Request to {url} timed out. The server may be slow or unreachable. \
             You may retry once; if it fails again, fall back to your training knowledge."
        );
    }
    if e.is_connect() {
        return format!(
            "Could not connect to {url}. The host likely does not exist or has no DNS record. \
             Do not retry with another guessed URL — this often means the URL was hallucinated. \
             Fall back to your training knowledge or ask the user for the correct URL."
        );
    }
    format!("Failed to fetch URL {url}: {e}")
}

/// Derive a research-search query from a URL (filename / last path
/// segment), since the platform research API is query-based.
fn scholarly_query_from_url(url: &Url) -> String {
    let seg = url
        .path_segments()
        .and_then(|s| s.filter(|x| !x.is_empty()).last())
        .unwrap_or("")
        .to_string();
    let mut q = seg.replace("%20", " ");
    for ext in [".pdf", ".html", ".htm", ".abs", ".full", ".epdf"] {
        if let Some(stripped) = q.strip_suffix(ext) {
            q = stripped.to_string();
        }
    }
    q = q.replace(['-', '_', '.', '/', '+'], " ");
    let q = q.split_whitespace().collect::<Vec<_>>().join(" ");
    if q.is_empty() {
        url.host_str().unwrap_or("research").to_string()
    } else {
        q
    }
}

/// Render the platform research payload as citable markdown for the agent.
fn format_research_results(query: &str, data: &serde_json::Value) -> String {
    let mut out = format!(
        "PRISM research search (Semantic Scholar / arXiv / PubMed / OpenAlex) for: {query}\n\
         Forge's raw fetch was retired for this scholarly source and replaced by \
         PRISM's platform research path. These are citable search hits — verify and \
         cite, do not fabricate.\n\n"
    );
    let items = data
        .get("results")
        .or_else(|| data.get("papers"))
        .or_else(|| data.get("items"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if items.is_empty() {
        out.push_str(&format!(
            "No results array in the platform response. Raw payload (truncated):\n{}\n",
            serde_json::to_string_pretty(data)
                .unwrap_or_default()
                .chars()
                .take(2000)
                .collect::<String>()
        ));
        return out;
    }
    for (i, it) in items.iter().take(8).enumerate() {
        let g = |k: &str| it.get(k).and_then(|x| x.as_str()).unwrap_or("");
        let title = if g("title").is_empty() {
            "(no title)"
        } else {
            g("title")
        };
        out.push_str(&format!(
            "{}. {}\n   authors: {}\n   year: {}\n   url/doi: {} {}\n   abstract: {}\n\n",
            i + 1,
            title,
            g("authors"),
            it.get("year").map(|y| y.to_string()).unwrap_or_default(),
            g("url"),
            g("doi"),
            g("abstract").chars().take(600).collect::<String>()
        ));
    }
    out
}

/// Format a non-2xx HTTP status for the agent.
///   - 404 / 410 → URL doesn't exist (or no longer does); don't retry,
///     fall back to training knowledge or ask the user.
///   - 401 / 403 → auth-walled; the agent has no credentials, fall back.
///   - 429 → rate-limited; retry-after with backoff is appropriate.
///   - 5xx → server-side; one retry is fine.
fn classify_status_error(url: &Url, status: reqwest::StatusCode) -> String {
    let code = status.as_u16();
    match code {
        404 | 410 => format!(
            "URL {url} returned {code} (not found). The page does not exist. \
             Do not retry with another guessed URL — fall back to your training knowledge \
             or ask the user for the correct URL."
        ),
        401 | 403 => format!(
            "URL {url} returned {code} (auth required). The fetch tool has no credentials \
             for this resource. Fall back to your training knowledge or ask the user for \
             a public URL."
        ),
        429 => format!(
            "URL {url} returned 429 (rate-limited). Wait briefly before retrying, or use a \
             different source."
        ),
        500..=599 => format!(
            "URL {url} returned {code} (server error). One retry is appropriate; if it fails \
             again, fall back to another source or your training knowledge."
        ),
        _ => format!("Failed to fetch {url} - status code {status}"),
    }
}

#[async_trait::async_trait]
impl NetFetchService for ForgeFetch {
    async fn fetch(&self, url: String, raw: Option<bool>) -> anyhow::Result<HttpResponse> {
        let normalized = normalize_url(&url);
        let parsed = Url::parse(&normalized).with_context(|| {
            format!(
                "URL `{url}` is not parseable. This usually means the URL is malformed or \
                 hallucinated. Do not retry with another guessed URL — fall back to your \
                 training knowledge or ask the user for the correct URL."
            )
        })?;

        // General rule — NO host allowlist. Try Forge's raw fetch; if it
        // fails the way Forge structurally cannot recover (auth wall,
        // binary/PDF, anti-bot block) for ANY source, retire that dead-end
        // and fall back to PRISM's native research path. Forge stays the
        // path for everything it can actually deliver.
        match self.fetch_url(&parsed, raw.unwrap_or(false)).await {
            Ok(resp) => Ok(resp),
            Err(forge_err) => match self.prism_research(&parsed).await {
                Ok(resp) => Ok(resp),
                Err(prism_err) => Err(anyhow!(
                    "Forge fetch failed: {forge_err}\n\nPRISM research \
                     fallback also failed: {prism_err}"
                )),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── normalize_url ────────────────────────────────────────────────

    #[test]
    fn normalize_url_passes_through_https() {
        assert_eq!(
            normalize_url("https://example.com/foo"),
            "https://example.com/foo"
        );
    }

    #[test]
    fn normalize_url_passes_through_http() {
        assert_eq!(
            normalize_url("http://example.com/foo"),
            "http://example.com/foo"
        );
    }

    #[test]
    fn normalize_url_fixes_protocol_relative() {
        // Real example from a PRISM TUI session: LLM emitted this
        // citing an azom.com page; the original parser bailed with
        // "relative URL without a base". This is the bug fix.
        assert_eq!(
            normalize_url("//www.azom.com/spx?ArticleID=1"),
            "https://www.azom.com/spx?ArticleID=1"
        );
    }

    #[test]
    fn normalize_url_fixes_scheme_less_domain() {
        assert_eq!(
            normalize_url("www.example.com/path"),
            "https://www.example.com/path"
        );
        assert_eq!(normalize_url("example.com"), "https://example.com");
    }

    #[test]
    fn normalize_url_trims_whitespace() {
        assert_eq!(
            normalize_url("  https://example.com  "),
            "https://example.com"
        );
    }

    #[test]
    fn normalize_url_leaves_unrecoverable_inputs_alone() {
        // Path-only, fragment-only, and query-only inputs aren't
        // valid stand-alone URLs and we don't try to recover them.
        // The downstream parser will return a meaningful error.
        assert_eq!(normalize_url("/just/a/path"), "/just/a/path");
        assert_eq!(normalize_url("#fragment"), "#fragment");
        assert_eq!(normalize_url("?query=1"), "?query=1");
    }

    #[test]
    fn normalize_url_strips_stray_leading_dot() {
        // Real example from a PRISM TUI session (Bug #13): the LLM
        // emitted `.wikipedia.org/Ti-6Al-4V` mid-retry. Pre-fix the
        // existing scheme-less branch would happily produce
        // `https://.wikipedia.org/...`, which is also unparseable.
        assert_eq!(
            normalize_url(".wikipedia.org/Ti-6Al-4V"),
            "https://wikipedia.org/Ti-6Al-4V"
        );
        assert_eq!(normalize_url(".example.com"), "https://example.com");
        assert_eq!(
            normalize_url(".sub.example.com/page"),
            "https://sub.example.com/page"
        );
    }

    #[test]
    fn normalize_url_does_not_strip_double_dot_or_space_after_dot() {
        // Double-dot: probably truly broken model output. Don't
        // pretend we recovered it — let the parser surface the error
        // so the calling agent can fall back to training knowledge.
        assert_eq!(normalize_url("..wikipedia.org"), "..wikipedia.org");
        // Dot followed by space: definitely not a hostname start.
        assert_eq!(normalize_url(". wikipedia.org"), ". wikipedia.org");
    }

    #[test]
    fn normalize_url_does_not_prefix_bare_word() {
        // "hello" has no dot, so we don't pretend it's a domain.
        assert_eq!(normalize_url("hello"), "hello");
    }

    // ── classify_status_error ────────────────────────────────────────
    //
    // The agent reads these strings directly. We test the substrings the
    // agent should see — mainly the "fall back to training knowledge"
    // nudge, so a refactor doesn't accidentally drop it.

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn classify_404_tells_agent_to_stop_retrying() {
        let msg = classify_status_error(
            &url("https://example.com/nope"),
            reqwest::StatusCode::NOT_FOUND,
        );
        assert!(msg.contains("404"));
        assert!(
            msg.contains("Do not retry") && msg.contains("training knowledge"),
            "404 message should nudge fallback, got: {msg}"
        );
    }

    #[test]
    fn classify_410_treats_like_404() {
        let msg =
            classify_status_error(&url("https://example.com/gone"), reqwest::StatusCode::GONE);
        assert!(msg.contains("410"));
        assert!(msg.contains("training knowledge"));
    }

    #[test]
    fn classify_403_acknowledges_auth_wall() {
        let msg = classify_status_error(
            &url("https://example.com/x"),
            reqwest::StatusCode::FORBIDDEN,
        );
        assert!(msg.contains("403"));
        assert!(msg.contains("auth"));
        assert!(msg.contains("training knowledge"));
    }

    #[test]
    fn classify_429_suggests_backoff_not_fallback() {
        let msg = classify_status_error(
            &url("https://example.com/x"),
            reqwest::StatusCode::TOO_MANY_REQUESTS,
        );
        assert!(msg.contains("429"));
        assert!(msg.contains("rate-limited"));
        // Rate limit ≠ hallucination; we don't ask the agent to bail to
        // training knowledge here — backoff is the right answer.
        assert!(
            !msg.contains("training knowledge"),
            "429 shouldn't trigger training-knowledge fallback: {msg}"
        );
    }

    #[test]
    fn classify_500_allows_one_retry() {
        let msg = classify_status_error(
            &url("https://example.com/x"),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        );
        assert!(msg.contains("500"));
        assert!(msg.contains("retry"));
    }

    #[test]
    fn classify_300_falls_through_to_generic_message() {
        // Multiple choices: not a hallucination signal, not a rate
        // limit. The classifier shouldn't impose a behavioural nudge.
        let msg = classify_status_error(
            &url("https://example.com/x"),
            reqwest::StatusCode::MULTIPLE_CHOICES,
        );
        assert!(msg.contains("300"));
        assert!(!msg.contains("training knowledge"));
    }

    #[test]
    fn test_is_binary_content_type_text_types_are_not_binary() {
        assert!(!is_binary_content_type("text/html"));
        assert!(!is_binary_content_type("text/plain"));
        assert!(!is_binary_content_type("text/css"));
        assert!(!is_binary_content_type("application/json"));
        assert!(!is_binary_content_type("application/xml"));
        assert!(!is_binary_content_type("application/javascript"));
        assert!(!is_binary_content_type("application/yaml"));
        assert!(!is_binary_content_type("image/svg+xml"));
        assert!(!is_binary_content_type("text/csv"));
        assert!(!is_binary_content_type("text/markdown"));
        assert!(!is_binary_content_type("")); // empty = unknown, allow
    }

    #[test]
    fn test_is_binary_content_type_binary_types_detected() {
        assert!(is_binary_content_type("application/gzip"));
        assert!(is_binary_content_type("application/x-gzip"));
        assert!(is_binary_content_type("application/octet-stream"));
        assert!(is_binary_content_type("application/zip"));
        assert!(is_binary_content_type("application/x-tar"));
        assert!(is_binary_content_type("application/pdf"));
        assert!(is_binary_content_type("image/png"));
        assert!(is_binary_content_type("image/jpeg"));
        assert!(is_binary_content_type("audio/mpeg"));
        assert!(is_binary_content_type("video/mp4"));
    }

    #[test]
    fn test_is_binary_content_type_case_insensitive() {
        assert!(!is_binary_content_type("Application/JSON"));
        assert!(!is_binary_content_type("TEXT/HTML; charset=utf-8"));
        assert!(is_binary_content_type("Application/Gzip"));
    }

    // ── PRISM-native research query derivation ───────────────────────

    #[test]
    fn scholarly_query_derived_from_url_filename() {
        let q = scholarly_query_from_url(
            &Url::parse(
                "https://ntrs.nasa.gov/api/citations/20210014640/downloads/\
                 GRCop-42%20final%20Manuscript%20for%20NASA%20TM.pdf",
            )
            .unwrap(),
        );
        // %20 decoded, .pdf stripped, separators normalised to spaces.
        assert_eq!(q, "GRCop 42 final Manuscript for NASA TM");
    }

    #[test]
    fn scholarly_query_falls_back_to_host_when_no_path() {
        let q = scholarly_query_from_url(&Url::parse("https://arxiv.org/").unwrap());
        assert_eq!(q, "arxiv.org");
    }
}
