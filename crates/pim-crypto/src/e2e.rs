//! End-to-end encryption between a mesh client and its gateway.
//!
//! Uses ECIES: Ephemeral X25519 ECDH + HKDF-SHA256 → AES-256-GCM.
//!
//! # Wire format
//!
//! ```text
//! ephemeral_pub (32)  || nonce (12) || ciphertext (variable) || tag (16)
//! ```
//!
//! The gateway's static X25519 key is derived from its Ed25519 seed via HKDF
//! so that no separate key file is needed.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use sha2::Sha256;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
/// Errors returned by end-to-end client-to-gateway encryption helpers.
pub enum E2eError {
    /// The ciphertext was too short to contain the required header and tag.
    #[error("ciphertext too short to contain E2E header")]
    TooShort,
    /// Authentication failed or the wrong key was used.
    #[error("decryption failed (invalid ciphertext or wrong key)")]
    DecryptionFailed,
    /// Encryption failed unexpectedly.
    #[error("encryption failed")]
    EncryptionFailed,
}

// ── Key material sizes ────────────────────────────────────────────────────────

const EPHEMERAL_PUB_SIZE: usize = 32;
const NONCE_SIZE: usize = 12;
const TAG_SIZE: usize = 16;
const HEADER_SIZE: usize = EPHEMERAL_PUB_SIZE + NONCE_SIZE; // 44

// ── Public API ────────────────────────────────────────────────────────────────

/// Derive an X25519 static secret from an Ed25519 seed.
///
/// Both the client (for ECDH) and the gateway (for decryption) use this same
/// derivation so that the gateway's public X25519 key can be included in its
/// advertisement or config without requiring a separate key file.
pub fn x25519_from_seed(ed25519_seed: &[u8; 32]) -> StaticSecret {
    let hk = Hkdf::<Sha256>::new(None, ed25519_seed);
    let mut key_bytes = [0u8; 32];
    hk.expand(b"pim-x25519-identity-v1", &mut key_bytes)
        .expect("32 bytes is valid");
    StaticSecret::from(key_bytes)
}

/// Return the X25519 public key corresponding to `ed25519_seed`.
pub fn x25519_public_from_seed(ed25519_seed: &[u8; 32]) -> [u8; 32] {
    X25519PublicKey::from(&x25519_from_seed(ed25519_seed)).to_bytes()
}

/// Encrypt `plaintext` to `gateway_x25519_pub` using ECIES.
///
/// Returns: `ephemeral_pub (32) || nonce (12) || ciphertext || tag (16)`.
pub fn e2e_encrypt(plaintext: &[u8], gateway_x25519_pub: &[u8; 32]) -> Result<Vec<u8>, E2eError> {
    // Generate ephemeral X25519 key pair
    let ephemeral_secret = StaticSecret::random_from_rng(OsRng);
    let ephemeral_pub = X25519PublicKey::from(&ephemeral_secret);

    // ECDH with gateway's static public key
    let gateway_pub = X25519PublicKey::from(*gateway_x25519_pub);
    let shared_secret = ephemeral_secret.diffie_hellman(&gateway_pub);

    // HKDF → AES-256 key
    let hk = Hkdf::<Sha256>::new(None, shared_secret.as_bytes());
    let mut aes_key = [0u8; 32];
    hk.expand(b"pim-e2e-v1", &mut aes_key)
        .expect("32 bytes is valid");

    // Random nonce
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    rand::RngCore::fill_bytes(&mut OsRng, &mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    // AES-256-GCM encrypt
    let cipher = Aes256Gcm::new_from_slice(&aes_key).expect("32 bytes is valid");
    let ciphertext_with_tag = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| E2eError::EncryptionFailed)?;

    // Assemble: ephemeral_pub || nonce || ciphertext_with_tag
    let mut out = Vec::with_capacity(HEADER_SIZE + ciphertext_with_tag.len());
    out.extend_from_slice(&ephemeral_pub.to_bytes());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext_with_tag);

    Ok(out)
}

