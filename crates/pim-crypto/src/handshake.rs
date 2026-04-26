//! Authenticated peer handshake and session-key derivation.

use ed25519_dalek::{Signer, Verifier};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use sha2::Sha256;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

use crate::identity::Identity;

type HmacSha256 = Hmac<Sha256>;

/// The result of a successful handshake: a shared session key.
#[derive(Clone)]
pub struct SessionKey {
    key: [u8; 32],
}

impl SessionKey {
    /// Return the raw 32-byte session key material.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.key
    }
}

/// Data sent by the initiator to start a handshake.
pub struct HandshakeInit {
    /// Initiator's long-term Ed25519 public key.
    pub sender_pub: [u8; 32],
    /// Ephemeral X25519 public key for this handshake.
    pub ephemeral_pub: [u8; 32],
    /// Random nonce.
    pub nonce: [u8; 32],
    /// Ed25519 signature over (ephemeral_pub || nonce).
    pub signature: [u8; 64],
}

/// Data sent by the responder.
pub struct HandshakeResponse {
    /// Responder's long-term Ed25519 public key.
    pub sender_pub: [u8; 32],
    /// Ephemeral X25519 public key.
    pub ephemeral_pub: [u8; 32],
    /// Random nonce.
    pub nonce: [u8; 32],
    /// Ed25519 signature over (ephemeral_pub || nonce).
    pub signature: [u8; 64],
}

/// Confirmation message containing HMAC of the handshake transcript.
pub struct HandshakeConfirm {
    /// Transcript MAC proving both sides derived the same session key.
    pub hmac: [u8; 32],
}

/// Manages one side of the handshake protocol.
///
/// Usage:
/// - Initiator: `initiate()` → send Init → receive Response → `finalize_initiator()` → send Confirm
/// - Responder: receive Init → `respond()` → send Response → receive Confirm → `verify_confirm()`
pub struct Handshaker {
    identity: HandshakeIdentity,
    ephemeral_secret: Option<StaticSecret>,
    ephemeral_pub: Option<[u8; 32]>,
    session_key: Option<SessionKey>,
    transcript: Vec<u8>,
}

/// Subset of Identity needed for handshake (avoids lifetime issues).
struct HandshakeIdentity {
    signing_key_bytes: [u8; 32],
    verifying_key_bytes: [u8; 32],
}

impl Handshaker {
    /// Create a handshake state machine bound to the local node identity.
    pub fn new(identity: &Identity) -> Self {
        Self {
            identity: HandshakeIdentity {
                signing_key_bytes: identity.signing_key().to_bytes(),
                verifying_key_bytes: identity.public_key_bytes(),
            },
            ephemeral_secret: None,
            ephemeral_pub: None,
            session_key: None,
            transcript: Vec::new(),
        }
    }

    /// Initiator step 1: produce a HandshakeInit.
    pub fn initiate(&mut self) -> HandshakeInit {
        let ephemeral_secret = StaticSecret::random_from_rng(OsRng);
        let ephemeral_pub = X25519PublicKey::from(&ephemeral_secret);
        let ephemeral_pub_bytes = ephemeral_pub.to_bytes();

        let mut nonce = [0u8; 32];
        rand::RngCore::fill_bytes(&mut OsRng, &mut nonce);

        // Sign (ephemeral_pub || nonce)
        let mut msg = Vec::with_capacity(64);
        msg.extend_from_slice(&ephemeral_pub_bytes);
        msg.extend_from_slice(&nonce);

        let signing_key = ed25519_dalek::SigningKey::from_bytes(&self.identity.signing_key_bytes);
        let signature = signing_key.sign(&msg);

        // Store state
        self.ephemeral_secret = Some(ephemeral_secret);
        self.ephemeral_pub = Some(ephemeral_pub_bytes);

        let init = HandshakeInit {
            sender_pub: self.identity.verifying_key_bytes,
            ephemeral_pub: ephemeral_pub_bytes,
            nonce,
            signature: signature.to_bytes(),
        };

        // Append to transcript
        self.transcript.extend_from_slice(&init.sender_pub);
        self.transcript.extend_from_slice(&init.ephemeral_pub);
        self.transcript.extend_from_slice(&init.nonce);

        init
    }

    /// Responder step: verify the init and produce a response + derive session key.
    pub fn respond(&mut self, init: &HandshakeInit) -> Result<HandshakeResponse, HandshakeError> {
        // Verify initiator's signature
        let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&init.sender_pub)
            .map_err(|_| HandshakeError::InvalidPublicKey)?;

        let mut msg = Vec::with_capacity(64);
        msg.extend_from_slice(&init.ephemeral_pub);
        msg.extend_from_slice(&init.nonce);

        let signature = ed25519_dalek::Signature::from_bytes(&init.signature);
        verifying_key
            .verify(&msg, &signature)
            .map_err(|_| HandshakeError::InvalidSignature)?;

        // Generate our ephemeral key
        let ephemeral_secret = StaticSecret::random_from_rng(OsRng);
        let ephemeral_pub = X25519PublicKey::from(&ephemeral_secret);
        let ephemeral_pub_bytes = ephemeral_pub.to_bytes();

        let mut nonce = [0u8; 32];
        rand::RngCore::fill_bytes(&mut OsRng, &mut nonce);

        // Sign our (ephemeral_pub || nonce)
        let mut sign_msg = Vec::with_capacity(64);
        sign_msg.extend_from_slice(&ephemeral_pub_bytes);
        sign_msg.extend_from_slice(&nonce);

