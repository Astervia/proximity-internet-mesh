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
mod tests;
