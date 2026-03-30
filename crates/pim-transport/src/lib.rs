//! Transport abstractions and the default TCP transport implementation.

#![warn(missing_docs)]

mod tcp;

use async_trait::async_trait;
use pim_core::NodeId;
use pim_protocol::TransportFrame;

pub use tcp::TcpTransport;

/// Errors specific to the transport layer.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// Attempted to address a peer that is not currently connected.
    #[error("peer not connected: {0}")]
    PeerNotConnected(NodeId),

    /// Opening a transport connection failed.
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    /// Writing a frame to a peer failed.
    #[error("send failed: {0}")]
    SendFailed(String),

    /// Peer write queue is saturated and cannot accept another frame immediately.
    #[error("peer send queue full (congested): {0}")]
    Congested(NodeId),

    /// Receiving or decoding an inbound frame failed.
    #[error("receive failed: {0}")]
    ReceiveFailed(String),

    /// Transport has been shut down and cannot continue operating.
    #[error("transport shut down")]
    Shutdown,

    /// Inner protocol codec failed.
    #[error("protocol error: {0}")]
    Protocol(#[from] pim_core::PimError),

    /// Socket or other I/O operation failed.
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
