//! Discovery service: UDP broadcast / receive loop.
//!
//! Each node binds to `0.0.0.0:<discovery_port>` and enables `SO_BROADCAST`.
//! Advertisements are sent to `255.255.255.255:<discovery_port>` at a
//! configurable interval so that any node on the same LAN can hear them.
//!
//! On receiving a valid advertisement from a previously-unknown peer, the
//! service adds the peer to the [`PeerTable`] and notifies the daemon via a
//! `mpsc` channel so it can initiate a transport connection + handshake.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use pim_core::NodeId;

use crate::advertisement::{
    DiscoveryAdvertisement, NodeCapabilities, ENCRYPTED_PACKET_SIZE, PACKET_SIZE,
};
use crate::peer_table::{PeerRecord, PeerTable};

/// Default UDP port used for discovery broadcasts.
pub const DEFAULT_DISCOVERY_PORT: u16 = 9101;

/// Interval between presence broadcasts.
pub const DEFAULT_BROADCAST_INTERVAL: Duration = Duration::from_secs(5);

/// How long a peer can be unheard before it is expired from the table.
pub const DEFAULT_PEER_TIMEOUT: Duration = Duration::from_secs(30);

// ── DiscoveryService ──────────────────────────────────────────────────────────

/// Sends presence broadcasts and receives advertisements from other nodes.
///
/// Create one per daemon process, then call [`DiscoveryService::run`] as a
/// background task.  New-peer notifications arrive via the channel returned by
/// [`DiscoveryService::new_peer_rx`].
pub struct DiscoveryService {
    /// Our own advertisement, re-broadcast every interval.
    own_ad: DiscoveryAdvertisement,
    discovery_port: u16,
    broadcast_interval: Duration,
    peer_timeout: Duration,
    discovery_key: Option<[u8; 32]>,
    peer_table: Arc<Mutex<PeerTable>>,
    /// Sender for notifying the daemon when a new peer is discovered.
    new_peer_tx: mpsc::Sender<PeerRecord>,
}

impl DiscoveryService {
    /// Create a new `DiscoveryService`.
    ///
    /// * `self_id` / `public_key` / `capabilities` — our node's identity.
    /// * `listen_port` — the TCP port our transport is listening on (advertised
    ///   to peers so they can connect).
    /// * Returns the service and a receiver for new-peer events.
    pub fn new(
        self_id: NodeId,
        public_key: [u8; 32],
        capabilities: NodeCapabilities,
        listen_port: u16,
    ) -> (Self, mpsc::Receiver<PeerRecord>) {
        let (new_peer_tx, new_peer_rx) = mpsc::channel(64);
        let service = Self {
            own_ad: DiscoveryAdvertisement {
                node_id: self_id,
                public_key,
                capabilities,
                listen_port,
            },
            discovery_port: DEFAULT_DISCOVERY_PORT,
            broadcast_interval: DEFAULT_BROADCAST_INTERVAL,
            peer_timeout: DEFAULT_PEER_TIMEOUT,
            discovery_key: None,
            peer_table: Arc::new(Mutex::new(PeerTable::new())),
            new_peer_tx,
        };
        (service, new_peer_rx)
    }

    /// Override the UDP discovery port (default: [`DEFAULT_DISCOVERY_PORT`]).
    pub fn with_port(mut self, port: u16) -> Self {
        self.discovery_port = port;
        self
    }

    /// Override the broadcast interval.
    pub fn with_broadcast_interval(mut self, interval: Duration) -> Self {
        self.broadcast_interval = interval;
        self
    }

    /// Override the peer timeout.
    pub fn with_peer_timeout(mut self, timeout: Duration) -> Self {
        self.peer_timeout = timeout;
        self
    }

    /// Configure the AES-256-GCM key used to encrypt discovery
    /// advertisements. Sourced from
    /// [`pim_crypto::MeshSecret::discovery_key`] when the daemon is
    /// configured for a private mesh; absent for open meshes (plaintext
    /// advertisements).
    pub fn with_discovery_key(mut self, key: [u8; 32]) -> Self {
        self.discovery_key = Some(key);
        self
    }

