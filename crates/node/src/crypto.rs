//! E2EE primitives for node-to-node communication.
//!
//! Protocol: X25519 key agreement + ChaCha20-Poly1305 AEAD.
//! Same cryptographic pattern as Signal and WireGuard.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use rand::RngCore;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

const NONCE_LEN: usize = 12;
const KEY_FILE_NAME: &str = "node_key";
const SIGNING_KEY_FILE_NAME: &str = "node_signing_key";

/// Load an existing keypair from disk, or generate a new one.
///
/// Private key is stored as raw 32 bytes at `state_dir/node_key`
/// with 0600 permissions. It never leaves the machine.
pub fn load_or_generate_key(state_dir: &Path) -> Result<(StaticSecret, PublicKey)> {
    let key_path = key_file_path(state_dir);

    if key_path.exists() {
        load_key(&key_path)
    } else {
        let (secret, public) = generate_keypair();
        save_key(&key_path, &secret)?;
        tracing::info!(
            public_key = %encode_public_key(&public),
            "generated new node keypair"
        );
        Ok((secret, public))
    }
}

/// Load an existing Ed25519 signing keypair from disk, or generate a new one.
pub fn load_or_generate_signing_key(state_dir: &Path) -> Result<(SigningKey, VerifyingKey)> {
    let key_path = signing_key_file_path(state_dir);

    if key_path.exists() {
        load_signing_key(&key_path)
    } else {
        let signing = generate_signing_keypair();
        save_signing_key(&key_path, &signing)?;
        let verifying = signing.verifying_key();
        tracing::info!(
            signing_public_key = %encode_signing_public_key(&verifying),
            "generated new node signing keypair"
        );
        Ok((signing, verifying))
    }
}

/// Generate a fresh X25519 keypair.
fn generate_keypair() -> (StaticSecret, PublicKey) {
    let secret = StaticSecret::random_from_rng(OsRng);
    let public = PublicKey::from(&secret);
    (secret, public)
}

fn generate_signing_keypair() -> SigningKey {
    SigningKey::generate(&mut OsRng)
}

/// Load a private key from a 32-byte file.
fn load_key(path: &Path) -> Result<(StaticSecret, PublicKey)> {
    let mut bytes = std::fs::read(path)
        .with_context(|| format!("failed to read node key from {}", path.display()))?;

    if bytes.len() != 32 {
        bytes.zeroize();
        bail!(
            "node key at {} has invalid length {} (expected 32)",
            path.display(),
            bytes.len()
        );
    }

    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&bytes);
    bytes.zeroize();

    let secret = StaticSecret::from(key_bytes);
    key_bytes.zeroize();

    let public = PublicKey::from(&secret);
    Ok((secret, public))
}

fn load_signing_key(path: &Path) -> Result<(SigningKey, VerifyingKey)> {
    let mut bytes = std::fs::read(path)
        .with_context(|| format!("failed to read node signing key from {}", path.display()))?;

    if bytes.len() != 32 {
        bytes.zeroize();
        bail!(
            "node signing key at {} has invalid length {} (expected 32)",
            path.display(),
            bytes.len()
        );
    }

    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&bytes);
    bytes.zeroize();

    let signing = SigningKey::from_bytes(&key_bytes);
    key_bytes.zeroize();

    let verifying = signing.verifying_key();
    Ok((signing, verifying))
}

/// Save a private key as raw 32 bytes with restricted permissions.
fn save_key(path: &Path, secret: &StaticSecret) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    // Serialize the secret key bytes
    let bytes = secret.to_bytes();
    std::fs::write(path, bytes)
        .with_context(|| format!("failed to write node key to {}", path.display()))?;

    // Set 0600 permissions (owner read/write only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }

    Ok(())
}

