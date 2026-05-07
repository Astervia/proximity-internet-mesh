use super::super::*;
use super::{FULL_CONFIG, MINIMAL_CONFIG};

#[test]
fn discovery_defaults_when_section_absent() {
    let config = Config::from_toml_str(MINIMAL_CONFIG).unwrap();
    assert!(config.discovery.enabled);
    assert_eq!(config.discovery.port, 9101);
    assert_eq!(config.discovery.broadcast_interval_ms, 5000);
    assert_eq!(config.discovery.peer_timeout_ms, 30000);
    assert!(config.discovery.connect_relays);
    assert!(config.discovery.connect_gateways);
}

#[test]
fn discovery_enabled_false_round_trips() {
    let toml = r#"
[node]
name = "t"
[discovery]
enabled = false
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert!(!config.discovery.enabled);
    let serialized = config.to_toml_string().unwrap();
    let reparsed = Config::from_toml_str(&serialized).unwrap();
    assert!(!reparsed.discovery.enabled);
}

#[test]
fn discovery_custom_port_round_trips() {
    let toml = r#"
[node]
name = "t"
[discovery]
port = 19101
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert_eq!(config.discovery.port, 19101);
    let serialized = config.to_toml_string().unwrap();
    let reparsed = Config::from_toml_str(&serialized).unwrap();
    assert_eq!(reparsed.discovery.port, 19101);
}

#[test]
fn config_round_trip_with_all_discovery_fields() {
    let config = Config::from_toml_str(FULL_CONFIG).unwrap();
    assert!(config.discovery.enabled);
    assert_eq!(config.discovery.port, 9101);
    assert!(config.discovery.connect_relays);
    assert!(config.discovery.connect_gateways);
    assert!(!config.relay.enabled);
    let serialized = config.to_toml_string().unwrap();
    let reparsed = Config::from_toml_str(&serialized).unwrap();
    assert_eq!(config, reparsed);
}
