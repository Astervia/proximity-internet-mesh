use super::super::*;

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
