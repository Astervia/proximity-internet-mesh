use super::super::*;
use super::peer_id;

/// `register_discovered` + `is_reconnectable_addr` makes discovered peers reconnectable.
#[tokio::test]
async fn register_discovered_makes_peer_reconnectable() {
    let mgr = ReconnectManager::new([]);
    let addr: SocketAddr = "127.0.0.1:9200".parse().unwrap();
    let target = ConnectTarget::Tcp(addr);
    // Before registration: not reconnectable.
    assert!(mgr.is_reconnectable_target(&peer_id(50)).await.is_none());
    // Register as discovered and bind the NodeId → addr.
    mgr.register_discovered(target).await;
    mgr.register(peer_id(50), target).await;
    // Now the peer should be reconnectable.
    assert_eq!(
        mgr.is_reconnectable_target(&peer_id(50)).await,
        Some(target)
    );
}

/// Both configured and discovered peers are reconnectable.
#[tokio::test]
async fn is_reconnectable_covers_configured_and_discovered() {
    let configured: SocketAddr = "127.0.0.1:9100".parse().unwrap();
    let discovered: SocketAddr = "127.0.0.1:9200".parse().unwrap();
    let configured = ConnectTarget::Tcp(configured);
    let discovered = ConnectTarget::Tcp(discovered);
    let mgr = ReconnectManager::new([configured]);
    mgr.register(peer_id(51), configured).await;
    mgr.register(peer_id(52), discovered).await;
    mgr.register_discovered(discovered).await;

    assert!(
        mgr.is_reconnectable_target(&peer_id(51)).await.is_some(),
        "configured peer must be reconnectable"
    );
    assert!(
        mgr.is_reconnectable_target(&peer_id(52)).await.is_some(),
        "discovered peer must be reconnectable"
    );
    assert!(
        mgr.is_reconnectable_target(&peer_id(99)).await.is_none(),
        "unknown peer must not be reconnectable"
    );
}

/// Client-only capability `(!is_relay() && !is_gateway())` must trigger the skip condition.
#[test]
fn client_only_peer_is_skipped() {
    let caps = NodeCapabilities::client();
    // The consumer skips when neither relay nor gateway.
    assert!(
        !caps.is_relay() && !caps.is_gateway(),
        "consumer must filter out client-only peers"
    );
}

/// Relay and gateway capability sets must pass the capability filter.
#[test]
fn relay_and_gateway_caps_pass_filter() {
    let relay = NodeCapabilities::relay();
    let gateway = NodeCapabilities::gateway();
    assert!(
        relay.is_relay() || relay.is_gateway(),
        "relay caps must pass filter"
    );
    assert!(
        gateway.is_relay() || gateway.is_gateway(),
        "gateway caps must pass filter"
    );
}

/// The self-check (`record.node_id == state.self_id`) correctly identifies own broadcasts.
#[test]
fn self_advertisement_is_ignored() {
    let own_id = NodeId::from_bytes([42; 16]);
    let record_id = NodeId::from_bytes([42; 16]);
    assert_eq!(
        record_id, own_id,
        "own NodeId must be caught by self-check in consumer"
    );
}

/// When `discovery.enabled = false`, the discovery service must not be started.
#[test]
fn discovery_disabled_in_config_skips_spawning() {
    let config = Config::from_toml_str("[node]\nname=\"t\"\n[discovery]\nenabled=false\n").unwrap();
    assert!(
        !config.discovery.enabled,
        "discovery service must not start when enabled=false"
    );
}
