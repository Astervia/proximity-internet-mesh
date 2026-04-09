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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);

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
        if path.exists() {
            Self::load(path)
        } else {
            let identity = Self::generate();
            identity.save(path)?;
            Ok(identity)
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
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn generate_produces_valid_identity() {
        let id = Identity::generate();
        // NodeId should be derived from the public key
        let expected = NodeId::from_public_key(&id.public_key_bytes());
        assert_eq!(id.node_id(), expected);
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("node.key");

        let original = Identity::generate();
        original.save(&path).unwrap();

        let loaded = Identity::load(&path).unwrap();
        assert_eq!(original.node_id(), loaded.node_id());
        assert_eq!(original.public_key_bytes(), loaded.public_key_bytes());
    }

    #[test]
    fn load_invalid_length_fails() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad.key");
        std::fs::write(&path, [0u8; 16]).unwrap();

        let result = Identity::load(&path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("32 bytes"));
    }

    #[test]
    fn load_nonexistent_fails() {
        let result = Identity::load(Path::new("/tmp/pim_nonexistent_key_12345"));
        assert!(result.is_err());
    }

    #[test]
    fn load_or_generate_creates_new() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("node.key");

        assert!(!path.exists());
        let id = Identity::load_or_generate(&path).unwrap();
        assert!(path.exists());

        // Loading again should give the same identity
        let id2 = Identity::load_or_generate(&path).unwrap();
        assert_eq!(id.node_id(), id2.node_id());
    }

    #[test]
    fn two_identities_are_different() {
        let a = Identity::generate();
        let b = Identity::generate();
        assert_ne!(a.node_id(), b.node_id());
        assert_ne!(a.public_key_bytes(), b.public_key_bytes());
    }
}