/// Decrypt an E2E-encrypted frame using the gateway's Ed25519 seed.
///
/// `ciphertext` is the output of [`e2e_encrypt`].
pub fn e2e_decrypt(
    ciphertext: &[u8],
    gateway_ed25519_seed: &[u8; 32],
) -> Result<Vec<u8>, E2eError> {
    if ciphertext.len() < HEADER_SIZE + TAG_SIZE {
        return Err(E2eError::TooShort);
    }

    // Parse header
    let mut ephemeral_pub_bytes = [0u8; 32];
    ephemeral_pub_bytes.copy_from_slice(&ciphertext[..32]);
    let ephemeral_pub = X25519PublicKey::from(ephemeral_pub_bytes);

    let nonce_bytes = &ciphertext[32..44];
    let ct = &ciphertext[44..];

    // Derive gateway's static X25519 secret from Ed25519 seed
    let gw_secret = x25519_from_seed(gateway_ed25519_seed);

    // ECDH
    let shared_secret = gw_secret.diffie_hellman(&ephemeral_pub);

    // HKDF → AES-256 key (same derivation as encrypt)
    let hk = Hkdf::<Sha256>::new(None, shared_secret.as_bytes());
    let mut aes_key = [0u8; 32];
    hk.expand(b"pim-e2e-v1", &mut aes_key)
        .expect("32 bytes is valid");

    // Decrypt
    let cipher = Aes256Gcm::new_from_slice(&aes_key).expect("32 bytes is valid");
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ct)
        .map_err(|_| E2eError::DecryptionFailed)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn gw_seed() -> [u8; 32] {
        [0x42u8; 32]
    }

    fn relay_seed() -> [u8; 32] {
        [0xABu8; 32]
    }

    #[test]
    fn e2e_round_trip() {
        let seed = gw_seed();
        let gw_pub = x25519_public_from_seed(&seed);

        let plaintext = b"hello gateway, this is a secret IP packet";
        let encrypted = e2e_encrypt(plaintext, &gw_pub).unwrap();
        let decrypted = e2e_decrypt(&encrypted, &seed).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypted_is_larger_than_plaintext() {
        let seed = gw_seed();
        let gw_pub = x25519_public_from_seed(&seed);

        let plain = b"data";
        let enc = e2e_encrypt(plain, &gw_pub).unwrap();

        // At minimum: HEADER (44) + TAG (16) = 60 bytes overhead
        assert!(enc.len() >= plain.len() + HEADER_SIZE + TAG_SIZE);
    }

    #[test]
    fn different_encryptions_produce_different_ciphertexts() {
        let seed = gw_seed();
        let gw_pub = x25519_public_from_seed(&seed);
        let plain = b"test message";

        let e1 = e2e_encrypt(plain, &gw_pub).unwrap();
        let e2 = e2e_encrypt(plain, &gw_pub).unwrap();

        // Different ephemeral keys → different ciphertexts
        assert_ne!(e1, e2);
    }

    #[test]
    fn relay_cannot_decrypt_e2e_payload() {
        let gw_seed = gw_seed();
        let gw_pub = x25519_public_from_seed(&gw_seed);
        let relay_seed = relay_seed();

        let plain = b"confidential";
        let enc = e2e_encrypt(plain, &gw_pub).unwrap();

        // Relay tries to decrypt with its own seed — must fail
        let result = e2e_decrypt(&enc, &relay_seed);
        assert!(
            matches!(result, Err(E2eError::DecryptionFailed)),
            "relay must not be able to decrypt gateway's E2E payload"
        );
    }

    #[test]
    fn tampered_e2e_payload_rejected() {
        let seed = gw_seed();
        let gw_pub = x25519_public_from_seed(&seed);

        let plain = b"important packet";
        let mut enc = e2e_encrypt(plain, &gw_pub).unwrap();

        // Flip a bit in the ciphertext portion
        let last = enc.len() - 1;
        enc[last] ^= 0xFF;

        let result = e2e_decrypt(&enc, &seed);
        assert!(matches!(result, Err(E2eError::DecryptionFailed)));
    }

    #[test]
    fn too_short_ciphertext_rejected() {
        let seed = gw_seed();
        let result = e2e_decrypt(&[0u8; 10], &seed);
        assert!(matches!(result, Err(E2eError::TooShort)));
    }

    #[test]
    fn x25519_from_same_seed_is_deterministic() {
        let seed = gw_seed();
        let pub1 = x25519_public_from_seed(&seed);
        let pub2 = x25519_public_from_seed(&seed);
        assert_eq!(pub1, pub2);
    }

    #[test]
    fn x25519_from_different_seeds_is_different() {
        let pub1 = x25519_public_from_seed(&gw_seed());
        let pub2 = x25519_public_from_seed(&relay_seed());
        assert_ne!(pub1, pub2);
    }
}
