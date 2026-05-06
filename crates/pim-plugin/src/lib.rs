//! Plugin extensibility for `pim-daemon`.
//!
//! A plugin runs in-process under the daemon's tokio runtime. The
//! daemon owns mesh-essential state (identity broadcast, routing,
//! the peer keystore); plugins consume those services through the
//! ports defined here and contribute:
//!
//! - Inbound [`ControlFrame::PluginPayload`] handling for the
//!   plugin's own `kind` namespace.
//! - Optional reactions to peer-state changes — currently
//!   [`DaemonPlugin::on_peer_forgotten`].
//!
//! JSON-RPC method registration is intentionally NOT routed through
//! this trait: methods are wired into the daemon's RPC dispatcher at
//! compile time behind a Cargo feature, so the daemon can be built
//! entirely without a given plugin.

#![warn(missing_docs)]

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use pim_core::NodeId;
use pim_protocol::ControlFrame;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Origin of an inbound `PeerInfo` frame.
///
/// `Direct` means it arrived on a session we have a Noise handshake
/// with; `Routed` means it arrived as a multi-hop control payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerInfoSource {
    /// Direct neighbour over an existing session.
    Direct,
    /// Routed via the multi-hop control plane (mesh broadcast).
    Routed,
}

/// Event published by the daemon's [`PeerDirectory`] when its peer
/// keystore mutates. Plugins subscribe to react to identity changes
/// without polling.
#[derive(Debug, Clone)]
pub enum PeerDirectoryEvent {
    /// A peer's identity was learned or refreshed.
    Seen {
        /// Stable cryptographic identifier.
        node_id: NodeId,
        /// X25519 public key derived from the peer's signing key.
        x25519_pub: [u8; 32],
        /// Latest friendly name advertised by the peer (may be empty).
        name: String,
        /// Whether the identity arrived directly or through routing.
        via: PeerInfoSource,
    },
    /// A peer was forgotten via `peers.forget`.
    Forgotten {
        /// Forgotten peer.
        node_id: NodeId,
    },
}

/// Read-mostly access to the daemon's keystore of known peers.
///
/// The implementation lives in `pim-daemon`; plugins receive it as
/// `Arc<dyn PeerDirectory>` through [`PluginContext`].
#[async_trait]
pub trait PeerDirectory: Send + Sync {
    /// Latest cached X25519 public key for `peer`, if known.
    async fn lookup_x25519(&self, peer: &NodeId) -> Option<[u8; 32]>;

    /// Latest friendly name observed for `peer`, if known.
    async fn lookup_name(&self, peer: &NodeId) -> Option<String>;

    /// Subscribe to identity-state events. Each subscriber gets every
    /// event emitted after the moment of subscription.
    fn subscribe(&self) -> broadcast::Receiver<PeerDirectoryEvent>;
}

/// Send a [`ControlFrame`] toward a peer, either over a direct
/// connected session or via the multi-hop routing table.
#[async_trait]
pub trait ControlSender: Send + Sync {
    /// Send to a directly-connected peer. Best-effort; failures are
    /// logged by the underlying transport.
    async fn send_direct(&self, peer: NodeId, frame: ControlFrame);

    /// Send via the routing table. Returns `true` when the next-hop
    /// send was attempted; `false` when no route exists.
    async fn send_routed(&self, dst_id: NodeId, frame: ControlFrame) -> bool;
}

/// Local identity material a plugin may need (typically for ECIES
/// decrypt of messages addressed to us).
pub trait IdentitySecrets: Send + Sync {
    /// Raw bytes of our Ed25519 signing key. Plugins that need an
    /// X25519 secret derive it deterministically from this seed
    /// (see `pim_crypto::e2e_decrypt_in_place`, etc.).
    fn signing_seed(&self) -> [u8; 32];
}

/// Snapshot of services + scratch space handed to a plugin at startup.
#[derive(Clone)]
pub struct PluginContext {
    /// Read-side access to the peer keystore.
    pub peers: Arc<dyn PeerDirectory>,
    /// Outbound control-frame sender.
    pub control: Arc<dyn ControlSender>,
    /// Local identity (for ECIES decrypt etc.).
    pub identity: Arc<dyn IdentitySecrets>,
    /// Daemon data directory; plugins place their own files
    /// (databases, snapshots) under it.
    pub data_dir: PathBuf,
    /// Daemon-wide cancellation token. Plugins should tie any
    /// long-running tasks to this so a clean shutdown propagates.
    pub cancel: CancellationToken,
}

/// In-process plugin contract.
///
/// One instance per plugin per daemon. The daemon takes
/// `Arc<dyn DaemonPlugin>`, calls [`Self::start`] once, then routes
/// inbound payloads matching [`Self::payload_kinds`] through
/// [`Self::handle_payload`]. On clean shutdown, [`Self::shutdown`] is
/// called once.
#[async_trait]
pub trait DaemonPlugin: Send + Sync + 'static {
    /// Stable identifier — used for log spans and (by convention) as a
    /// prefix for the plugin's payload kinds.
    fn name(&self) -> &'static str;

    /// Stable list of `PluginPayload.kind` values this plugin claims.
    /// The daemon dispatches inbound payloads by matching `kind`
    /// against each registered plugin in order; the first match wins.
    fn payload_kinds(&self) -> &'static [&'static str];

    /// Handle an inbound [`ControlFrame::PluginPayload`] whose `kind`
    /// appeared in [`Self::payload_kinds`].
    async fn handle_payload(&self, src: NodeId, kind: &str, body: Bytes);

    /// Spawn whatever long-running tasks the plugin needs. Returns
    /// once startup is complete; long-running work should be on
    /// detached `tokio::spawn` handles tied to `ctx.cancel`.
    async fn start(self: Arc<Self>, ctx: PluginContext) -> anyhow::Result<()>;

    /// Notification: the daemon dropped this peer's identity from the
    /// keystore (e.g. via `peers.forget`). Plugins typically wipe any
    /// per-peer state of their own here. Failure is logged but does
    /// not block other plugins from being notified.
    async fn on_peer_forgotten(&self, peer: NodeId);

    /// Best-effort shutdown — called once at daemon teardown.
    async fn shutdown(&self);
}
