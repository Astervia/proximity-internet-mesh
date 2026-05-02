use super::super::*;
use super::{FULL_CONFIG, MINIMAL_CONFIG};

#[test]
fn minimal_config_applies_messaging_defaults() {
    let config = Config::from_toml_str(MINIMAL_CONFIG).unwrap();
    assert_eq!(config.messaging.broadcast.outgoing_interval_s, None);
    assert!(config.messaging.broadcast.watch_incoming);
    assert_eq!(config.messaging.broadcast.min_peer_interval_s, 60);
}

#[test]
fn full_config_parses_broadcast_overrides() {
    let config = Config::from_toml_str(FULL_CONFIG).unwrap();
    assert_eq!(config.messaging.broadcast.outgoing_interval_s, Some(300));
    assert!(config.messaging.broadcast.watch_incoming);
    assert_eq!(config.messaging.broadcast.min_peer_interval_s, 60);
}

#[test]
fn outgoing_interval_can_be_omitted_to_disable() {
    let toml = r#"
[node]
name = "no-broadcast"

[messaging.broadcast]
watch_incoming = false
min_peer_interval_s = 120
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert_eq!(config.messaging.broadcast.outgoing_interval_s, None);
    assert!(!config.messaging.broadcast.watch_incoming);
    assert_eq!(config.messaging.broadcast.min_peer_interval_s, 120);
}

#[test]
fn min_outgoing_interval_constant_is_30s() {
    // Pin the floor so accidental loosening is caught here as well as
    // at the RPC layer.
    assert_eq!(BroadcastConfig::MIN_OUTGOING_INTERVAL_S, 30);
}
