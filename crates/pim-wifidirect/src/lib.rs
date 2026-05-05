//! Wi-Fi Direct (IEEE 802.11 P2P) peer discovery and group formation for PIM.
//!
//! # Overview
//!
//! This crate adds an optional transport-finding layer on top of the existing
//! `TcpTransport`.  When enabled it:
//!
//! 1. Drives `wpa_supplicant`'s P2P subsystem via [`wpa_cli`] subprocesses.
//! 2. Discovers nearby Wi-Fi Direct-capable devices.
//! 3. Negotiates a P2P group (GO/GC role assignment + DHCP).
//! 4. Emits the resulting peer `SocketAddr` on a channel so the daemon can call
//!    `initiate_peer_connection` — the same path used by UDP broadcast discovery.
//!
//! # Design
//!
//! Wi-Fi Direct integration is a **peer-finding** layer, not a new transport.
//! Once a P2P group interface is up and we have a peer IP, the existing
//! `TcpTransport` handles all framing, handshake, and sessions unchanged.
//!
//! TCP/LAN and Wi-Fi Direct discovery run in parallel and are fully additive.

#![warn(missing_docs)]

#[cfg(target_os = "macos")]
mod bonjour;
pub mod group;
pub mod wpa_cli;

use std::net::SocketAddr;

#[cfg(not(target_os = "macos"))]
use std::{collections::HashSet, net::IpAddr, time::Duration};

use pim_core::WifiDirectConfig;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
#[cfg(not(target_os = "macos"))]
use tracing::debug;
use tracing::{info, warn};

pub use group::{GroupRole, WifiDirectGroup, GO_INTERFACE_IP};
pub use wpa_cli::{P2pPeerInfo, WpaCliController};

/// Errors produced by the Wi-Fi Direct subsystem.
#[derive(Debug, thiserror::Error)]
pub enum WifiDirectError {
    /// A `wpa_cli` invocation failed or returned an unexpected response.
    #[error("wpa_cli error: {0}")]
    WpaCli(String),
    /// A Bonjour / DNS-SD operation failed on macOS.
    #[error("bonjour error: {0}")]
    Bonjour(String),
    /// P2P group formation did not complete within the expected time.
    #[error("group formation failed: {0}")]
    GroupFormation(String),
    /// An I/O error occurred while reading network state.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Per-peer lifecycle event surfaced by [`WifiDirectDiscovery`] when
/// a caller has registered a sender via
/// [`WifiDirectDiscovery::set_peer_event_tx`]. Used by the daemon's
/// peer-cleanup subsystem to maintain the `wfd_peer_lifecycle`
/// freshness table, which the WFD cleanup tracker ages out (and
/// optionally drops via `wpa_cli p2p_remove_client` when the
/// destructive action is wired in).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WfdPeerEvent {
    /// Wi-Fi Direct peer MAC (lowercase, colon-separated).
    pub mac: String,
    /// What just happened.
    pub kind: WfdPeerEventKind,
}

/// Discriminator for [`WfdPeerEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WfdPeerEventKind {
    /// `p2p_peers` reported the MAC. Fired before any connection is
    /// attempted; the cleanup table uses this as the initial
    /// `first_seen_at_s` stamp.
    Discovered,
    /// Successful P2P group formation with this peer (peer IP
    /// resolved, address emitted on the canonical `peer_tx`
    /// channel). Bumps `last_connected_at_s`.
    Connected,
}

/// Drives Wi-Fi Direct peer discovery and group formation.
///
/// Constructed via [`WifiDirectDiscovery::new`], which also returns a
/// `mpsc::Receiver<SocketAddr>` that emits one address per successfully formed
/// P2P group.  The address is `peer_ip:listen_port` — ready to be passed
/// directly to `initiate_peer_connection`.
pub struct WifiDirectDiscovery {
    #[cfg(target_os = "macos")]
    node_name: String,
    #[cfg(not(target_os = "macos"))]
    ctrl: WpaCliController,
    config: WifiDirectConfig,
    listen_port: u16,
    peer_tx: mpsc::Sender<SocketAddr>,
    /// Optional sink for per-peer discovery/connection events — see
    /// [`WfdPeerEvent`]. Set via
    /// [`WifiDirectDiscovery::set_peer_event_tx`] before the service
    /// is started; if `None`, the discovery loop never emits per-
    /// peer events and the existing `peer_tx` flow is unaffected.
    peer_event_tx: Option<mpsc::Sender<WfdPeerEvent>>,
}

