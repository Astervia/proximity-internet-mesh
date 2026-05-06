//! Plugin-side adapters that bridge the daemon's internal state onto
//! the trait surface exposed by [`pim_plugin`].
//!
//! These are constructed once at startup and handed to each plugin via
//! [`pim_plugin::PluginContext`].

use std::sync::Arc;

use async_trait::async_trait;
use pim_core::NodeId;
use pim_crypto::Identity;
use pim_plugin::{ControlSender, IdentitySecrets};
use pim_protocol::ControlFrame;

use crate::app::ip_control::send_routed_control;
use crate::app::peer_tasks::send_control;
use crate::app::DaemonState;

/// `Arc<dyn ControlSender>` impl wrapping the daemon's transport
/// helpers.
pub(crate) struct ControlSenderAdapter {
    state: Arc<DaemonState>,
}

impl ControlSenderAdapter {
    pub fn new(state: Arc<DaemonState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl ControlSender for ControlSenderAdapter {
    async fn send_direct(&self, peer: NodeId, frame: ControlFrame) {
        send_control(&self.state, &peer, frame).await;
    }

    async fn send_routed(&self, dst_id: NodeId, frame: ControlFrame) -> bool {
        send_routed_control(&self.state, dst_id, frame).await
    }
}

/// `Arc<dyn IdentitySecrets>` impl exposing the local Ed25519 seed.
pub(crate) struct IdentitySecretsAdapter {
    identity: Arc<Identity>,
}

impl IdentitySecretsAdapter {
    pub fn new(identity: Arc<Identity>) -> Self {
        Self { identity }
    }
}

impl IdentitySecrets for IdentitySecretsAdapter {
    fn signing_seed(&self) -> [u8; 32] {
        self.identity.signing_key().to_bytes()
    }
}
