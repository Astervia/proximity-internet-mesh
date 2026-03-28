use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, error, info, warn};

use pim_core::{FrameCodec, NodeId};
use pim_protocol::{LengthDelimitedCodec, TransportFrame};

use crate::{PeerAddress, Transport, TransportError};

/// TCP-based transport for development and Docker testing.
///
/// Each peer connection is a bidirectional TCP stream. Frames are
/// length-delimited for stream framing.
pub struct TcpTransport {
    /// Map of connected peers: NodeId → write half of their TCP stream.
    peers: Arc<RwLock<HashMap<NodeId, PeerWriter>>>,
    /// Channel for received frames from all peer read tasks.
    incoming_tx: mpsc::Sender<(NodeId, TransportFrame)>,
    incoming_rx: Mutex<mpsc::Receiver<(NodeId, TransportFrame)>>,
    /// Our actual listen address (after bind).
    pub listen_addr: SocketAddr,
    /// Our own node ID (so we can tell peers who we are).
    node_id: NodeId,
}

/// Handle for writing to a peer's TCP stream.
struct PeerWriter {
    tx: mpsc::Sender<Vec<u8>>,
}

impl TcpTransport {
    /// Create a new TCP transport that listens on the given address.
    ///
    /// Spawns a background listener task that accepts incoming connections.
    /// Incoming connections must send a 16-byte NodeId as the first message
    /// so we can identify the peer.
    pub async fn new(listen_addr: SocketAddr, node_id: NodeId) -> Result<Arc<Self>, TransportError> {
        let (incoming_tx, incoming_rx) = mpsc::channel(256);

        // Bind first so we know the actual address (important for port 0)
        let listener = TcpListener::bind(listen_addr).await?;
        let actual_addr = listener.local_addr()?;

        let transport = Arc::new(Self {
            peers: Arc::new(RwLock::new(HashMap::new())),
            incoming_tx,
            incoming_rx: Mutex::new(incoming_rx),
            listen_addr: actual_addr,
            node_id,
        });

        info!(listen_addr = %actual_addr, "transport listening");

        let t = transport.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        debug!(%addr, "accepted connection");
                        let t2 = t.clone();
                        tokio::spawn(async move {
                            if let Err(e) = t2.handle_incoming(stream).await {
                                warn!(%addr, "incoming connection failed: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        error!("listener accept failed: {e}");
                        break;
                    }
                }
            }
        });

        Ok(transport)
    }

    /// Handle an incoming TCP connection.
    /// The peer sends its 16-byte NodeId first, then we exchange frames.
    async fn handle_incoming(self: &Arc<Self>, mut stream: TcpStream) -> Result<(), TransportError> {
        // Read the peer's NodeId (16 bytes)
        let mut id_buf = [0u8; 16];
        stream.read_exact(&mut id_buf).await?;
        let peer_id = NodeId::from_bytes(id_buf);

        info!(%peer_id, "peer identified on incoming connection");

        self.register_peer(peer_id, stream).await;
        Ok(())
    }

    /// Register a connected peer: split the stream, spawn read/write tasks.
    async fn register_peer(&self, peer_id: NodeId, stream: TcpStream) {
        let (read_half, write_half) = stream.into_split();

        // Write task: receives serialized frames via channel and writes to socket
        let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(64);
        let write_peer_id = peer_id;
        tokio::spawn(async move {
            let mut writer = write_half;
            while let Some(data) = write_rx.recv().await {
                if let Err(e) = writer.write_all(&data).await {
                    warn!(%write_peer_id, "write failed: {e}");
                    break;
                }
            }
            debug!(%write_peer_id, "write task ended");
        });

        // Read task: reads frames from socket and sends to incoming channel
        let incoming_tx = self.incoming_tx.clone();
        let peers = self.peers.clone();
        let read_peer_id = peer_id;
        tokio::spawn(async move {
            let mut reader = read_half;
            let mut buf = BytesMut::with_capacity(4096);

            loop {
                // Read more data from socket
                let mut tmp = [0u8; 4096];
                match reader.read(&mut tmp).await {
                    Ok(0) => {
                        info!(%read_peer_id, "peer disconnected");
                        break;
                    }
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    Err(e) => {
                        warn!(%read_peer_id, "read error: {e}");
                        break;
                    }
                }

                // Try to decode complete frames from buffer
                loop {
                    match LengthDelimitedCodec::decode(&mut buf) {
                        Ok(Some(mut frame_bytes)) => {
                            match TransportFrame::decode(&mut frame_bytes) {
                                Ok(frame) => {
                                    if incoming_tx.send((read_peer_id, frame)).await.is_err() {
                                        debug!(%read_peer_id, "incoming channel closed");
                                        return;
                                    }
                                }
                                Err(e) => {
                                    warn!(%read_peer_id, "frame decode error: {e}");
                                }
                            }
                        }
                        Ok(None) => break, // need more data
                        Err(e) => {
                            warn!(%read_peer_id, "framing error: {e}");
                            break;
                        }
                    }
                }
            }

            // Cleanup: remove peer from map
            peers.write().await.remove(&read_peer_id);
            info!(%read_peer_id, "peer removed");
        });

        // Store the writer
        self.peers
            .write()
            .await
            .insert(peer_id, PeerWriter { tx: write_tx });
    }

    /// Rename a peer in the connection map after learning their real NodeId
    /// from a higher-level handshake.  No-op if `old_id` is not present.
    pub async fn rename_peer(&self, old_id: NodeId, new_id: NodeId) {
        let mut peers = self.peers.write().await;
        if let Some(writer) = peers.remove(&old_id) {
            peers.insert(new_id, writer);
            debug!(%old_id, %new_id, "peer renamed in transport");
        }
    }
}

