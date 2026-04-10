//! Cryptographic primitives used by the mesh handshake, session, and E2E flows.

#![warn(missing_docs)]

pub mod e2e;
pub mod handshake;
pub mod identity;
pub mod session;

pub use e2e::{e2e_decrypt, e2e_decrypt_in_place, e2e_encrypt, x25519_from_seed, x25519_public_from_seed, E2eError};
pub use handshake::{
    HandshakeConfirm, HandshakeError, HandshakeInit, HandshakeResponse, Handshaker, SessionKey,
};
pub use identity::Identity;
pub use session::{EncryptedFrame, SessionCipher, SessionError};
