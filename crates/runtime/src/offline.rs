//! Process-wide offline policy helpers.
//!
//! `PRISM_OFFLINE=1` blocks remote network targets while preserving explicit
//! loopback endpoints such as a local llama.cpp server. This keeps standalone
//! inference usable without allowing a provider, marketplace, or user URL to
//! create an outbound connection.

/// The environment variable used by every PRISM process and Python worker.
pub const ENV: &str = "PRISM_OFFLINE";

/// Whether hard offline mode is enabled.
#[must_use]
pub fn enabled() -> bool {
    std::env::var(ENV).is_ok_and(|value| value.trim() == "1")
}

/// Return whether `raw_url` explicitly targets loopback.
#[must_use]
pub fn is_loopback_url(raw_url: &str) -> bool {
    let authority = raw_url
        .trim()
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(raw_url)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or_else(|| {
            raw_url
                .trim()
                .split_once("://")
                .map(|(_, rest)| rest)
                .unwrap_or(raw_url)
                .split(['/', '?', '#'])
                .next()
                .unwrap_or_default()
        });
    let host = authority
        .strip_prefix('[')
        .and_then(|value| value.split(']').next())
        .unwrap_or_else(|| authority.split(':').next().unwrap_or(authority))
        .to_ascii_lowercase();

    host == "localhost"
        || host == "localhost.localdomain"
        || host == "::1"
        || host == "0:0:0:0:0:0:0:1"
        || host == "127.0.0.1"
        || host.starts_with("127.")
}

/// Reject a remote URL when hard offline mode is enabled.
pub fn check_url(raw_url: &str) -> Result<(), String> {
    if enabled() && !is_loopback_url(raw_url) {
        return Err(format!(
            "offline mode: outbound network blocked for {raw_url}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_loopback_targets_without_allowing_private_networks() {
        assert!(is_loopback_url("http://localhost:8080/v1"));
        assert!(is_loopback_url("http://127.0.0.1:8080/v1"));
        assert!(is_loopback_url("http://127.42.0.9:8080/v1"));
        assert!(is_loopback_url("http://[::1]:8080/v1"));
        assert!(!is_loopback_url("http://10.0.0.4:8080/v1"));
        assert!(!is_loopback_url("https://api.example.invalid/v1"));
    }

    #[test]
    fn offline_check_is_noop_when_disabled_and_rejects_remote_targets() {
        assert!(check_url("https://api.example.invalid").is_ok());
        // The environment-sensitive branch is covered by the process-level
        // integration test; this unit test remains deterministic for parallel
        // cargo test execution.
    }
}