#[async_trait]
impl Transport for TcpTransport {
    async fn send(&self, peer: &NodeId, frame: TransportFrame) -> Result<(), TransportError> {
        let peers = self.peers.read().await;
        let writer = peers
            .get(peer)
            .ok_or_else(|| TransportError::PeerNotConnected(*peer))?;

        // Serialize frame
        let mut frame_buf = BytesMut::new();
        frame.encode(&mut frame_buf);

        // Length-delimit
        let mut wire_buf = BytesMut::new();
        LengthDelimitedCodec::encode(&frame_buf, &mut wire_buf);

        // Non-blocking: return Congested instead of blocking when the write
        // queue is full.  Callers apply priority-based drop policy.
        writer.tx.try_send(wire_buf.to_vec()).map_err(|e| match e {
            tokio::sync::mpsc::error::TrySendError::Full(_) => TransportError::Congested(*peer),
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                TransportError::SendFailed("write channel closed".into())
            }
        })
    }

    async fn recv(&self) -> Result<(NodeId, TransportFrame), TransportError> {
        let mut rx = self.incoming_rx.lock().await;
        rx.recv()
            .await
            .ok_or(TransportError::Shutdown)
    }

    async fn connect(&self, peer: &PeerAddress) -> Result<(), TransportError> {
        let mut stream = TcpStream::connect(peer.addr)
            .await
            .map_err(|e| TransportError::ConnectionFailed(e.to_string()))?;

        // Send our NodeId to identify ourselves
        stream.write_all(self.node_id.as_bytes()).await?;

        info!(peer_id = %peer.node_id, addr = %peer.addr, "connected to peer");

        self.register_peer(peer.node_id, stream).await;
        Ok(())
    }

    async fn disconnect(&self, peer: &NodeId) -> Result<(), TransportError> {
        let removed = self.peers.write().await.remove(peer);
        if removed.is_some() {
            info!(%peer, "disconnected from peer");
            Ok(())
        } else {
            Err(TransportError::PeerNotConnected(*peer))
        }
    }

    fn connected_peers(&self) -> Vec<NodeId> {
        // Use try_read to avoid blocking; return empty if locked
        match self.peers.try_read() {
            Ok(peers) => peers.keys().copied().collect(),
            Err(_) => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pim_protocol::FrameType;

    fn make_frame(data: &[u8]) -> TransportFrame {
        TransportFrame {
            frame_type: FrameType::Data,
            nonce: [0; 12],
            payload: data.to_vec(),
            tag: [0; 16],
        }
    }

    #[tokio::test]
    async fn two_transports_send_recv() {
        let node_a = NodeId::from_bytes([0xAA; 16]);
        let node_b = NodeId::from_bytes([0xBB; 16]);

        let transport_a = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_a)
            .await
            .unwrap();
        let listen_addr_a = transport_a.listen_addr;

        let transport_b = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_b)
            .await
            .unwrap();

        // B connects to A
        transport_b
            .connect(&PeerAddress {
                node_id: node_a,
                addr: listen_addr_a,
            })
            .await
            .unwrap();

        // Give the listener time to accept and register
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // B sends to A
        let frame = make_frame(b"hello from B");
        transport_b.send(&node_a, frame.clone()).await.unwrap();

        // A receives
        let (sender, received) = transport_a.recv().await.unwrap();
        assert_eq!(sender, node_b);
        assert_eq!(received.payload, b"hello from B");
    }

    #[tokio::test]
    async fn bidirectional_communication() {
        let node_a = NodeId::from_bytes([0x01; 16]);
        let node_b = NodeId::from_bytes([0x02; 16]);

        let transport_a = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_a)
            .await
            .unwrap();
        let addr_a = transport_a.listen_addr;

        let transport_b = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_b)
            .await
            .unwrap();

        transport_b
            .connect(&PeerAddress {
                node_id: node_a,
                addr: addr_a,
            })
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // B → A
        transport_b
            .send(&node_a, make_frame(b"B to A"))
            .await
            .unwrap();
        let (sender, msg) = transport_a.recv().await.unwrap();
        assert_eq!(sender, node_b);
        assert_eq!(msg.payload, b"B to A");

        // A → B
        transport_a
            .send(&node_b, make_frame(b"A to B"))
            .await
            .unwrap();
        let (sender, msg) = transport_b.recv().await.unwrap();
        assert_eq!(sender, node_a);
        assert_eq!(msg.payload, b"A to B");
    }

    #[tokio::test]
    async fn send_to_disconnected_peer_fails() {
        let node_a = NodeId::from_bytes([0x01; 16]);
        let unknown = NodeId::from_bytes([0xFF; 16]);

        let transport = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_a)
            .await
            .unwrap();

        let result = transport.send(&unknown, make_frame(b"hello")).await;
        assert!(matches!(result, Err(TransportError::PeerNotConnected(_))));
    }

    #[tokio::test]
    async fn disconnect_removes_peer() {
        let node_a = NodeId::from_bytes([0x01; 16]);
        let node_b = NodeId::from_bytes([0x02; 16]);

        let transport_a = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_a)
            .await
            .unwrap();
        let addr_a = transport_a.listen_addr;

        let transport_b = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_b)
            .await
            .unwrap();

        transport_b
            .connect(&PeerAddress {
                node_id: node_a,
                addr: addr_a,
            })
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        assert!(transport_b.connected_peers().contains(&node_a));
        transport_b.disconnect(&node_a).await.unwrap();
        assert!(!transport_b.connected_peers().contains(&node_a));
    }

    #[tokio::test]
    async fn multiple_peers() {
        let node_a = NodeId::from_bytes([0x01; 16]);
        let node_b = NodeId::from_bytes([0x02; 16]);
        let node_c = NodeId::from_bytes([0x03; 16]);

        let transport_a = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_a)
            .await
            .unwrap();
        let addr_a = transport_a.listen_addr;

        let transport_b = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_b)
            .await
            .unwrap();

        let transport_c = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_c)
            .await
            .unwrap();

        // B and C both connect to A
        transport_b
            .connect(&PeerAddress {
                node_id: node_a,
                addr: addr_a,
            })
            .await
            .unwrap();
        transport_c
            .connect(&PeerAddress {
                node_id: node_a,
                addr: addr_a,
            })
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Both send to A
        transport_b
            .send(&node_a, make_frame(b"from B"))
            .await
            .unwrap();
        transport_c
            .send(&node_a, make_frame(b"from C"))
            .await
            .unwrap();

        // A receives both (order may vary)
        let mut received = Vec::new();
        let (s1, m1) = transport_a.recv().await.unwrap();
        received.push((s1, m1.payload.clone()));
        let (s2, m2) = transport_a.recv().await.unwrap();
        received.push((s2, m2.payload.clone()));

        received.sort_by_key(|(_, p)| p.clone());
        assert_eq!(received[0].1, b"from B");
        assert_eq!(received[0].0, node_b);
        assert_eq!(received[1].1, b"from C");
        assert_eq!(received[1].0, node_c);
    }

    #[tokio::test]
    async fn send_returns_congested_when_write_queue_full() {
        // Verify that try_send is used and TransportError::Congested is returned
        // when the per-peer write channel is full.
        //
        // Strategy: create a transport pair, then flood the write side fast
        // enough that the write task can't keep up.  We use a small write-
        // channel capacity (1) via a saturating loop with a stall on the far
        // end (stop consuming incoming frames on the receiver transport so the
        // TCP receive buffer fills and backs up the write task).

        let node_a = NodeId::from_bytes([0x01; 16]);
        let node_b = NodeId::from_bytes([0x02; 16]);

        let transport_a = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_a)
            .await
            .unwrap();
        let addr_a = transport_a.listen_addr;

        let transport_b = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_b)
            .await
            .unwrap();

        transport_b
            .connect(&PeerAddress { node_id: node_a, addr: addr_a })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Attempt to send a large number of frames very quickly without
        // consuming on transport_a.  Eventually the write channel (cap 64)
        // will fill and try_send will return Full → Congested.
        let payload = vec![0u8; 1024];
        let mut got_congested = false;
        for _ in 0..512 {
            let f = TransportFrame {
                frame_type: FrameType::Data,
                nonce: [0; 12],
                payload: payload.clone(),
                tag: [0; 16],
            };
            match transport_b.send(&node_a, f).await {
                Ok(()) => {}
                Err(crate::TransportError::Congested(_)) => {
                    got_congested = true;
                    break;
                }
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        assert!(got_congested, "should hit Congested after saturating write queue");
    }

    #[tokio::test]
    async fn multiple_frames_in_sequence() {
        let node_a = NodeId::from_bytes([0x01; 16]);
        let node_b = NodeId::from_bytes([0x02; 16]);

        let transport_a = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_a)
            .await
            .unwrap();
        let addr_a = transport_a.listen_addr;

        let transport_b = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_b)
            .await
            .unwrap();

        transport_b
            .connect(&PeerAddress {
                node_id: node_a,
                addr: addr_a,
            })
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Send 10 frames rapidly
        for i in 0..10u8 {
            transport_b
                .send(&node_a, make_frame(&[i]))
                .await
                .unwrap();
        }

        // Receive all 10
        for i in 0..10u8 {
            let (_, msg) = transport_a.recv().await.unwrap();
            assert_eq!(msg.payload, vec![i]);
        }
    }
}
