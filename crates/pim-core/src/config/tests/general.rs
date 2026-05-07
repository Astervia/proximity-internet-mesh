use super::super::*;
use super::{FULL_CONFIG, MINIMAL_CONFIG};
use std::path::PathBuf;

#[test]
fn parse_minimal_config() {
    let config = Config::from_toml_str(MINIMAL_CONFIG).unwrap();
    assert_eq!(config.node.name, "test-node");
    // Defaults should be applied
    assert_eq!(config.interface.name, "pim0");
    assert_eq!(config.interface.mtu, 1400);
    assert_eq!(config.routing.max_hops, 10);
    assert!(!config.gateway.enabled);
    assert!(config.security.require_encryption);
    assert_eq!(
        config.security.authorization_policy,
        AuthorizationPolicy::AllowAll
    );
    assert!(config.security.authorized_peers.is_empty());
    assert_eq!(config.mesh.mode, MeshMode::Open);
}

#[test]
fn parse_full_config() {
    let config = Config::from_toml_str(FULL_CONFIG).unwrap();
    assert_eq!(config.node.name, "my-device");
    assert_eq!(config.node.data_dir, PathBuf::from("/tmp/pim"));
    assert!(config.gateway.enabled);
    assert_eq!(config.gateway.nat_interface, "wlan0");
    assert_eq!(config.transport.listen_port, 9100);
    assert_eq!(config.transport.max_reconnect_attempts, 20);
    assert_eq!(config.transport.connect_timeout_ms, 3000);
    assert_eq!(config.mesh.mode, MeshMode::Private);
    assert_eq!(
        config.security.authorization_policy,
        AuthorizationPolicy::AllowList
    );
    assert_eq!(config.security.authorized_peers.len(), 1);
    assert_eq!(
        config.security.trust_store_file,
        PathBuf::from("/tmp/pim/trusted-peers.toml")
    );
}

#[test]
fn config_round_trip() {
    let config = Config::from_toml_str(FULL_CONFIG).unwrap();
    let serialized = config.to_toml_string().unwrap();
    let reparsed = Config::from_toml_str(&serialized).unwrap();
    assert_eq!(config, reparsed);
}

#[test]
fn invalid_toml_returns_error() {
    let result = Config::from_toml_str("not valid toml {{{}}}");
    assert!(result.is_err());
    assert!(format!("{}", result.unwrap_err()).contains("config error"));
}

#[test]
fn peers_section_is_optional() {
    let config = Config::from_toml_str(MINIMAL_CONFIG).unwrap();
    assert!(config.peers.is_empty());
}
