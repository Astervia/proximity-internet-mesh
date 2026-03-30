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

use crate::advertisement::{DiscoveryAdvertisement, NodeCapabilities, PACKET_SIZE};
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

    /// A shared reference to the peer table (for the daemon to query).
    pub fn peer_table(&self) -> Arc<Mutex<PeerTable>> {
        self.peer_table.clone()
    }

    /// Send one broadcast advertisement on `socket`.
    pub async fn broadcast_presence(&self, socket: &UdpSocket) -> Result<()> {
        let payload = self.own_ad.serialize();
        let dst = SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::BROADCAST,
            self.discovery_port,
        ));
        socket.send_to(&payload, dst).await.context("broadcast send")?;
        debug!(node_id = %self.own_ad.node_id, "sent discovery broadcast");
        Ok(())
    }

    /// Handle a raw UDP packet received from `from`.
    ///
    /// Returns the `PeerRecord` if the advertisement is valid and from a
    /// previously-unknown peer, otherwise `None`.
    pub async fn handle_advertisement(
        &self,
        data: &[u8],
        from: SocketAddr,
    ) -> Option<PeerRecord> {
        let ad = DiscoveryAdvertisement::deserialize(data)?;

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
        let bind_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, self.discovery_port));
        let socket = UdpSocket::bind(bind_addr)
            .await
            .context("failed to bind discovery UDP socket")?;
        socket.set_broadcast(true).context("SO_BROADCAST")?;

        info!(port = self.discovery_port, "discovery service started");

        let mut broadcast_interval = tokio::time::interval(self.broadcast_interval);
        let mut gc_interval = tokio::time::interval(self.peer_timeout / 2);
        let mut recv_buf = vec![0u8; PACKET_SIZE + 16];

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
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn make_service(self_id: u8, port: u16) -> (Arc<DiscoveryService>, mpsc::Receiver<PeerRecord>) {
        let id = NodeId::from_bytes([self_id; 16]);
        let (svc, rx) = DiscoveryService::new(
            id,
            [self_id; 32],
            NodeCapabilities::client(),
            port,
        );
        (Arc::new(svc.with_port(port)), rx)
    }

    fn peer_advertisement(node_n: u8, tcp_port: u16) -> DiscoveryAdvertisement {
        DiscoveryAdvertisement {
            node_id: NodeId::from_bytes([node_n; 16]),
            public_key: [node_n; 32],
            capabilities: NodeCapabilities::relay(),
            listen_port: tcp_port,
        }
    }

    #[tokio::test]
    async fn handle_advertisement_adds_new_peer() {
        let (svc, mut rx) = make_service(1, 9200);

        let ad = peer_advertisement(2, 9100);
        let from = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)), 9101);

        let record = svc.handle_advertisement(&ad.serialize(), from).await;
        assert!(record.is_some(), "new peer should be returned");
        assert_eq!(record.unwrap().node_id, NodeId::from_bytes([2; 16]));

        assert!(rx.try_recv().is_ok(), "new peer notification sent");
        assert_eq!(svc.peer_table.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn own_advertisement_is_ignored() {
        let (svc, mut rx) = make_service(1, 9201);

        // Simulate receiving our own broadcast
        let own_ad = svc.own_ad.serialize();
        let from = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9101);

        let record = svc.handle_advertisement(&own_ad, from).await;
        assert!(record.is_none());
        assert!(rx.try_recv().is_err(), "no notification for own packet");
        assert_eq!(svc.peer_table.lock().await.len(), 0);
    }

    #[tokio::test]
    async fn duplicate_advertisement_returns_none() {
        let (svc, _rx) = make_service(1, 9202);
        let ad = peer_advertisement(3, 9100);
        let from = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3)), 9101);

        svc.handle_advertisement(&ad.serialize(), from).await;
        let second = svc.handle_advertisement(&ad.serialize(), from).await;
        assert!(second.is_none(), "duplicate should return None");
        assert_eq!(svc.peer_table.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn capabilities_are_stored_correctly() {
        let (svc, _rx) = make_service(1, 9203);

        let ad = DiscoveryAdvertisement {
            node_id: NodeId::from_bytes([9; 16]),
            public_key: [9; 32],
            capabilities: NodeCapabilities::gateway(),
            listen_port: 9100,
        };
        let from = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9)), 9101);
        svc.handle_advertisement(&ad.serialize(), from).await;

        let table = svc.peer_table.lock().await;
        let record = table.get(&NodeId::from_bytes([9; 16])).unwrap();
        assert!(record.capabilities.is_gateway());
        assert!(record.capabilities.is_relay());
    }

    #[tokio::test]
    async fn invalid_packet_is_ignored() {
        let (svc, mut rx) = make_service(1, 9204);
        let from = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9101);

        let result = svc.handle_advertisement(b"garbage", from).await;
        assert!(result.is_none());
        assert!(rx.try_recv().is_err());
    }
}
