use super::super::*;
use crate::identity::Identity;

#[test]
fn full_handshake_produces_matching_keys() {
    let alice_id = Identity::generate();
    let bob_id = Identity::generate();

    let mut alice = Handshaker::new(&alice_id);
    let mut bob = Handshaker::new(&bob_id);

    // Alice initiates
    let init = alice.initiate();

    // Bob responds
    let response = bob.respond(&init).unwrap();

    // Alice finalizes
    alice.finalize_initiator(&response).unwrap();

    // Both should have the same session key
    let alice_key = alice.session_key().unwrap();
    let bob_key = bob.session_key().unwrap();
    assert_eq!(alice_key.as_bytes(), bob_key.as_bytes());
}

#[test]
fn confirm_messages_verify() {
    let alice_id = Identity::generate();
    let bob_id = Identity::generate();

    let mut alice = Handshaker::new(&alice_id);
    let mut bob = Handshaker::new(&bob_id);

    let init = alice.initiate();
    let response = bob.respond(&init).unwrap();
    alice.finalize_initiator(&response).unwrap();

    // Both produce confirms
    let alice_confirm = alice.make_confirm().unwrap();
    let bob_confirm = bob.make_confirm().unwrap();

    // And both can verify each other's confirms
    // (transcripts are the same, keys are the same → HMACs match)
    assert_eq!(alice_confirm.hmac, bob_confirm.hmac);
    alice.verify_confirm(&bob_confirm).unwrap();
    bob.verify_confirm(&alice_confirm).unwrap();
}

#[test]
fn tampered_signature_rejected() {
    let alice_id = Identity::generate();
    let bob_id = Identity::generate();

    let mut alice = Handshaker::new(&alice_id);
    let mut bob = Handshaker::new(&bob_id);

    let mut init = alice.initiate();
    // Tamper with the signature
    init.signature[0] ^= 0xFF;

    let result = bob.respond(&init);
    assert!(matches!(result, Err(HandshakeError::InvalidSignature)));
}

#[test]
fn wrong_confirm_hmac_rejected() {
    let alice_id = Identity::generate();
    let bob_id = Identity::generate();

    let mut alice = Handshaker::new(&alice_id);
    let mut bob = Handshaker::new(&bob_id);

    let init = alice.initiate();
    let response = bob.respond(&init).unwrap();
    alice.finalize_initiator(&response).unwrap();

    let mut bad_confirm = alice.make_confirm().unwrap();
    bad_confirm.hmac[0] ^= 0xFF;

    let result = bob.verify_confirm(&bad_confirm);
    assert!(matches!(result, Err(HandshakeError::ConfirmMismatch)));
}

#[test]
fn different_sessions_produce_different_keys() {
    let alice_id = Identity::generate();
    let bob_id = Identity::generate();

    // Session 1
    let mut a1 = Handshaker::new(&alice_id);
    let mut b1 = Handshaker::new(&bob_id);
    let init1 = a1.initiate();
    let resp1 = b1.respond(&init1).unwrap();
    a1.finalize_initiator(&resp1).unwrap();

    // Session 2 (same identities, different ephemeral keys)
    let mut a2 = Handshaker::new(&alice_id);
    let mut b2 = Handshaker::new(&bob_id);
    let init2 = a2.initiate();
    let resp2 = b2.respond(&init2).unwrap();
    a2.finalize_initiator(&resp2).unwrap();

    assert_ne!(
        a1.session_key().unwrap().as_bytes(),
        a2.session_key().unwrap().as_bytes()
    );
}

#[test]
fn matching_mesh_keys_yield_matching_session_keys() {
    let mesh_key = [0xABu8; 32];
    let alice_id = Identity::generate();
    let bob_id = Identity::generate();

    let mut alice = Handshaker::new(&alice_id).with_mesh_handshake_key(mesh_key);
    let mut bob = Handshaker::new(&bob_id).with_mesh_handshake_key(mesh_key);

    let init = alice.initiate();
    let response = bob.respond(&init).unwrap();
    alice.finalize_initiator(&response).unwrap();

    assert_eq!(
        alice.session_key().unwrap().as_bytes(),
        bob.session_key().unwrap().as_bytes()
    );
    let alice_confirm = alice.make_confirm().unwrap();
    bob.verify_confirm(&alice_confirm).unwrap();
}

#[test]
fn mismatched_mesh_keys_are_rejected_at_confirm() {
    let alice_mesh = [0xABu8; 32];
    let bob_mesh = [0xCDu8; 32];
    let alice_id = Identity::generate();
    let bob_id = Identity::generate();

    let mut alice = Handshaker::new(&alice_id).with_mesh_handshake_key(alice_mesh);
    let mut bob = Handshaker::new(&bob_id).with_mesh_handshake_key(bob_mesh);

    let init = alice.initiate();
    // Signatures still verify — the divergence is in the derived key.
    let response = bob.respond(&init).unwrap();
    alice.finalize_initiator(&response).unwrap();

    // Different IKMs → different session keys → confirm HMAC must fail.
    assert_ne!(
        alice.session_key().unwrap().as_bytes(),
        bob.session_key().unwrap().as_bytes()
    );
    let alice_confirm = alice.make_confirm().unwrap();
    let result = bob.verify_confirm(&alice_confirm);
    assert!(matches!(result, Err(HandshakeError::ConfirmMismatch)));
}

#[test]
fn open_and_private_meshes_do_not_interop() {
    let mesh_key = [0xABu8; 32];
    let alice_id = Identity::generate();
    let bob_id = Identity::generate();

    // Alice is on a private mesh; Bob is on the open mesh.
    let mut alice = Handshaker::new(&alice_id).with_mesh_handshake_key(mesh_key);
    let mut bob = Handshaker::new(&bob_id);

    let init = alice.initiate();
    let response = bob.respond(&init).unwrap();
    alice.finalize_initiator(&response).unwrap();

    assert_ne!(
        alice.session_key().unwrap().as_bytes(),
        bob.session_key().unwrap().as_bytes()
    );
    let alice_confirm = alice.make_confirm().unwrap();
    let result = bob.verify_confirm(&alice_confirm);
    assert!(matches!(result, Err(HandshakeError::ConfirmMismatch)));
}

#[test]
fn finalize_without_init_fails() {
    let id = Identity::generate();
    let mut hs = Handshaker::new(&id);
    let fake_response = HandshakeResponse {
        sender_pub: [0; 32],
        ephemeral_pub: [0; 32],
        nonce: [0; 32],
        signature: [0; 64],
    };
    let result = hs.finalize_initiator(&fake_response);
    assert!(matches!(result, Err(HandshakeError::InvalidState)));
}
