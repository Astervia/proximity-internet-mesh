use super::super::*;
use std::net::{IpAddr, Ipv4Addr};

fn make_service(self_id: u8, port: u16) -> (Arc<DiscoveryService>, mpsc::Receiver<PeerRecord>) {
    let id = NodeId::from_bytes([self_id; 16]);
    let (svc, rx) = DiscoveryService::new(id, [self_id; 32], NodeCapabilities::client(), port);
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

#[tokio::test]
async fn encrypted_service_ignores_plaintext_packets() {
    let id = NodeId::from_bytes([1; 16]);
    let (svc, mut rx) = DiscoveryService::new(id, [1; 32], NodeCapabilities::client(), 9205);
    let svc = Arc::new(svc.with_port(9205).with_discovery_key([0x33; 32]));
    let ad = peer_advertisement(4, 9100);
    let from = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 4)), 9101);

    let result = svc.handle_advertisement(&ad.serialize(), from).await;
    assert!(result.is_none());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn encrypted_service_accepts_keyed_packets() {
    let id = NodeId::from_bytes([1; 16]);
    let (svc, mut rx) = DiscoveryService::new(id, [1; 32], NodeCapabilities::client(), 9206);
    let svc = Arc::new(svc.with_port(9206).with_discovery_key([0x44; 32]));

    let ad = peer_advertisement(5, 9100);
    let from = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), 9101);

    let result = svc
        .handle_advertisement(&ad.serialize_encrypted(&[0x44; 32]), from)
        .await;
    assert!(result.is_some());
    assert!(rx.try_recv().is_ok());
}

/// `current_socket_fd` is the platform-shell hook for marking the
/// discovery socket VPN-bypassing on Android (`VpnService.protect(fd)`).
/// Verify it stays consistent with the running service: `Some(fd)`
/// after `run()` binds, `> 0` (valid Unix fd).
#[cfg(unix)]
#[tokio::test]
async fn current_socket_fd_publishes_after_bind() {
    use crate::service::current_socket_fd;
    use tokio_util::sync::CancellationToken;

    let port = 9210;
    let id = NodeId::from_bytes([42; 16]);
    let (svc, _rx) = DiscoveryService::new(id, [42; 32], NodeCapabilities::client(), port);
    let svc = Arc::new(svc.with_port(port));
    let cancel = CancellationToken::new();
    let svc_run = svc.clone();
    let cancel_run = cancel.clone();
    let join = tokio::spawn(async move { svc_run.run(cancel_run).await });

    // Poll briefly for the OnceLock to populate. Most of the time
    // the publish lands within milliseconds of `Service::run` start.
    let mut fd = None;
    for _ in 0..50 {
        if let Some(v) = current_socket_fd() {
            fd = Some(v);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    cancel.cancel();
    let _ = join.await;

    let fd = fd.expect("discovery socket fd should be published after bind");
    assert!(fd > 0, "expected a positive Unix fd, got {fd}");
}
