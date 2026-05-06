//! [`pim_plugin::DaemonPlugin`] implementation that delegates to
//! [`crate::service::MessagingService`].

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use pim_core::NodeId;
use pim_plugin::{DaemonPlugin, PluginContext};

use crate::service::MessagingService;
use crate::wire::{KIND_ACK, KIND_MESSAGE};

const KINDS: &[&str] = &[KIND_MESSAGE, KIND_ACK];

/// Daemon plugin shell.
///
/// The daemon constructs the [`MessagingService`] first (since its RPC
/// layer needs a direct handle to call `send`, `mark_*`, history, etc.)
/// and then wraps it with this plugin so payload dispatch and lifecycle
/// hooks can flow through the same instance.
pub struct MessagingPlugin {
    service: Arc<MessagingService>,
}

impl MessagingPlugin {
    /// Wrap an existing [`MessagingService`] as a [`DaemonPlugin`].
    pub fn new(service: Arc<MessagingService>) -> Arc<Self> {
        Arc::new(Self { service })
    }
}

#[async_trait]
impl DaemonPlugin for MessagingPlugin {
    fn name(&self) -> &'static str {
        "messaging"
    }

    fn payload_kinds(&self) -> &'static [&'static str] {
        KINDS
    }

    async fn handle_payload(&self, src: NodeId, kind: &str, body: Bytes) {
        match kind {
            KIND_MESSAGE => self.service.handle_incoming_message(src, body).await,
            KIND_ACK => self.service.handle_incoming_message_ack(src, body).await,
            _ => {}
        }
    }

    async fn start(self: Arc<Self>, _ctx: PluginContext) -> anyhow::Result<()> {
        // No background tasks: the plugin reacts to inbound payloads
        // and to RPC calls dispatched into `MessagingService` directly.
        Ok(())
    }

    async fn on_peer_forgotten(&self, peer: NodeId) {
        self.service.on_peer_forgotten(peer).await;
    }

    async fn shutdown(&self) {}
}