    /// A shared reference to the peer table (for the daemon to query).
    pub fn peer_table(&self) -> Arc<Mutex<PeerTable>> {
        self.peer_table.clone()
    }

    /// Send one broadcast advertisement on `socket`.
    pub async fn broadcast_presence(&self, socket: &UdpSocket) -> Result<()> {
        let payload = match self.discovery_key {
            Some(key) => self.own_ad.serialize_encrypted(&key).to_vec(),
            None => self.own_ad.serialize().to_vec(),
        };
        let dst = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::BROADCAST, self.discovery_port));
        socket
            .send_to(&payload, dst)
            .await
            .context("broadcast send")?;
        debug!(node_id = %self.own_ad.node_id, "sent discovery broadcast");
        Ok(())
    }

    /// Handle a raw UDP packet received from `from`.
    ///
    /// Returns the `PeerRecord` if the advertisement is valid and from a
    /// previously-unknown peer, otherwise `None`.
    pub async fn handle_advertisement(&self, data: &[u8], from: SocketAddr) -> Option<PeerRecord> {
        let ad = match self.discovery_key {
            Some(key) => DiscoveryAdvertisement::deserialize_encrypted(data, &key)?,
            None => DiscoveryAdvertisement::deserialize(data)?,
        };

        // Ignore our own broadcasts
        if ad.node_id == self.own_ad.node_id {
            return None;
        }

        let peer_addr = SocketAddr::new(from.ip(), ad.listen_port);
        let record = PeerRecord {
            node_id: ad.node_id,
            public_key: ad.public_key,
            capabilities: ad.capabilities,
            listen_addr: peer_addr,
            last_seen: std::time::Instant::now(),
        };

        let is_new = self.peer_table.lock().await.upsert(record.clone());
        if is_new {
            info!(%ad.node_id, addr = %peer_addr, "discovered new peer");
            self.new_peer_tx.send(record.clone()).await.ok();
            Some(record)
        } else {
            debug!(%ad.node_id, "refreshed known peer");
            None
        }
    }

    /// Run the discovery service until `cancel` is triggered.
    ///
    /// Binds a UDP socket on `0.0.0.0:<discovery_port>`, broadcasts presence,
    /// and processes incoming advertisements.
    pub async fn run(self: Arc<Self>, cancel: CancellationToken) -> Result<()> {
        let bind_addr = SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::UNSPECIFIED,
            self.discovery_port,
        ));
        let socket = UdpSocket::bind(bind_addr)
            .await
            .context("failed to bind discovery UDP socket")?;
        socket.set_broadcast(true).context("SO_BROADCAST")?;

        info!(port = self.discovery_port, "discovery service started");

        let mut broadcast_interval = tokio::time::interval(self.broadcast_interval);
        let mut gc_interval = tokio::time::interval(self.peer_timeout / 2);
        let mut recv_buf = vec![0u8; ENCRYPTED_PACKET_SIZE.max(PACKET_SIZE + 16)];

        loop {
            tokio::select! {
                _ = broadcast_interval.tick() => {
                    if let Err(e) = self.broadcast_presence(&socket).await {
                        warn!("broadcast failed: {e}");
                    }
                }

                res = socket.recv_from(&mut recv_buf) => {
                    match res {
                        Ok((n, from)) => {
                            self.handle_advertisement(&recv_buf[..n], from).await;
                        }
                        Err(e) => warn!("UDP recv error: {e}"),
                    }
                }

                _ = gc_interval.tick() => {
                    let timeout = self.peer_timeout;
                    let removed = self.peer_table.lock().await.expire_stale(timeout);
                    for id in removed {
                        debug!(%id, "peer expired from discovery table");
                    }
                }

                _ = cancel.cancelled() => {
                    info!("discovery service stopping");
                    break;
                }
            }
        }
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
