use super::super::*;
use super::peer_id;

/// When `wifi_direct.enabled = false` (the default), the block that spawns the
/// Wi-Fi Direct service must be skipped — verified by checking config.
#[test]
fn wifidirect_disabled_config_skips_spawning() {
    let config = Config::from_toml_str("[node]\nname=\"t\"\n").unwrap();
    assert!(
        !config.wifi_direct.enabled,
        "Wi-Fi Direct service must not start when enabled=false"
    );
}

/// When `wifi_direct.enabled = true`, the daemon reads `interface` and `listen_port`.
#[test]
fn wifidirect_enabled_config_exposes_interface_and_port() {
    let toml = "[node]\nname=\"t\"\n[wifi_direct]\nenabled=true\ninterface=\"wlan1\"\n";
    let config = Config::from_toml_str(toml).unwrap();
    assert!(config.wifi_direct.enabled);
    assert_eq!(config.wifi_direct.interface, "wlan1");
    // listen_port comes from transport section, not wifi_direct.
    assert_eq!(config.transport.listen_port, 9100);
}

/// Discovered Wi-Fi Direct peer addresses must be registered for reconnect-on-loss.
#[tokio::test]
async fn wifidirect_addr_registered_for_reconnect() {
    let mgr = ReconnectManager::new([]);
    let addr: SocketAddr = "192.168.49.100:9100".parse().unwrap();
    let target = ConnectTarget::Tcp(addr);
    mgr.register_discovered(target).await;
    // Bind a fake NodeId to the address to enable the lookup.
    mgr.register(peer_id(60), target).await;
    assert_eq!(
        mgr.is_reconnectable_target(&peer_id(60)).await,
        Some(target),
        "Wi-Fi Direct discovered peer must be reconnectable"
    );
}

/// Wi-Fi Direct and UDP discovery can both be enabled simultaneously; their
/// peer channel addresses both go through `register_discovered`.
#[tokio::test]
async fn wifidirect_coexists_with_udp_discovery() {
    let mgr = ReconnectManager::new([]);
    let udp_addr: SocketAddr = "172.34.0.20:9100".parse().unwrap();
    let wfd_addr: SocketAddr = "192.168.49.100:9100".parse().unwrap();
    let udp_target = ConnectTarget::Tcp(udp_addr);
    let wfd_target = ConnectTarget::Tcp(wfd_addr);
    mgr.register_discovered(udp_target).await;
    mgr.register_discovered(wfd_target).await;
    mgr.register(peer_id(61), udp_target).await;
    mgr.register(peer_id(62), wfd_target).await;
    assert!(
        mgr.is_reconnectable_target(&peer_id(61)).await.is_some(),
        "UDP peer reconnectable"
    );
    assert!(
        mgr.is_reconnectable_target(&peer_id(62)).await.is_some(),
        "WFD peer reconnectable"
    );
}

/// Verify `WifiDirectDiscovery::new` can be constructed from config without panicking.
#[test]
fn wifidirect_discovery_construction_from_config() {
    use pim_wifidirect::WifiDirectDiscovery;
    let toml = "[node]\nname=\"t\"\n[wifi_direct]\nenabled=true\n";
    let config = Config::from_toml_str(toml).unwrap();
    let (_svc, _rx) = WifiDirectDiscovery::new(
        config.node.name,
        config.wifi_direct,
        config.transport.listen_port,
    );
    // Construction must not panic.
}

#[test]
fn transport_reconnect_limit_parses_from_config() {
    let config =
        Config::from_toml_str("[node]\nname=\"t\"\n[transport]\nmax_reconnect_attempts=4\n")
            .unwrap();
    assert_eq!(config.transport.max_reconnect_attempts, 4);
}
