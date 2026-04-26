use super::super::*;

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
