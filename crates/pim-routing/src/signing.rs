//! Ed25519 signing and verification for route update frames.
//!
//! Route advertisements are signed by their originator using their Ed25519
//! identity key.  Recipients verify the signature before installing any routes,
//! preventing injection of forged routing information.
//!
//! # Signed bytes
//!
//! The signature covers: `origin_id(16) || sequence(8) || entry_count(2) ||
//! entries(N × 22)`.  The `signature` field itself is excluded (naturally) and
//! the encoded representation is deterministic for a given frame.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use pim_protocol::RouteUpdateFrame;

/// Produce the canonical byte string that is signed/verified.
fn signing_bytes(frame: &RouteUpdateFrame) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(26 + frame.entries.len() * 22);
    bytes.extend_from_slice(frame.origin_id.as_bytes());
    bytes.extend_from_slice(&frame.sequence.to_be_bytes());
    bytes.extend_from_slice(&(frame.entries.len() as u16).to_be_bytes());
    for entry in &frame.entries {
        bytes.extend_from_slice(entry.destination.as_bytes());
        bytes.push(entry.hops);
        bytes.push(entry.flags);
        bytes.extend_from_slice(&entry.mesh_ip);
    }
    bytes
}

/// Sign a route update in-place, writing the Ed25519 signature into
/// `frame.signature`.
pub fn sign_route_update(frame: &mut RouteUpdateFrame, signing_key: &SigningKey) {
    let msg = signing_bytes(frame);
    let sig: Signature = signing_key.sign(&msg);
    frame.signature = sig.to_bytes();
}

/// Verify the Ed25519 signature on a route update.
///
/// Returns `true` only if the signature is well-formed and valid for the given
/// verifying key.  A zero-filled signature (legacy / unsigned) always returns
/// `false`.
pub fn verify_route_update(frame: &RouteUpdateFrame, verifying_key: &VerifyingKey) -> bool {
    if frame.signature == [0u8; 64] {
        return false; // unsigned — reject
    }
    let msg = signing_bytes(frame);
    let sig = Signature::from_bytes(&frame.signature);
    verifying_key.verify(&msg, &sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    use ed25519_dalek::SigningKey;
    use pim_core::NodeId;
    use pim_protocol::{RouteEntry, RouteUpdateFrame};
    use rand::rngs::OsRng;

    fn make_frame(origin: NodeId, seq: u64) -> RouteUpdateFrame {
        RouteUpdateFrame {
            origin_id: origin,
            sequence: seq,
            entries: vec![
                RouteEntry {
                    destination: NodeId::from_bytes([0x02; 16]),
                    hops: 1,
                    flags: 0x01,
                    mesh_ip: [10, 77, 0, 1],
                },
                RouteEntry {
                    destination: NodeId::from_bytes([0x03; 16]),
                    hops: 2,
                    flags: 0x00,
                    mesh_ip: [10, 77, 0, 10],
                },
            ],
            signature: [0u8; 64],
        }
    }

    fn make_key() -> SigningKey {
        SigningKey::generate(&mut OsRng)
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let key = make_key();
        let origin = NodeId::from_public_key(&key.verifying_key().to_bytes());
        let mut frame = make_frame(origin, 1);

        sign_route_update(&mut frame, &key);
        assert!(verify_route_update(&frame, &key.verifying_key()));
    }

    #[test]
    fn tampered_payload_fails_verification() {
        let key = make_key();
        let origin = NodeId::from_public_key(&key.verifying_key().to_bytes());
        let mut frame = make_frame(origin, 1);

        sign_route_update(&mut frame, &key);
        // Tamper with sequence number after signing
        frame.sequence += 1;
        assert!(!verify_route_update(&frame, &key.verifying_key()));
    }

    #[test]
    fn wrong_key_fails_verification() {
        let signer = make_key();
        let wrong_key = make_key();
        let origin = NodeId::from_public_key(&signer.verifying_key().to_bytes());
        let mut frame = make_frame(origin, 1);

        sign_route_update(&mut frame, &signer);
        assert!(!verify_route_update(&frame, &wrong_key.verifying_key()));
    }

    #[test]
    fn unsigned_frame_rejected() {
        let key = make_key();
        let origin = NodeId::from_public_key(&key.verifying_key().to_bytes());
        let frame = make_frame(origin, 1); // signature stays all-zero
        assert!(!verify_route_update(&frame, &key.verifying_key()));
    }

    #[test]
    fn different_sequence_produces_different_signature() {
        let key = make_key();
        let origin = NodeId::from_public_key(&key.verifying_key().to_bytes());
        let mut f1 = make_frame(origin, 1);
        let mut f2 = make_frame(origin, 2);
        sign_route_update(&mut f1, &key);
        sign_route_update(&mut f2, &key);
        assert_ne!(f1.signature, f2.signature);
    }
}
