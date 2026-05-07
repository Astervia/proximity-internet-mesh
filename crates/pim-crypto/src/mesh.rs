//! Mesh-membership secret derivation.
//!
//! A "private mesh" is a group of nodes that share a passphrase. The
//! passphrase is stretched once at daemon startup via Argon2id and the
//! resulting 32-byte master is split via HKDF-SHA256 into three
//! purpose-bound sub-keys:
//!
//! * `discovery_key` — AES-256-GCM key for encrypting UDP/Wi-Fi-Direct
//!   discovery advertisements. Non-members can't decrypt the ad and
//!   never learn the mesh exists on the wire.
//! * `handshake_key` — mixed into the HKDF IKM of the per-session
//!   handshake (see [`crate::handshake`]). A peer without the right
//!   `handshake_key` derives a different session key, fails the
//!   transcript HMAC, and is rejected before any traffic flows.
//! * `fingerprint` — 8-byte display value (NOT a secret) used by the
//!   UI/CLI to identify the mesh; derived from the same master so two
//!   nodes that share the passphrase show the same fingerprint.
//!
//! Open mesh = absence of `MeshSecret`. Private mesh = `MeshSecret`
//! present and matching across all nodes.

use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Argon2id parameters used to stretch the passphrase.
///
/// Defaults aim for ~100 ms on a modern desktop and ~600 ms on a
/// Raspberry Pi 4. Cost is paid **once at daemon startup**, not per
/// handshake, so this can be conservative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshKdfParams {
    /// Memory cost in KiB. Default 65536 (64 MiB).
    pub m_cost_kib: u32,
    /// Number of iterations. Default 3.
    pub t_cost: u32,
    /// Parallelism. Default 1 (single-threaded; multi-threaded Argon2
    /// requires extra plumbing and the startup cost target is met
    /// without it).
    pub p_cost: u32,
}

impl Default for MeshKdfParams {
    fn default() -> Self {
        Self {
            m_cost_kib: 65536,
            t_cost: 3,
            p_cost: 1,
        }
    }
}

impl MeshKdfParams {
    fn to_argon2_params(self) -> Result<Params, MeshKdfError> {
        Params::new(self.m_cost_kib, self.t_cost, self.p_cost, Some(32))
            .map_err(|e| MeshKdfError::InvalidParams(format!("{e}")))
    }
}

/// Derived secrets for a private mesh.
///
/// Construct with [`MeshSecret::derive`]. The master Argon2id output
/// is consumed during construction and never exposed outside this
/// type.
#[derive(Clone)]
pub struct MeshSecret {
    discovery_key: [u8; 32],
    handshake_key: [u8; 32],
    fingerprint: [u8; 8],
}

impl MeshSecret {
    /// Derive a `MeshSecret` from a UTF-8 passphrase and an optional
    /// mesh label.
    ///
    /// `mesh_id` is mixed into the Argon2 salt to provide cryptographic
    /// separation between meshes that happen to pick the same
    /// passphrase ("home", "office"). Pass `None` (or `Some("")`) for
    /// no mesh label — both produce the same secret.
    pub fn derive(
        passphrase: &str,
        mesh_id: Option<&str>,
        params: MeshKdfParams,
    ) -> Result<Self, MeshKdfError> {
        if passphrase.is_empty() {
            return Err(MeshKdfError::EmptyPassphrase);
        }

        // Argon2id salt: fixed domain-separation prefix + mesh_id (if any).
        // The Argon2 crate enforces salt.len() >= 8, which the prefix satisfies.
        let mut salt = Vec::with_capacity(32);
        salt.extend_from_slice(b"pim-mesh-v1");
        if let Some(id) = mesh_id {
            salt.extend_from_slice(id.as_bytes());
        }

        let argon2 = Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            params.to_argon2_params()?,
        );
        let mut master = [0u8; 32];
        argon2
            .hash_password_into(passphrase.as_bytes(), &salt, &mut master)
            .map_err(|e| MeshKdfError::Argon2(format!("{e}")))?;

