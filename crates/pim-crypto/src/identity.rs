//! Persistent node identity backed by an Ed25519 signing key.

use std::path::Path;

use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;

use pim_core::NodeId;

/// A node's cryptographic identity: an Ed25519 keypair and derived NodeId.
#[derive(Debug)]
pub struct Identity {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
    node_id: NodeId,
}

impl Identity {
    /// Generate a new random identity.
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        Self::from_signing_key(signing_key)
    }

    /// Construct from an existing signing key.
    fn from_signing_key(signing_key: SigningKey) -> Self {
        let verifying_key = signing_key.verifying_key();
        let node_id = NodeId::from_public_key(&verifying_key.to_bytes());
        Self {
            signing_key,
            verifying_key,
            node_id,
        }
    }

    /// Save the private key to a file (raw 32-byte seed).
    pub fn save(&self, path: &Path) -> Result<(), std::io::Error> {
        self.save_internal(path, true)
    }

    /// Save the private key to a file, taking an overwrite parameter.
    fn save_internal(&self, path: &Path, overwrite: bool) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut options = std::fs::OpenOptions::new();
        options.write(true);
        if overwrite {
            options.create(true).truncate(true);
        } else {
            options.create_new(true);
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        use std::io::Write;
        let mut file = options.open(path)?;
        file.write_all(&self.signing_key.to_bytes())
    }

    /// Load an identity from a private key file.
    pub fn load(path: &Path) -> Result<Self, std::io::Error> {
        let bytes = std::fs::read(path)?;
        if bytes.len() != 32 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("key file must be 32 bytes, got {}", bytes.len()),
            ));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        let signing_key = SigningKey::from_bytes(&seed);
        Ok(Self::from_signing_key(signing_key))
    }

    /// Load an existing identity or generate a new one.
    pub fn load_or_generate(path: &Path) -> Result<Self, std::io::Error> {
        match Self::load(path) {
            Ok(identity) => Ok(identity),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let identity = Self::generate();
                match identity.save_internal(path, false) {
                    Ok(()) => Ok(identity),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        // Another process created it between our load and save attempts
                        Self::load(path)
                    }
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Return the stable node identifier derived from the public key.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Return the Ed25519 signing key.
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Return the Ed25519 verifying key.
    pub fn verifying_key(&self) -> &VerifyingKey {
        &self.verifying_key
    }

    /// Return the raw 32-byte public key.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.verifying_key.to_bytes()
    }
}

#[cfg(test)]
mod tests;
