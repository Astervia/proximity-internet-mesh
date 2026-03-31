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
        let counter = u32::from_be_bytes(frame.nonce[8..12].try_into().unwrap()) as u64;
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
mod tests {
    use super::*;

    fn test_key() -> [u8; 32] {
        [0x42u8; 32]
    }

    fn test_prefix() -> [u8; 8] {
        [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let cipher = SessionCipher::new(&test_key(), test_prefix());
        let plaintext = b"hello proximity mesh";

        let encrypted = cipher.encrypt(plaintext).unwrap();
        let decrypted = cipher.decrypt(&encrypted).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn ciphertext_differs_from_plaintext() {
        let cipher = SessionCipher::new(&test_key(), test_prefix());
        let plaintext = b"hello proximity mesh";

        let encrypted = cipher.encrypt(plaintext).unwrap();
        assert_ne!(encrypted.ciphertext, plaintext);
    }

    #[test]
    fn tampered_ciphertext_fails_decryption() {
        let cipher = SessionCipher::new(&test_key(), test_prefix());
        let plaintext = b"important data";

        let mut encrypted = cipher.encrypt(plaintext).unwrap();
        encrypted.ciphertext[0] ^= 0xFF;

        let result = cipher.decrypt(&encrypted);
        assert!(matches!(result, Err(SessionError::DecryptionFailed)));
    }

    #[test]
    fn wrong_key_fails_decryption() {
        let cipher1 = SessionCipher::new(&[0x01; 32], test_prefix());
        let cipher2 = SessionCipher::new(&[0x02; 32], test_prefix());

        let encrypted = cipher1.encrypt(b"secret").unwrap();
        let result = cipher2.decrypt(&encrypted);
        assert!(matches!(result, Err(SessionError::DecryptionFailed)));
    }

    #[test]
    fn sequential_encryptions_use_different_nonces() {
        let cipher = SessionCipher::new(&test_key(), test_prefix());

        let e1 = cipher.encrypt(b"msg1").unwrap();
        let e2 = cipher.encrypt(b"msg2").unwrap();

        assert_ne!(e1.nonce, e2.nonce);
    }

    #[test]
    fn replayed_nonce_is_rejected() {
        let cipher = SessionCipher::new(&test_key(), test_prefix());
        let plaintext = b"original message";

        let frame = cipher.encrypt(plaintext).unwrap();
        // First decryption succeeds.
        let decrypted = cipher.decrypt(&frame).unwrap();
        assert_eq!(decrypted, plaintext);
        // Second decryption with the same frame (replayed nonce) must be rejected.
        let result = cipher.decrypt(&frame);
        assert!(matches!(result, Err(SessionError::ReplayedNonce)));
    }

    #[test]
    fn nonce_counter_overflow_returns_error() {
        let cipher = SessionCipher::new(&test_key(), test_prefix());

        // Set counter near the max
        cipher.counter.store(MAX_NONCE_COUNTER, Ordering::SeqCst);

        let result = cipher.encrypt(b"should fail");
        assert!(matches!(result, Err(SessionError::NonceExhausted)));
    }

    #[test]
    fn empty_plaintext_round_trip() {
        let cipher = SessionCipher::new(&test_key(), test_prefix());
        let encrypted = cipher.encrypt(b"").unwrap();
        let decrypted = cipher.decrypt(&encrypted).unwrap();
        assert!(decrypted.is_empty());
    }

    #[test]
    fn large_plaintext_round_trip() {
        let cipher = SessionCipher::new(&test_key(), test_prefix());
        let plaintext = vec![0xAB; 64 * 1024]; // 64KB

        let encrypted = cipher.encrypt(&plaintext).unwrap();
        let decrypted = cipher.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }
}