        // Split via HKDF-SHA256 into purpose-bound sub-keys.
        let hk = Hkdf::<Sha256>::new(None, &master);

        let mut discovery_key = [0u8; 32];
        let mut handshake_key = [0u8; 32];
        let mut fingerprint = [0u8; 8];
        hk.expand(b"pim-disco-v1", &mut discovery_key)
            .map_err(|e| MeshKdfError::Hkdf(format!("{e}")))?;
        hk.expand(b"pim-handshake-v1", &mut handshake_key)
            .map_err(|e| MeshKdfError::Hkdf(format!("{e}")))?;
        hk.expand(b"pim-fingerprint-v1", &mut fingerprint)
            .map_err(|e| MeshKdfError::Hkdf(format!("{e}")))?;

        // Master is no longer needed; let it drop on return.
        Ok(Self {
            discovery_key,
            handshake_key,
            fingerprint,
        })
    }

    /// AES-256-GCM key for encrypted discovery advertisements.
    pub fn discovery_key(&self) -> &[u8; 32] {
        &self.discovery_key
    }

    /// 32-byte secret mixed into the per-session handshake HKDF IKM.
    pub fn handshake_key(&self) -> &[u8; 32] {
        &self.handshake_key
    }

    /// 8-byte non-secret display value used by UI/CLI.
    pub fn fingerprint(&self) -> &[u8; 8] {
        &self.fingerprint
    }

    /// Lowercase hex of [`Self::fingerprint`], suitable for logging.
    pub fn fingerprint_hex(&self) -> String {
        let mut out = String::with_capacity(16);
        for b in self.fingerprint {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }
}

impl std::fmt::Debug for MeshSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never log key material — only the public fingerprint.
        f.debug_struct("MeshSecret")
            .field("fingerprint", &self.fingerprint_hex())
            .finish_non_exhaustive()
    }
}

/// Compute the RFCOMM Hello mesh-membership tag.
///
/// Used by the Bluetooth RFCOMM session-level handshake (Hello /
/// HelloAck) to prove that both ends share the mesh handshake key
/// **before** the RFCOMM channel is bridged onto loopback TCP. Without
/// this check, a paired device with a `PIM-*` name but no mesh
/// membership could force us to spin up a TCP-bridge handshake task
/// (Ed25519 work, 10s timeout) that's certain to fail.
///
/// The tag is `HMAC-SHA256(handshake_key, node_id_hex)`. It binds to
/// the *sender's* node id, so a captured tag from a real PIM peer is
/// only useful for replaying that specific NodeId — and the inner
/// Ed25519 + handshake-bound session key derivation in `pim-transport`
/// will reject the replay because the attacker can't sign with the
/// real node's private key.
///
/// Returns the raw 32-byte HMAC. Encode as lowercase hex on the wire
/// (the JSON-based RFCOMM Hello envelope can't carry binary bytes).
pub fn compute_rfcomm_hello_tag(handshake_key: &[u8; 32], node_id_hex: &str) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(handshake_key).expect("HMAC accepts any key size");
    mac.update(node_id_hex.as_bytes());
    let result = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

/// Errors returned during mesh-secret derivation.
#[derive(Debug, thiserror::Error)]
pub enum MeshKdfError {
    /// Passphrase string was empty.
    #[error("mesh passphrase must not be empty")]
    EmptyPassphrase,
    /// Argon2id parameters were rejected by the implementation.
    #[error("invalid argon2 parameters: {0}")]
    InvalidParams(String),
    /// Argon2id execution failed.
    #[error("argon2 failure: {0}")]
    Argon2(String),
    /// HKDF expansion failed (should be unreachable for our fixed sizes).
    #[error("hkdf failure: {0}")]
    Hkdf(String),
}

#[cfg(test)]
mod tests;
