use super::super::*;
use super::peer_id;

fn test_addr() -> SocketAddr {
    "127.0.0.1:9999".parse().unwrap()
}

fn test_target() -> ConnectTarget {
    ConnectTarget::Tcp(test_addr())
}

#[tokio::test]
async fn begin_reconnect_returns_true_once() {
    let mgr = ReconnectManager::new([test_target()]);
    assert!(
        mgr.begin_reconnect(test_target()).await,
        "first claim should succeed"
    );
    assert!(
        !mgr.begin_reconnect(test_target()).await,
        "second claim should fail"
    );
}

#[tokio::test]
async fn end_reconnect_allows_begin_again() {
    let mgr = ReconnectManager::new([test_target()]);
    mgr.begin_reconnect(test_target()).await;
    mgr.end_reconnect(test_target()).await;
    assert!(
        mgr.begin_reconnect(test_target()).await,
        "should be able to claim after release"
    );
}

#[tokio::test]
async fn configured_addr_none_for_unknown_peer() {
    let mgr = ReconnectManager::new([test_target()]);
    assert!(mgr.configured_target(&peer_id(1)).await.is_none());
}

#[tokio::test]
async fn configured_target_returns_target_after_register() {
    let target = test_target();
    let mgr = ReconnectManager::new([target]);
    mgr.register(peer_id(1), target).await;
    assert_eq!(mgr.configured_target(&peer_id(1)).await, Some(target));
}

#[tokio::test]
async fn configured_target_none_for_unconfigured_target() {
    let configured = ConnectTarget::Tcp("127.0.0.1:9999".parse::<SocketAddr>().unwrap());
    let other = ConnectTarget::Tcp("127.0.0.1:8888".parse::<SocketAddr>().unwrap());
    let mgr = ReconnectManager::new([configured]);
    mgr.register(peer_id(1), other).await;
    assert!(mgr.configured_target(&peer_id(1)).await.is_none());
}

#[tokio::test]
async fn multiple_configured_peers_tracked_independently() {
    let addr_a: SocketAddr = "127.0.0.1:9001".parse().unwrap();
    let addr_b: SocketAddr = "127.0.0.1:9002".parse().unwrap();
    let target_a = ConnectTarget::Tcp(addr_a);
    let target_b = ConnectTarget::Tcp(addr_b);
    let mgr = ReconnectManager::new([target_a, target_b]);

    mgr.register(peer_id(1), target_a).await;
    mgr.register(peer_id(2), target_b).await;

    assert_eq!(mgr.configured_target(&peer_id(1)).await, Some(target_a));
    assert_eq!(mgr.configured_target(&peer_id(2)).await, Some(target_b));

    // Reconnect slots are independent.
    assert!(mgr.begin_reconnect(target_a).await);
    assert!(mgr.begin_reconnect(target_b).await);
    assert!(!mgr.begin_reconnect(target_a).await);
}
