use super::super::*;

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
    let mut encrypted = e2e_encrypt(plaintext, &gw_pub).unwrap();
    let decrypted = e2e_decrypt(&mut encrypted, &seed).unwrap();

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
    let mut enc = e2e_encrypt(plain, &gw_pub).unwrap();

    // Relay tries to decrypt with its own seed — must fail
    let result = e2e_decrypt(&mut enc, &relay_seed);
    assert!(
        matches!(result, Err(E2eError::DecryptionFailed)),
        "relay must not be able to decrypt gateway's E2E payload"
    );
}

#[test]
fn e2e_decrypt_in_place_round_trip() {
    let seed = gw_seed();
    let gw_pub = x25519_public_from_seed(&seed);

    let plaintext = b"hello gateway, this is an in-place secret IP packet";
    let mut encrypted = e2e_encrypt(plaintext, &gw_pub).unwrap();

    e2e_decrypt_in_place(&mut encrypted, &seed).unwrap();

    assert_eq!(encrypted, plaintext);
}

#[test]
fn e2e_decrypt_in_place_too_short() {
    let seed = gw_seed();
    let mut buffer = vec![0u8; 10];
    let result = e2e_decrypt_in_place(&mut buffer, &seed);
    assert!(matches!(result, Err(E2eError::TooShort)));
}

#[test]
fn e2e_decrypt_in_place_tampered_payload_rejected() {
    let seed = gw_seed();
    let gw_pub = x25519_public_from_seed(&seed);

    let plain = b"important packet";
    let mut enc = e2e_encrypt(plain, &gw_pub).unwrap();

    // Flip a bit in the ciphertext portion
    let last = enc.len() - 1;
    enc[last] ^= 0xFF;

    let result = e2e_decrypt_in_place(&mut enc, &seed);
    assert!(matches!(result, Err(E2eError::DecryptionFailed)));
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

    let result = e2e_decrypt(&mut enc, &seed);
    assert!(matches!(result, Err(E2eError::DecryptionFailed)));
}

#[test]
fn too_short_ciphertext_rejected() {
    let seed = gw_seed();
    let mut too_short = [0u8; 10];
    let result = e2e_decrypt(&mut too_short, &seed);
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
