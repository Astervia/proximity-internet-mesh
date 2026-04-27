use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use pim_core::{AuthorizationPolicy, NodeId};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::fs_util::atomic_write;

pub(crate) fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(format!("{}{}", home, &s[1..]));
        }
    }
    path.to_path_buf()
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct TrustedPeersFile {
    #[serde(default)]
    peers: Vec<NodeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthorizationDecision {
    Allowed,
    TrustedOnFirstUse,
    Rejected,
}

pub(crate) struct AuthorizationManager {
    policy: AuthorizationPolicy,
    allow_list: HashSet<NodeId>,
    trusted_peers: RwLock<HashSet<NodeId>>,
    trust_store_file: PathBuf,
}

impl AuthorizationManager {
    pub(crate) fn new(
        policy: AuthorizationPolicy,
        allow_list: impl IntoIterator<Item = NodeId>,
        trust_store_file: PathBuf,
    ) -> Result<Self> {
        let trust_store_file = expand_tilde(&trust_store_file);
        let trusted_peers = if policy == AuthorizationPolicy::TrustOnFirstUse {
            load_trusted_peers(&trust_store_file)?
        } else {
            HashSet::new()
        };
        Ok(Self {
            policy,
            allow_list: allow_list.into_iter().collect(),
            trusted_peers: RwLock::new(trusted_peers),
            trust_store_file,
        })
    }

    pub(crate) async fn authorize_discovered_peer(&self, peer_id: NodeId) -> bool {
        match self.policy {
            AuthorizationPolicy::AllowAll | AuthorizationPolicy::TrustOnFirstUse => true,
            AuthorizationPolicy::AllowList => self.allow_list.contains(&peer_id),
        }
    }

    pub(crate) async fn authorize_authenticated_peer(
        &self,
        peer_id: NodeId,
    ) -> Result<AuthorizationDecision> {
        match self.policy {
            AuthorizationPolicy::AllowAll => Ok(AuthorizationDecision::Allowed),
            AuthorizationPolicy::AllowList => Ok(if self.allow_list.contains(&peer_id) {
                AuthorizationDecision::Allowed
            } else {
                AuthorizationDecision::Rejected
            }),
            AuthorizationPolicy::TrustOnFirstUse => {
                {
                    let trusted = self.trusted_peers.read().await;
                    if trusted.contains(&peer_id) {
                        return Ok(AuthorizationDecision::Allowed);
                    }
                }

                let mut trusted = self.trusted_peers.write().await;
                if trusted.contains(&peer_id) {
                    return Ok(AuthorizationDecision::Allowed);
                }
                trusted.insert(peer_id);
                if let Err(err) = persist_trusted_peers(&self.trust_store_file, &trusted).await {
                    trusted.remove(&peer_id);
                    return Err(err).context("persist TOFU trust store");
                }
                Ok(AuthorizationDecision::TrustedOnFirstUse)
            }
        }
    }
}

fn load_trusted_peers(path: &PathBuf) -> Result<HashSet<NodeId>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(e) => return Err(e).with_context(|| format!("read trust store {}", path.display())),
    };
    let file: TrustedPeersFile = toml::from_str(&content)
        .with_context(|| format!("parse trust store {}", path.display()))?;
    Ok(file.peers.into_iter().collect())
}

async fn persist_trusted_peers(path: &Path, peers: &HashSet<NodeId>) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create trust store dir {}", parent.display()))?;
    }
    let mut ordered: Vec<NodeId> = peers.iter().copied().collect();
    ordered.sort_by_key(|peer| peer.to_hex());
    let content = toml::to_string_pretty(&TrustedPeersFile { peers: ordered })
        .context("serialize trust store")?;
    atomic_write(
        path.to_str()
            .with_context(|| format!("non-utf8 trust store path {}", path.display()))?,
        content.as_bytes(),
    )
    .await
    .with_context(|| format!("write trust store {}", path.display()))
}

pub(crate) fn parse_discovery_shared_key(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64 {
        bail!("discovery.shared_key must be 64 hex characters");
    }
    let mut key = [0u8; 32];
    for (idx, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let hex = std::str::from_utf8(chunk).context("discovery.shared_key must be valid UTF-8")?;
        key[idx] = u8::from_str_radix(hex, 16)
            .with_context(|| format!("invalid discovery key byte {hex}"))?;
    }
    Ok(key)
}
