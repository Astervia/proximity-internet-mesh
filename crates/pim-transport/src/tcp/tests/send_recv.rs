use super::super::*;
use super::make_frame;

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
