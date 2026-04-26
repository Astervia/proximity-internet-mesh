use super::super::*;

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

#[test]
fn encrypt_in_place_detached_round_trip() {
    let cipher = SessionCipher::new(&test_key(), test_prefix());
    let plaintext = b"hello proximity mesh";

    let mut buffer = plaintext.to_vec();
    let (nonce_bytes, tag_bytes) = cipher.encrypt_in_place_detached(&mut buffer).unwrap();

    assert_ne!(buffer, plaintext);

    cipher
        .decrypt_in_place_detached(&nonce_bytes, &mut buffer, &tag_bytes)
        .unwrap();

    assert_eq!(buffer, plaintext);
}
