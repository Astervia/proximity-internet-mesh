//! Session-level symmetric encryption for transport payloads.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Encrypted frame produced by SessionCipher.
#[derive(Clone, Debug)]
pub struct EncryptedFrame {
    /// 12-byte nonce used for this frame.
    pub nonce: [u8; 12],
    /// Ciphertext + AES-GCM authentication tag.
    pub ciphertext: Vec<u8>,
}

/// Symmetric cipher for encrypting/decrypting frames within a session.
///
/// Uses AES-256-GCM with an incrementing nonce counter to prevent reuse.
/// The nonce is constructed as: 8-byte random session prefix || 4-byte counter.
///
/// Replay protection: `decrypt` tracks the highest accepted counter and rejects
/// any frame whose counter is ≤ the last accepted value.
pub struct SessionCipher {
    cipher: Aes256Gcm,
    nonce_prefix: [u8; 8],
    counter: AtomicU32,
    /// Highest counter value accepted during decryption.
    /// Initialised to `u64::MAX` (sentinel meaning "no frame received yet").
    last_recv_counter: AtomicU64,
}

/// Maximum number of frames before the nonce counter wraps.
const MAX_NONCE_COUNTER: u32 = u32::MAX - 1;

impl SessionCipher {
    /// Create a new SessionCipher from a 32-byte key and 8-byte nonce prefix.
    pub fn new(key: &[u8; 32], nonce_prefix: [u8; 8]) -> Self {
        let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key is valid for AES-256");
        Self {
            cipher,
            nonce_prefix,
            counter: AtomicU32::new(0),
            last_recv_counter: AtomicU64::new(u64::MAX),
        }
    }

    /// Encrypt plaintext, returning the nonce and ciphertext.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<EncryptedFrame, SessionError> {
        let count = self.counter.fetch_add(1, Ordering::SeqCst);
        if count >= MAX_NONCE_COUNTER {
            return Err(SessionError::NonceExhausted);
        }

        let nonce_bytes = self.build_nonce(count);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|_| SessionError::EncryptionFailed)?;

        Ok(EncryptedFrame {
            nonce: nonce_bytes,
            ciphertext,
        })
    }

    /// Decrypt an encrypted frame.
    ///
    /// Rejects replayed frames: the counter embedded in `frame.nonce[8..12]` must
    /// be strictly greater than the last accepted counter.
    pub fn decrypt(&self, frame: &EncryptedFrame) -> Result<Vec<u8>, SessionError> {
        let counter = u32::from_be_bytes(
            frame
                .nonce
                .get(8..12)
                .ok_or(SessionError::InvalidNonce)?
                .try_into()
                .map_err(|_| SessionError::InvalidNonce)?,
        ) as u64;
        let last = self.last_recv_counter.load(Ordering::SeqCst);
        if last != u64::MAX && counter <= last {
            return Err(SessionError::ReplayedNonce);
        }

        let nonce = Nonce::from_slice(&frame.nonce);
        let plaintext = self
            .cipher
            .decrypt(nonce, frame.ciphertext.as_ref())
            .map_err(|_| SessionError::DecryptionFailed)?;

        self.last_recv_counter.store(counter, Ordering::SeqCst);
        Ok(plaintext)
    }

    /// Encrypts plaintext in-place, returning the generated nonce and tag.
    /// Returns an error if the nonce counter is exhausted or encryption fails.
    pub fn encrypt_in_place_detached(
        &self,
        payload: &mut [u8],
    ) -> Result<([u8; 12], [u8; 16]), SessionError> {
        let count = self.counter.fetch_add(1, Ordering::SeqCst);
        if count >= MAX_NONCE_COUNTER {
            return Err(SessionError::NonceExhausted);
        }

        let nonce_bytes = self.build_nonce(count);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let tag = aes_gcm::aead::AeadInPlace::encrypt_in_place_detached(
            &self.cipher,
            nonce,
            b"",
            payload,
        )
        .map_err(|_| SessionError::EncryptionFailed)?;

        let mut tag_bytes = [0u8; 16];
        tag_bytes.copy_from_slice(&tag);

        Ok((nonce_bytes, tag_bytes))
    }

    /// Decrypts a frame in-place, avoiding an allocation.
    /// Returns an error if the nonce is replayed, or if decryption fails.
    pub fn decrypt_in_place_detached(
        &self,
        nonce_bytes: &[u8; 12],
        payload: &mut [u8],
        tag_bytes: &[u8; 16],
    ) -> Result<(), SessionError> {
        let counter = u32::from_be_bytes(
            nonce_bytes
                .get(8..12)
                .ok_or(SessionError::InvalidNonce)?
                .try_into()
                .map_err(|_| SessionError::InvalidNonce)?,
        ) as u64;
        let last = self.last_recv_counter.load(Ordering::SeqCst);
        if last != u64::MAX && counter <= last {
            return Err(SessionError::ReplayedNonce);
        }

        let nonce = Nonce::from_slice(nonce_bytes);
        let tag = aes_gcm::aead::Tag::<aes_gcm::Aes256Gcm>::from_slice(tag_bytes);
        aes_gcm::aead::AeadInPlace::decrypt_in_place_detached(
            &self.cipher,
            nonce,
            b"",
            payload,
            tag,
        )
        .map_err(|_| SessionError::DecryptionFailed)?;

        self.last_recv_counter.store(counter, Ordering::SeqCst);
        Ok(())
    }

    /// Build a 12-byte nonce from the prefix and counter.
    fn build_nonce(&self, counter: u32) -> [u8; 12] {
        let mut nonce = [0u8; 12];
        nonce[..8].copy_from_slice(&self.nonce_prefix);
        nonce[8..12].copy_from_slice(&counter.to_be_bytes());
        nonce
    }
}

#[derive(Debug, thiserror::Error)]
/// Errors returned by [`SessionCipher`].
pub enum SessionError {
    /// The nonce length or format is invalid.
    #[error("invalid nonce format")]
    InvalidNonce,
    /// The nonce counter reached its maximum and the session must be replaced.
    #[error("nonce counter exhausted — session must be rekeyed")]
    NonceExhausted,
    /// Encrypting the payload failed.
    #[error("encryption failed")]
    EncryptionFailed,
    /// Decrypting the payload failed.
    #[error("decryption failed (invalid ciphertext or wrong key)")]
    DecryptionFailed,
    /// A frame reused or regressed the receive nonce counter.
    #[error("replayed nonce: frame counter has already been accepted")]
    ReplayedNonce,
}

#[cfg(test)]
mod tests;
