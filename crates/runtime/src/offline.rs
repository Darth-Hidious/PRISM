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

    if host == "localhost" || host == "localhost.localdomain" {
        return true;
    }

    // Everything else must be a real IP LITERAL that is itself loopback.
    //
    // This was `host.starts_with("127.")`, which matched any DOMAIN beginning
    // with those characters. `http://127.evil.example/` and
    // `http://127.0.0.1.attacker.example/` both passed as "loopback" while
    // resolving wherever their owner points them — so an attacker-controlled
    // name defeated both `check_url`'s offline policy and every caller that
    // treats loopback as "safe, stays on this machine".
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
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

    /// A hostname is not an address. `starts_with("127.")` matched any DOMAIN
    /// beginning with those characters, so an attacker-registered name was
    /// treated as loopback — defeating the offline policy and every caller
    /// that reads "loopback" as "never leaves this machine".
    #[test]
    fn a_domain_that_merely_looks_loopback_is_not_loopback() {
        for hostile in [
            "http://127.evil.example/",
            "http://127.0.0.1.attacker.example/",
            "https://localhost.attacker.example/",
            "http://127.0.0.1@attacker.example/",
            "http://attacker.example#127.0.0.1",
            "http://attacker.example/?x=127.0.0.1",
        ] {
            assert!(
                !is_loopback_url(hostile),
                "{hostile} is attacker-controlled, not loopback"
            );
        }
    }

    /// The real literals must keep working, including the whole 127/8 block
    /// and IPv6 — the fix must not over-tighten.
    #[test]
    fn real_loopback_literals_still_pass() {
        for ok in [
            "http://127.0.0.1:7327",
            "http://127.0.0.1",
            "http://127.42.0.9:8080/v1",
            "http://localhost:7327",
            "http://localhost.localdomain/x",
            "http://[::1]:8080/v1",
            "HTTP://LOCALHOST/",
        ] {
            assert!(is_loopback_url(ok), "{ok} is genuinely loopback");
        }
        // Not loopback, and must not become so.
        for remote in [
            "http://10.0.0.4:8080/v1",
            "http://0.0.0.0/",
            "https://api.example.invalid/v1",
        ] {
            assert!(!is_loopback_url(remote), "{remote}");
        }
    }

    /// Deliberately conservative, recorded so it is a decision rather than a
    /// surprise: Rust's `IpAddr` parser rejects the legacy abbreviated
    /// inet_aton forms that curl and browsers accept, so `127.1` and
    /// `0x7f000001` read as NON-loopback. That errs toward withholding a
    /// credential and refusing a request under offline mode — the safe
    /// direction. Widening it means hand-rolling inet_aton, which is how the
    /// original `starts_with("127.")` shortcut got written in the first place.
    #[test]
    fn abbreviated_ipv4_forms_are_treated_as_non_loopback() {
        for abbreviated in ["http://127.1", "http://0x7f000001/", "http://2130706433/"] {
            assert!(
                !is_loopback_url(abbreviated),
                "{abbreviated} — conservative by design; see the doc comment"
            );
        }
    }

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
