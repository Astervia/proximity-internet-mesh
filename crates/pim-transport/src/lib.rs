mod tcp;

use async_trait::async_trait;
use pim_core::NodeId;
use pim_protocol::TransportFrame;

pub use tcp::TcpTransport;

/// Errors specific to the transport layer.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("peer not connected: {0}")]
    PeerNotConnected(NodeId),

    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    #[error("send failed: {0}")]
    SendFailed(String),

    #[error("receive failed: {0}")]
    ReceiveFailed(String),

    #[error("transport shut down")]
    Shutdown,

    #[error("protocol error: {0}")]
    Protocol(#[from] pim_core::PimError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Address for connecting to a peer.
#[derive(Debug, Clone)]
pub struct PeerAddress {
    /// The peer's node ID (learned during handshake or configured).
    pub node_id: NodeId,
    /// Socket address to connect to.
    pub addr: std::net::SocketAddr,
}

/// Abstracted transport layer for sending/receiving frames between peers.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Send a frame to a specific peer.
    async fn send(&self, peer: &NodeId, frame: TransportFrame) -> Result<(), TransportError>;

    /// Receive the next frame from any connected peer.
    /// Returns the sender's NodeId and the frame.
    async fn recv(&self) -> Result<(NodeId, TransportFrame), TransportError>;

    /// Establish a connection to a peer at the given address.
    async fn connect(&self, peer: &PeerAddress) -> Result<(), TransportError>;

    /// Disconnect from a peer.
    async fn disconnect(&self, peer: &NodeId) -> Result<(), TransportError>;

    /// List currently connected peers.
    fn connected_peers(&self) -> Vec<NodeId>;
}