        let signing_key = ed25519_dalek::SigningKey::from_bytes(&self.identity.signing_key_bytes);
        let signature = signing_key.sign(&sign_msg);

        // Derive shared secret: X25519(our_ephemeral, their_ephemeral_pub)
        let their_ephemeral = X25519PublicKey::from(init.ephemeral_pub);
        let shared_secret = ephemeral_secret.diffie_hellman(&their_ephemeral);

        // Build transcript
        self.transcript.extend_from_slice(&init.sender_pub);
        self.transcript.extend_from_slice(&init.ephemeral_pub);
        self.transcript.extend_from_slice(&init.nonce);
        self.transcript
            .extend_from_slice(&self.identity.verifying_key_bytes);
        self.transcript.extend_from_slice(&ephemeral_pub_bytes);
        self.transcript.extend_from_slice(&nonce);

        // Derive session key via HKDF
        let mut salt = Vec::with_capacity(64);
        salt.extend_from_slice(&init.nonce);
        salt.extend_from_slice(&nonce);

        let hk = Hkdf::<Sha256>::new(Some(&salt), shared_secret.as_bytes());
        let mut key = [0u8; 32];
        hk.expand(b"pim-session-v1", &mut key)
            .expect("32 bytes is valid for HKDF-SHA256");

        self.session_key = Some(SessionKey { key });
        self.ephemeral_pub = Some(ephemeral_pub_bytes);

        Ok(HandshakeResponse {
            sender_pub: self.identity.verifying_key_bytes,
            ephemeral_pub: ephemeral_pub_bytes,
            nonce,
            signature: signature.to_bytes(),
        })
    }

    /// Initiator step 2: verify response and derive session key.
    pub fn finalize_initiator(
        &mut self,
        response: &HandshakeResponse,
    ) -> Result<(), HandshakeError> {
        // Verify responder's signature
        let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&response.sender_pub)
            .map_err(|_| HandshakeError::InvalidPublicKey)?;

        let mut msg = Vec::with_capacity(64);
        msg.extend_from_slice(&response.ephemeral_pub);
        msg.extend_from_slice(&response.nonce);

        let signature = ed25519_dalek::Signature::from_bytes(&response.signature);
        verifying_key
            .verify(&msg, &signature)
            .map_err(|_| HandshakeError::InvalidSignature)?;

        // Derive shared secret
        let ephemeral_secret = self
            .ephemeral_secret
            .take()
            .ok_or(HandshakeError::InvalidState)?;
        let their_ephemeral = X25519PublicKey::from(response.ephemeral_pub);
        let shared_secret = ephemeral_secret.diffie_hellman(&their_ephemeral);

        // Extend transcript
        self.transcript.extend_from_slice(&response.sender_pub);
        self.transcript.extend_from_slice(&response.ephemeral_pub);
        self.transcript.extend_from_slice(&response.nonce);

        // Derive session key via HKDF (same salt construction as responder)
        // The init nonce is the first 32 bytes of our existing transcript after sender_pub + ephemeral_pub
        // We stored: init.sender_pub(32) + init.ephemeral_pub(32) + init.nonce(32) = bytes 64..96
        let init_nonce = &self.transcript[64..96];
        let mut salt = Vec::with_capacity(64);
        salt.extend_from_slice(init_nonce);
        salt.extend_from_slice(&response.nonce);

        let hk = Hkdf::<Sha256>::new(Some(&salt), shared_secret.as_bytes());
        let mut key = [0u8; 32];
        hk.expand(b"pim-session-v1", &mut key)
            .expect("32 bytes is valid for HKDF-SHA256");

        self.session_key = Some(SessionKey { key });
        Ok(())
    }

    /// Produce a confirm message (HMAC of transcript with session key).
    pub fn make_confirm(&self) -> Result<HandshakeConfirm, HandshakeError> {
        let session_key = self
            .session_key
            .as_ref()
            .ok_or(HandshakeError::InvalidState)?;
        let mut mac =
            HmacSha256::new_from_slice(session_key.as_bytes()).expect("HMAC accepts any key size");
        mac.update(&self.transcript);
        let result = mac.finalize().into_bytes();
        let mut hmac = [0u8; 32];
        hmac.copy_from_slice(&result);
        Ok(HandshakeConfirm { hmac })
    }

    /// Verify a confirm message from the peer.
    pub fn verify_confirm(&self, confirm: &HandshakeConfirm) -> Result<(), HandshakeError> {
        let session_key = self
            .session_key
            .as_ref()
            .ok_or(HandshakeError::InvalidState)?;
        let mut mac =
            HmacSha256::new_from_slice(session_key.as_bytes()).expect("HMAC accepts any key size");
        mac.update(&self.transcript);
        mac.verify_slice(&confirm.hmac)
            .map_err(|_| HandshakeError::ConfirmMismatch)
    }

    /// Get the derived session key (only available after finalization).
    pub fn session_key(&self) -> Option<&SessionKey> {
        self.session_key.as_ref()
    }
}

#[derive(Debug, thiserror::Error)]
/// Errors returned when the authenticated handshake fails.
pub enum HandshakeError {
    /// The peer supplied an invalid Ed25519 public key.
    #[error("invalid public key")]
    InvalidPublicKey,
    /// The peer signature on handshake material did not verify.
    #[error("invalid signature")]
    InvalidSignature,
    /// The transcript confirmation MAC did not match.
    #[error("handshake confirm mismatch")]
    ConfirmMismatch,
    /// The handshake API was called in an invalid order.
    #[error("invalid handshake state")]
    InvalidState,
}

#[cfg(test)]
mod tests;
