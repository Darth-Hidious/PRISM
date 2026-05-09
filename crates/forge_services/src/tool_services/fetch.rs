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
        Self {
            client: Client::new(),
        }
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

    async fn fetch_url(&self, url: &Url, force_raw: bool) -> anyhow::Result<HttpResponse> {
        self.check_robots_txt(url).await?;

        let response = self
            .client
            .get(url.as_str())
            .send()
            .await
            .map_err(|e| anyhow!("Failed to fetch URL {url}: {e}"))?;
        let code = response.status().as_u16();

        if !response.status().is_success() {
            return Err(anyhow!(
                "Failed to fetch {} - status code {}",
                url,
                response.status()
            ));
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
        && host
            .chars()
            .next()
            .is_some_and(is_domain_lead_char)
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

#[async_trait::async_trait]
impl NetFetchService for ForgeFetch {
    async fn fetch(&self, url: String, raw: Option<bool>) -> anyhow::Result<HttpResponse> {
        let normalized = normalize_url(&url);
        let parsed =
            Url::parse(&normalized).with_context(|| format!("Failed to parse URL: {url}"))?;

        self.fetch_url(&parsed, raw.unwrap_or(false)).await
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
}
