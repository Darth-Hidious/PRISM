//! Disk-backed response cache.
//!
//! Keyed by sha256(source | url); stores the raw body plus fetch metadata as
//! one JSON file. This is what makes sweeps resumable and repeat runs fast:
//! nothing already fetched is fetched again unless the TTL expired or the
//! caller opts out.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const DEFAULT_TTL_SECS: u64 = 24 * 60 * 60; // 24 h

#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    source: String,
    url: String,
    fetched_at_unix: u64,
    /// Base64 of the raw body, kept verbatim so re-parsing is faithful.
    body_b64: String,
}

#[derive(Debug, Clone)]
pub struct DiskCache {
    dir: PathBuf,
    ttl: Duration,
}

impl DiskCache {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            ttl: Duration::from_secs(DEFAULT_TTL_SECS),
        }
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    fn key_path(&self, source: &str, url: &str) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(source.as_bytes());
        hasher.update(b"|");
        hasher.update(url.as_bytes());
        let hex: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        self.dir.join(format!("{hex}.json"))
    }

    /// Return the cached body when present AND fresh, else None.
    pub fn get(&self, source: &str, url: &str) -> Option<Vec<u8>> {
        let path = self.key_path(source, url);
        let raw = std::fs::read_to_string(&path).ok()?;
        let entry: CacheEntry = serde_json::from_str(&raw).ok()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if now.saturating_sub(entry.fetched_at_unix) > self.ttl.as_secs() {
            return None;
        }
        use base64_decode::decode;
        decode(&entry.body_b64).ok()
    }

    /// Store a body. A write failure is not fatal to the caller — the fetch
    /// still succeeded; caching is best-effort.
    pub fn put(&self, source: &str, url: &str, body: &[u8]) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let entry = CacheEntry {
            source: source.to_string(),
            url: url.to_string(),
            fetched_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            body_b64: base64_decode::encode(body),
        };
        let path = self.key_path(source, url);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&entry)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

/// Minimal base64 (standard alphabet, padded). Implemented inline to avoid a
/// dependency for two functions; the alphabet is the RFC 4648 standard one.
mod base64_decode {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(input: &[u8]) -> String {
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        for chunk in input.chunks(3) {
            let b = [
                chunk[0],
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            out.push(ALPHABET[(b[0] >> 2) as usize] as char);
            out.push(ALPHABET[(((b[0] & 0x03) << 4) | (b[1] >> 4)) as usize] as char);
            if chunk.len() > 1 {
                out.push(ALPHABET[(((b[1] & 0x0f) << 2) | (b[2] >> 6)) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(ALPHABET[(b[2] & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }

    pub fn decode(input: &str) -> Result<Vec<u8>, ()> {
        let bytes: Vec<u8> = input
            .bytes()
            .filter(|b| *b != b'\n' && *b != b'\r')
            .collect();
        if !bytes.len().is_multiple_of(4) {
            return Err(());
        }
        let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
        for chunk in bytes.chunks(4) {
            let vals: Vec<u8> = chunk
                .iter()
                .map(|c| {
                    if *c == b'=' {
                        Ok(0)
                    } else {
                        ALPHABET
                            .iter()
                            .position(|a| a == c)
                            .map(|p| p as u8)
                            .ok_or(())
                    }
                })
                .collect::<Result<Vec<u8>, ()>>()?;
            out.push((vals[0] << 2) | (vals[1] >> 4));
            if chunk[2] != b'=' {
                out.push((vals[1] << 4) | (vals[2] >> 2));
            }
            if chunk[3] != b'=' {
                out.push((vals[2] << 6) | vals[3]);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        for input in [
            &b""[..],
            &b"f"[..],
            &b"fo"[..],
            &b"foo"[..],
            &b"foob"[..],
            &[0u8, 255, 7, 33][..],
        ] {
            assert_eq!(
                base64_decode::decode(&base64_decode::encode(input)).unwrap(),
                input.to_vec()
            );
        }
    }

    #[test]
    fn cache_roundtrip_and_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf());
        assert!(cache.get("arxiv", "http://x").is_none());
        cache.put("arxiv", "http://x", b"hello body").unwrap();
        assert_eq!(cache.get("arxiv", "http://x").unwrap(), b"hello body");
        // Different URL does not collide.
        assert!(cache.get("arxiv", "http://y").is_none());
        // Expired entry is refused.
        let expired = cache.clone().with_ttl(Duration::from_secs(0));
        std::thread::sleep(Duration::from_millis(1100));
        assert!(expired.get("arxiv", "http://x").is_none());
    }
}