#[cfg(target_os = "macos")]
struct WifiDirectServiceType;

#[cfg(target_os = "macos")]
impl WifiDirectServiceType {
    const REG_TYPE: &'static str = "_pimmesh._tcp";
}

impl WifiDirectDiscovery {
    /// Create a new discovery service.
    ///
    /// Returns `(service, receiver)`.  Call [`WifiDirectDiscovery::run`] to start
    /// background operation; receive discovered peer addresses from the receiver.
    pub fn new(
        node_name: impl Into<String>,
        config: WifiDirectConfig,
        listen_port: u16,
    ) -> (Self, mpsc::Receiver<SocketAddr>) {
        #[cfg(not(target_os = "macos"))]
        let _ = node_name;

        let (peer_tx, peer_rx) = mpsc::channel(16);
        (
            Self {
                #[cfg(target_os = "macos")]
                node_name: node_name.into(),
                #[cfg(not(target_os = "macos"))]
                ctrl: WpaCliController::new(&config.interface),
                config,
                listen_port,
                peer_tx,
                peer_event_tx: None,
            },
            peer_rx,
        )
    }

    /// Register a sink for per-peer discovery / connection events —
    /// see [`WfdPeerEvent`]. Caller must invoke this before
    /// [`WifiDirectDiscovery::run`]; registering after the loop has
    /// started is a no-op for already-emitted events.
    pub fn set_peer_event_tx(&mut self, tx: mpsc::Sender<WfdPeerEvent>) {
        self.peer_event_tx = Some(tx);
    }

    /// Best-effort emission of a per-peer event. Silently dropped
    /// when no sink is registered; non-blocking so a slow consumer
    /// can never stall the discovery loop.
    fn emit_peer_event(&self, mac: &str, kind: WfdPeerEventKind) {
        let Some(tx) = self.peer_event_tx.as_ref() else {
            return;
        };
        let event = WfdPeerEvent {
            mac: mac.to_string(),
            kind,
        };
        if let Err(err) = tx.try_send(event) {
            tracing::debug!(%mac, ?kind, "WfdPeerEvent send failed: {err}");
        }
    }

