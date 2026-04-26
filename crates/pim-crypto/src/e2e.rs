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

use aes_gcm::aead::KeyInit;
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use sha2::Sha256;
use x25519_dalek::{PublicKey as X25519PublicKey, SharedSecret, StaticSecret};

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

fn derive_e2e_cipher(shared_secret: &SharedSecret) -> Aes256Gcm {
    let hk = Hkdf::<Sha256>::new(None, shared_secret.as_bytes());
    let mut okm = [0u8; 32];
    hk.expand(b"pim-e2e-v1", &mut okm)
        .expect("32 bytes is valid");
    Aes256Gcm::new_from_slice(&okm).expect("32 bytes is valid")
}

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

    // Random nonce
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    rand::RngCore::fill_bytes(&mut OsRng, &mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    // AES-256-GCM encrypt in-place
    let cipher = derive_e2e_cipher(&shared_secret);

    // Assemble directly into the output buffer to avoid intermediate allocations
    let mut out = Vec::with_capacity(HEADER_SIZE + plaintext.len() + TAG_SIZE);
    out.extend_from_slice(&ephemeral_pub.to_bytes());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(plaintext);

    use aes_gcm::aead::AeadInPlace;
    let tag = cipher
        .encrypt_in_place_detached(nonce, b"", &mut out[HEADER_SIZE..])
        .map_err(|_| E2eError::EncryptionFailed)?;

    out.extend_from_slice(&tag);

    Ok(out)
}

/// Decrypt an E2E-encrypted frame using the gateway's Ed25519 seed.
///
/// `ciphertext` is the output of [`e2e_encrypt`].
pub fn e2e_decrypt<'a>(
    ciphertext: &'a mut [u8],
    gateway_ed25519_seed: &[u8; 32],
) -> Result<&'a [u8], E2eError> {
    if ciphertext.len() < HEADER_SIZE + TAG_SIZE {
        return Err(E2eError::TooShort);
    }

    // Parse header
    let mut ephemeral_pub_bytes = [0u8; 32];
    ephemeral_pub_bytes.copy_from_slice(&ciphertext[..32]);
    let ephemeral_pub = X25519PublicKey::from(ephemeral_pub_bytes);

    let nonce_bytes = &ciphertext[32..44];

    // Derive gateway's static X25519 secret from Ed25519 seed
    let gw_secret = x25519_from_seed(gateway_ed25519_seed);

    // ECDH
    let shared_secret = gw_secret.diffie_hellman(&ephemeral_pub);

    // Decrypt
    use aes_gcm::aead::AeadInPlace;
    let cipher = derive_e2e_cipher(&shared_secret);

    let mut nonce_array = [0u8; 12];
    nonce_array.copy_from_slice(nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_array);

    let tag_start = ciphertext.len() - TAG_SIZE;
    let mut tag_array = [0u8; 16];
    tag_array.copy_from_slice(&ciphertext[tag_start..]);
    let tag = aes_gcm::aead::Tag::<Aes256Gcm>::from_slice(&tag_array);

    let (body, _) = ciphertext.split_at_mut(tag_start);
    let (_, payload) = body.split_at_mut(HEADER_SIZE);

    cipher
        .decrypt_in_place_detached(nonce, b"", payload, tag)
        .map_err(|_| E2eError::DecryptionFailed)?;

    Ok(&ciphertext[HEADER_SIZE..tag_start])
}

/// Decrypt an E2E-encrypted frame in-place, modifying the provided buffer.
///
/// Upon success, the plaintext will be shifted to the start of `buffer`
/// and the buffer will be truncated to the plaintext length.
pub fn e2e_decrypt_in_place(
    buffer: &mut Vec<u8>,
    gateway_ed25519_seed: &[u8; 32],
) -> Result<(), E2eError> {
    if buffer.len() < HEADER_SIZE + TAG_SIZE {
        return Err(E2eError::TooShort);
    }

    // Parse header
    let mut ephemeral_pub_bytes = [0u8; 32];
    ephemeral_pub_bytes.copy_from_slice(&buffer[..32]);
    let ephemeral_pub = X25519PublicKey::from(ephemeral_pub_bytes);

    let nonce = *Nonce::from_slice(&buffer[32..44]);

    // Derive gateway's static X25519 secret from Ed25519 seed
    let gw_secret = x25519_from_seed(gateway_ed25519_seed);

    // ECDH
    let shared_secret = gw_secret.diffie_hellman(&ephemeral_pub);

    let cipher = derive_e2e_cipher(&shared_secret);

    // Decrypt in place. AeadInPlace requires the tag to be provided and removed from the ciphertext.
    let ct_len = buffer.len() - HEADER_SIZE - TAG_SIZE;
    let mut tag_bytes = [0u8; TAG_SIZE];
    tag_bytes.copy_from_slice(&buffer[buffer.len() - TAG_SIZE..]);
    let tag = aes_gcm::aead::Tag::<Aes256Gcm>::from_slice(&tag_bytes);

    // Shift ciphertext to the start of the buffer so AeadInPlace can work on it
    buffer.copy_within(HEADER_SIZE..HEADER_SIZE + ct_len, 0);
    buffer.truncate(ct_len);

    aes_gcm::aead::AeadInPlace::decrypt_in_place_detached(&cipher, &nonce, b"", buffer, tag)
        .map_err(|_| E2eError::DecryptionFailed)?;

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
