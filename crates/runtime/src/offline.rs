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
    // Parse properly rather than pattern-match the string.
    //
    // This was hand-rolled: split on "://", then on `['/','?','#']`, then
    // `rsplit_once('@')`, then `host.starts_with("127.")`. That last clause
    // matched any DOMAIN beginning with those characters, so
    // `http://127.evil.example/` — a name anyone can register and point
    // anywhere — read as loopback and defeated both the offline policy and the
    // platform-credential guard that trusts it.
    //
    // `url::Host` distinguishes Domain from Ipv4/Ipv6 by construction, so a
    // hostname can never be mistaken for an address. It also canonicalises the
    // legacy inet_aton forms (`127.1`, `2130706433`, `0x7f000001`), which the
    // interim `IpAddr::from_str` fix had to treat as non-loopback.
    //
    // A near-identical, correct implementation already existed one crate away
    // in `prism-cli`'s `local_llm.rs` while the weaker one guarded the policy.
    // That copy now calls this; there is one implementation.
    match url::Url::parse(raw_url.trim()) {
        Ok(parsed) => match parsed.host() {
            Some(url::Host::Domain(domain)) => {
                domain.eq_ignore_ascii_case("localhost")
                    || domain.eq_ignore_ascii_case("localhost.localdomain")
            }
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        },
        Err(_) => false,
    }
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

    /// The legacy inet_aton spellings ARE loopback, and are now recognised.
    ///
    /// This test previously asserted the opposite. That was not a policy
    /// choice, it was a limitation of the interim `IpAddr::from_str` fix,
    /// which rejects the abbreviated forms that curl and browsers accept —
    /// so `127.1` read as a remote host. Parsing with `url` canonicalises
    /// them properly, and the limitation is gone rather than papered over.
    #[test]
    fn abbreviated_ipv4_forms_are_recognised_as_loopback() {
        for abbreviated in ["http://127.1", "http://0x7f000001/", "http://2130706433/"] {
            assert!(
                is_loopback_url(abbreviated),
                "{abbreviated} canonicalises to 127.0.0.1"
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
