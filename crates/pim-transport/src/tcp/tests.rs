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
        .connect(&PeerAddress {
            node_id: node_a,
            addr: addr_a,
        })
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
    assert!(
        got_congested,
        "should hit Congested after saturating write queue"
    );
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
        transport_b.send(&node_a, make_frame(&[i])).await.unwrap();
    }

    // Receive all 10
    for i in 0..10u8 {
        let (_, msg) = transport_a.recv().await.unwrap();
        assert_eq!(msg.payload, vec![i]);
    }
}

#[tokio::test]
async fn shutdown_releases_listening_port() {
    // Pick a fixed port so we can verify it is reusable after shutdown.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let node = NodeId::from_bytes([0xAB; 16]);
    let transport = TcpTransport::new(addr, node).await.unwrap();
    assert_eq!(transport.listen_addr, addr);

    transport.shutdown().await;

    // After shutdown, the port must be free to rebind immediately.
    // Without cancellable accept loops, this bind would fail with
    // EADDRINUSE until the process exits.
    let rebound = TcpListener::bind(addr).await;
    assert!(
        rebound.is_ok(),
        "port {addr} should be reusable after shutdown(), got {rebound:?}"
    );
}

#[tokio::test]
async fn external_cancel_releases_listening_port() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let node = NodeId::from_bytes([0xCD; 16]);
    let cancel = CancellationToken::new();
    let transport = TcpTransport::new_with_cancel(addr, node, cancel.clone())
        .await
        .unwrap();

    cancel.cancel();
    // Give the accept task a tick to observe the cancellation and drop
    // the listener.
    for _ in 0..50 {
        if TcpListener::bind(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    drop(transport);
    panic!("port {addr} was not released within 1s after cancel");
}