fn save_signing_key(path: &Path, signing: &SigningKey) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    std::fs::write(path, signing.to_bytes())
        .with_context(|| format!("failed to write node signing key to {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }

    Ok(())
}

/// Rotate the keypair: generate new, overwrite old, zeroize.
pub fn rotate_key(state_dir: &Path) -> Result<PublicKey> {
    let key_path = key_file_path(state_dir);

    let (secret, public) = generate_keypair();
    save_key(&key_path, &secret)?;

    tracing::info!(
        public_key = %encode_public_key(&public),
        "rotated node keypair"
    );

    Ok(public)
}

pub fn rotate_signing_key(state_dir: &Path) -> Result<VerifyingKey> {
    let key_path = signing_key_file_path(state_dir);

    let signing = generate_signing_keypair();
    let verifying = signing.verifying_key();
    save_signing_key(&key_path, &signing)?;

    tracing::info!(
        signing_public_key = %encode_signing_public_key(&verifying),
        "rotated node signing keypair"
    );

    Ok(verifying)
}

/// Compute a shared secret from our private key and their public key.
pub fn compute_shared_secret(our_secret: &StaticSecret, their_public: &PublicKey) -> [u8; 32] {
    our_secret.diffie_hellman(their_public).to_bytes()
}

/// Encrypt plaintext using ChaCha20-Poly1305 with a shared secret.
///
/// Output format: `nonce(12) || ciphertext || tag(16)`
pub fn encrypt(shared_secret: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new_from_slice(shared_secret)
        .map_err(|e| anyhow::anyhow!("cipher init failed: {e}"))?;

    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?;

    // Prepend nonce to ciphertext
    let mut output = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    output.extend_from_slice(&nonce_bytes);
    output.extend_from_slice(&ciphertext);

    Ok(output)
}

/// Decrypt data encrypted with `encrypt()`.
///
/// Input format: `nonce(12) || ciphertext || tag(16)`
pub fn decrypt(shared_secret: &[u8; 32], data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < NONCE_LEN + 16 {
        bail!(
            "encrypted data too short ({} bytes, minimum {})",
            data.len(),
            NONCE_LEN + 16
        );
    }

    let (nonce_bytes, ciphertext) = data.split_at(NONCE_LEN);
    let nonce = Nonce::from_slice(nonce_bytes);

    let cipher = ChaCha20Poly1305::new_from_slice(shared_secret)
        .map_err(|e| anyhow::anyhow!("cipher init failed: {e}"))?;

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("decryption failed (wrong key or corrupted data): {e}"))
}

/// Encode a public key as base64 for transmission.
pub fn encode_public_key(key: &PublicKey) -> String {
    base64::engine::general_purpose::STANDARD.encode(key.as_bytes())
}

/// Decode a base64-encoded public key.
pub fn decode_public_key(encoded: &str) -> Result<PublicKey> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("invalid base64 in public key")?;

    if bytes.len() != 32 {
        bail!(
            "public key has invalid length {} (expected 32)",
            bytes.len()
        );
    }

    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&bytes);
    Ok(PublicKey::from(key_bytes))
}

pub fn encode_signing_public_key(key: &VerifyingKey) -> String {
    base64::engine::general_purpose::STANDARD.encode(key.as_bytes())
}

pub fn decode_signing_public_key(encoded: &str) -> Result<VerifyingKey> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("invalid base64 in signing public key")?;

    if bytes.len() != 32 {
        bail!(
            "signing public key has invalid length {} (expected 32)",
            bytes.len()
        );
    }

    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&bytes);
    VerifyingKey::from_bytes(&key_bytes).context("invalid ed25519 public key")
}

pub fn sign_bytes(signing: &SigningKey, data: &[u8]) -> String {
    let signature: Signature = signing.sign(data);
    base64::engine::general_purpose::STANDARD.encode(signature.to_bytes())
}

pub fn verify_signature(public: &VerifyingKey, data: &[u8], signature_b64: &str) -> Result<()> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(signature_b64)
        .context("invalid base64 in signature")?;

    if bytes.len() != 64 {
        bail!("signature has invalid length {} (expected 64)", bytes.len());
    }

    let mut sig_bytes = [0u8; 64];
    sig_bytes.copy_from_slice(&bytes);
    let signature = Signature::from_bytes(&sig_bytes);
    public
        .verify(data, &signature)
        .context("signature verification failed")
}

fn key_file_path(state_dir: &Path) -> PathBuf {
    state_dir.join(KEY_FILE_NAME)
}

