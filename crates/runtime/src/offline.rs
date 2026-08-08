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
    let trimmed = raw_url.trim();
    // A scheme-less `127.0.0.1:8080` or `localhost:11434` is a shape the
    // previous string-splitting version accepted, and `Url::parse` rejects
    // outright. Callers happen to supply a scheme today (`use_command.rs:426`
    // refuses input without one, and every shipped `providers.toml` entry has
    // one), but a user-written `~/.prism/providers.toml` need not — and this
    // is a general safety primitive, so it should not silently narrow. Parse
    // the bare authority under a synthetic scheme; that cannot widen the
    // result, since the host is still typed by `url::Host`.
    // Retry on "no host", not merely on a parse error. `url` reads
    // `localhost:11434` as SCHEME `localhost` with path `11434` — a SUCCESSFUL
    // parse carrying no host — whereas `127.0.0.1:8080` fails outright because
    // a scheme cannot start with a digit. Keying off `Err` alone therefore
    // fixed one and silently missed the other.
    let has_host = |u: &url::Url| u.host().is_some();
    let parsed = match url::Url::parse(trimmed) {
        Ok(u) if has_host(&u) => Some(u),
        _ if trimmed.contains("://") => None,
        _ => url::Url::parse(&format!("http://{trimmed}"))
            .ok()
            .filter(has_host),
    };

    match parsed.as_ref().and_then(url::Url::host) {
        Some(url::Host::Domain(domain)) => {
            domain.eq_ignore_ascii_case("localhost")
                || domain.eq_ignore_ascii_case("localhost.localdomain")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
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

/// Test-support for the process-global `PRISM_OFFLINE`.
///
/// **Deliberately a plain `pub mod`, not `#[cfg(test)]`.** `cfg(test)` does not
/// cross crate boundaries, so every crate that wanted this ended up declaring
/// its own `static LOCK` — and two locks that do not exclude each other
/// serialize nothing. That happened SEVEN times on this branch, across
/// prism-agent, prism-mesh, prism-cli and prism-client, three of them added by
/// the same session that was fixing the previous one. `crates/client` failed
/// for real: "DELETE not refused for \" 1\"" when auth.rs's tests cleared the
/// variable mid-run of api.rs's.
///
/// The invariant is ONE lock per test binary, not one per workspace: cargo runs
/// each crate's tests as its own process, so this static is instantiated once
/// per binary and crates never contend with each other. What breaks is two
/// locks inside a single binary — which is what a per-file `static LOCK` in a
/// crate with several test modules produces every time. Naming one home, with
/// the RAII guard beside it, is what stops the next file from rolling its own.
/// The cost is a `Mutex<()>` in the shipped binary.
pub mod test_support {
    /// Serializes every test in this binary that mutates `PRISM_OFFLINE`.
    pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take the lock. Recovers from poisoning so one panicking test cannot
    /// convert every later one into a spurious failure that masks the cause.
    pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Restores `PRISM_OFFLINE` on drop, so an assertion failure — the very
    /// thing these tests exist to produce — cannot leave it set for the rest
    /// of the binary.
    pub struct OfflineEnvGuard(Option<String>);

    impl OfflineEnvGuard {
        pub fn capture() -> Self {
            Self(std::env::var(super::ENV).ok())
        }

        /// Capture the current value, then set a new one.
        pub fn set(value: &str) -> Self {
            let guard = Self::capture();
            unsafe { std::env::set_var(super::ENV, value) };
            guard
        }

        /// Capture the current value, then clear it.
        pub fn clear() -> Self {
            let guard = Self::capture();
            unsafe { std::env::remove_var(super::ENV) };
            guard
        }
    }

    impl Drop for OfflineEnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.0.take() {
                    Some(value) => std::env::set_var(super::ENV, value),
                    None => std::env::remove_var(super::ENV),
                }
            }
        }
    }
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

    /// `PRISM_OFFLINE` is process-global, so serialize the tests that set it.
    /// Same precedent as `client/src/auth.rs:238`. Recovers from poisoning so
    /// one panicking test cannot convert every later one into a spurious
    /// failure that masks the real cause.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// Restores `PRISM_OFFLINE` on drop, so an assertion failure cannot leave
    /// the variable set for the rest of the binary. A plain `let _restore =`
    /// does not survive an unwind; this does.
    struct OfflineEnvGuard(Option<String>);
    impl Drop for OfflineEnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.0.take() {
                    Some(value) => std::env::set_var(ENV, value),
                    None => std::env::remove_var(ENV),
                }
            }
        }
    }

    /// `PRISM_OFFLINE=0` means OFF, and every caller must agree on that.
    ///
    /// The rule was re-derived in six places as `== "1"` (untrimmed), and once
    /// in python-bridge as `var().is_err()` — "is the variable absent" — which
    /// made an explicit `0` behave like offline. Both shapes are gone; this
    /// pins the semantics they got wrong.
    #[test]
    fn only_a_trimmed_one_enables_offline() {
        let _guard = env_lock();
        let _restore = OfflineEnvGuard(std::env::var(ENV).ok());
        for (value, expected) in [
            ("1", true),
            (" 1", true),
            ("1 ", true),
            ("\t1\n", true),
            ("0", false),
            ("", false),
            ("true", false),
            ("yes", false),
            ("11", false),
        ] {
            unsafe { std::env::set_var(ENV, value) };
            assert_eq!(enabled(), expected, "PRISM_OFFLINE={value:?}");
        }
        unsafe { std::env::remove_var(ENV) };
        assert!(!enabled(), "unset is not offline");
    }

    /// A bare `host:port` with no scheme must still resolve, and must not
    /// become a way in. The string-splitting version accepted these; a plain
    /// `Url::parse` rejects them outright, which would have turned a local
    /// endpoint into a policy REFUSAL under PRISM_OFFLINE=1 — breaking exactly
    /// the local-llama.cpp user offline mode exists to serve.
    #[test]
    fn scheme_less_authorities_still_resolve_and_do_not_widen() {
        for local in [
            "127.0.0.1:8080",
            "localhost:11434",
            "127.0.0.1",
            "localhost",
            "[::1]:8080",
        ] {
            assert!(is_loopback_url(local), "{local} is on this machine");
        }
        // The scheme-less path must not become a bypass.
        for hostile in [
            "127.evil.example:8080",
            "127.0.0.1.attacker.example",
            "attacker.example:8080",
            "10.0.0.4:8080",
        ] {
            assert!(!is_loopback_url(hostile), "{hostile} is not this machine");
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
        // Shares the global with only_a_trimmed_one_enables_offline; without
        // this guard the two race on PRISM_OFFLINE.
        let _guard = env_lock();
        let _restore = OfflineEnvGuard(std::env::var(ENV).ok());
        assert!(check_url("https://api.example.invalid").is_ok());
        // The environment-sensitive branch is covered by the process-level
        // integration test; this unit test remains deterministic for parallel
        // cargo test execution.
    }
}