    /// Run the discovery loop until `cancel` fires.
    ///
    /// Periodically:
    /// 1. Issues `p2p_find` to keep background scanning active.
    /// 2. Polls `p2p_peers` every 2 s for newly discovered devices.
    /// 3. For each unseen MAC, attempts `p2p_connect` (PBC or PIN).
    /// 4. Polls `list_interfaces` for a new `p2p-*` interface (up to 15 s).
    /// 5. Resolves peer IP via [`WifiDirectGroup::from_iface`].
    /// 6. Sends `SocketAddr` on the peer channel.
    pub async fn run(self, cancel: CancellationToken) {
        // Phase A note: on android this falls into the
        // `cfg(not(target_os = "macos"))` arm below, which immediately
        // logs a warn and returns when `wpa_cli p2p_find` fails (the
        // binary doesn't exist on android). Phase B replaces the linux
        // wpa_cli backend with a JNI bridge to Java
        // `WifiP2pManager`, at which point android gets a real path.
        #[cfg(target_os = "macos")]
        {
            if self.config.interface != WifiDirectConfig::default().interface
                || self.config.go_intent != WifiDirectConfig::default().go_intent
                || self.config.listen_channel != WifiDirectConfig::default().listen_channel
                || self.config.op_channel != WifiDirectConfig::default().op_channel
                || self.config.connect_method != WifiDirectConfig::default().connect_method
            {
                info!(
                    interface = %self.config.interface,
                    go_intent = self.config.go_intent,
                    listen_channel = self.config.listen_channel,
                    op_channel = self.config.op_channel,
                    connect_method = %self.config.connect_method,
                    "Wi-Fi Direct macOS backend uses Bonjour peer-to-peer discovery; Linux-specific P2P tuning fields are ignored"
                );
            }

            bonjour::run(self.node_name, self.listen_port, self.peer_tx, cancel).await;
            return;
        }

        #[cfg(not(target_os = "macos"))]
        {
            info!(
                "Wi-Fi Direct discovery starting on interface {}",
                self.config.interface
            );

            // Verify wpa_cli is available; log a warning and return early if not.
            if let Err(e) = self.ctrl.p2p_find().await {
                warn!("Wi-Fi Direct: p2p_find failed (wpa_supplicant not running?): {e}");
                return;
            }

            let mut seen_macs: HashSet<String> = HashSet::new();
            let mut poll_interval = tokio::time::interval(Duration::from_secs(2));

            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        debug!("Wi-Fi Direct discovery cancelled");
                        let _ = self.ctrl.p2p_stop_find().await;
                        return;
                    }
                    _ = poll_interval.tick() => {
                        self.poll_and_connect(&mut seen_macs).await;
                    }
                }
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    async fn poll_and_connect(&self, seen_macs: &mut HashSet<String>) {
        let peers = match self.ctrl.p2p_peers().await {
            Ok(p) => p,
            Err(e) => {
                debug!("Wi-Fi Direct: p2p_peers error: {e}");
                return;
            }
        };

        for mac in peers {
            if seen_macs.contains(&mac) {
                continue;
            }
            seen_macs.insert(mac.clone());
            info!("Wi-Fi Direct: new peer discovered: {mac}");
            // Per-peer Discovered event for the cleanup subsystem;
            // skipped silently when no sink is registered.
            self.emit_peer_event(&mac, WfdPeerEventKind::Discovered);

            let result = self.connect_and_emit(&mac).await;
            if let Err(e) = result {
                warn!("Wi-Fi Direct: connection to {mac} failed: {e}");
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    async fn connect_and_emit(&self, mac: &str) -> Result<(), WifiDirectError> {
        // Initiate connection based on configured method.
        if self.config.connect_method == "pbc" {
            self.ctrl.p2p_connect_pbc(mac).await?;
        } else if let Some(pin) = self.config.connect_method.strip_prefix("pin:") {
            self.ctrl.p2p_connect_pin(mac, pin).await?;
        } else {
            warn!(
                "Wi-Fi Direct: unknown connect_method '{}', using pbc",
                self.config.connect_method
            );
            self.ctrl.p2p_connect_pbc(mac).await?;
        }

        // Wait up to 15 s for a p2p-* interface to appear.
        let group_iface = self.wait_for_group_iface(Duration::from_secs(15)).await?;
        info!("Wi-Fi Direct: P2P group interface appeared: {group_iface}");

        // Determine role by reading own IP — GO gets 192.168.49.1.
        let group = WifiDirectGroup::from_iface(
            &self.ctrl,
            &group_iface,
            GroupRole::Gc,
            Duration::from_secs(10),
        )
        .await;

        let group = match group {
            Ok(g) => g,
            Err(e) => {
                return Err(e);
            }
        };

        // Refine role: if own_ip == GO_INTERFACE_IP we are actually the GO.
        let group = if group.own_ip == GO_INTERFACE_IP {
            WifiDirectGroup::from_iface(
                &self.ctrl,
                &group_iface,
                GroupRole::Go,
                Duration::from_secs(10),
            )
            .await?
        } else {
            group
        };

        let peer_ip = match group.peer_ip {
            Some(ip) => ip,
            None => {
                return Err(WifiDirectError::GroupFormation(
                    "peer IP not resolved".into(),
                ));
            }
        };

        let addr = SocketAddr::new(IpAddr::V4(peer_ip), self.listen_port);
        info!(
            "Wi-Fi Direct: group formed (role={:?}), peer addr={addr}",
            group.role
        );

        // Stop scanning while in a group to avoid interference.
        let _ = self.ctrl.p2p_stop_find().await;

        // Per-peer Connected event for the cleanup subsystem;
        // skipped silently when no sink is registered.
        self.emit_peer_event(mac, WfdPeerEventKind::Connected);

        let _ = self.peer_tx.send(addr).await;
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    async fn wait_for_group_iface(&self, timeout: Duration) -> Result<String, WifiDirectError> {
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            if let Ok(ifaces) = self.ctrl.list_interfaces().await {
                for iface in &ifaces {
                    if iface.starts_with("p2p-") {
                        return Ok(iface.clone());
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(WifiDirectError::GroupFormation(
                    "p2p-* interface did not appear within timeout".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

#[cfg(test)]
mod tests;