fn signing_key_file_path(state_dir: &Path) -> PathBuf {
    state_dir.join(SIGNING_KEY_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn generate_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let (secret1, public1) = load_or_generate_key(tmp.path()).unwrap();
        let (secret2, public2) = load_or_generate_key(tmp.path()).unwrap();

        // Same key loaded from disk
        assert_eq!(public1.as_bytes(), public2.as_bytes());
        assert_eq!(secret1.to_bytes(), secret2.to_bytes());
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let alice_secret = StaticSecret::random_from_rng(OsRng);
        let alice_public = PublicKey::from(&alice_secret);

        let bob_secret = StaticSecret::random_from_rng(OsRng);
        let bob_public = PublicKey::from(&bob_secret);

        // Both sides compute the same shared secret
        let alice_shared = compute_shared_secret(&alice_secret, &bob_public);
        let bob_shared = compute_shared_secret(&bob_secret, &alice_public);
        assert_eq!(alice_shared, bob_shared);

        // Alice encrypts, Bob decrypts
        let plaintext = b"Li-Fe-P-O phase diagram results";
        let encrypted = encrypt(&alice_shared, plaintext).unwrap();
        let decrypted = decrypt(&bob_shared, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails_decrypt() {
        let secret = StaticSecret::random_from_rng(OsRng);
        let shared = compute_shared_secret(&secret, &PublicKey::from(&secret));

        let encrypted = encrypt(&shared, b"secret data").unwrap();

        // Try decrypting with a different key
        let wrong_secret = StaticSecret::random_from_rng(OsRng);
        let wrong_shared = compute_shared_secret(&wrong_secret, &PublicKey::from(&wrong_secret));

        assert!(decrypt(&wrong_shared, &encrypted).is_err());
    }

    #[test]
    fn key_rotation_produces_new_key() {
        let tmp = TempDir::new().unwrap();
        let (_, public1) = load_or_generate_key(tmp.path()).unwrap();
        let public2 = rotate_key(tmp.path()).unwrap();

        assert_ne!(public1.as_bytes(), public2.as_bytes());

        // New key loads correctly
        let (_, public3) = load_or_generate_key(tmp.path()).unwrap();
        assert_eq!(public2.as_bytes(), public3.as_bytes());
    }

    #[test]
    fn public_key_encode_decode_roundtrip() {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);

        let encoded = encode_public_key(&public);
        let decoded = decode_public_key(&encoded).unwrap();

        assert_eq!(public.as_bytes(), decoded.as_bytes());
    }

    #[test]
    fn short_data_rejected() {
        let shared = [0u8; 32];
        assert!(decrypt(&shared, &[0u8; 10]).is_err());
    }

    #[test]
    fn signing_key_generate_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let (signing1, public1) = load_or_generate_signing_key(tmp.path()).unwrap();
        let (signing2, public2) = load_or_generate_signing_key(tmp.path()).unwrap();

        assert_eq!(public1.as_bytes(), public2.as_bytes());
        assert_eq!(signing1.to_bytes(), signing2.to_bytes());
    }

    #[test]
    fn signing_roundtrip_verifies() {
        let tmp = TempDir::new().unwrap();
        let (signing, public) = load_or_generate_signing_key(tmp.path()).unwrap();
        let payload = br#"{"service":"ssh","owner_user_id":"user_123"}"#;

        let signature = sign_bytes(&signing, payload);
        verify_signature(&public, payload, &signature).unwrap();
    }

    #[test]
    fn wrong_signing_key_fails_verification() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();
        let (signing, _) = load_or_generate_signing_key(tmp1.path()).unwrap();
        let (_, wrong_public) = load_or_generate_signing_key(tmp2.path()).unwrap();
        let payload = b"ssh claim payload";

        let signature = sign_bytes(&signing, payload);
        assert!(verify_signature(&wrong_public, payload, &signature).is_err());
    }

    #[test]
    fn signing_public_key_encode_decode_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let (_, public) = load_or_generate_signing_key(tmp.path()).unwrap();

        let encoded = encode_signing_public_key(&public);
        let decoded = decode_signing_public_key(&encoded).unwrap();

        assert_eq!(public.as_bytes(), decoded.as_bytes());
    }
}
