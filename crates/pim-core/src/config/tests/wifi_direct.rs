use super::super::*;
use super::{FULL_CONFIG, MINIMAL_CONFIG};

#[test]
fn wifi_direct_defaults_to_disabled() {
    let config = Config::from_toml_str(MINIMAL_CONFIG).unwrap();
    assert!(!config.wifi_direct.enabled);
    assert_eq!(config.wifi_direct.interface, "wlan0");
    assert_eq!(config.wifi_direct.go_intent, 7);
    assert_eq!(config.wifi_direct.listen_channel, 6);
    assert_eq!(config.wifi_direct.op_channel, 6);
    assert_eq!(config.wifi_direct.connect_method, "pbc");
}

#[test]
fn wifi_direct_enabled_round_trips() {
    let toml = r#"
[node]
name = "t"
[wifi_direct]
enabled = true
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert!(config.wifi_direct.enabled);
    let serialized = config.to_toml_string().unwrap();
    let reparsed = Config::from_toml_str(&serialized).unwrap();
    assert!(reparsed.wifi_direct.enabled);
}

#[test]
fn wifi_direct_custom_interface_parses() {
    let toml = r#"
[node]
name = "t"
[wifi_direct]
enabled = true
interface = "wlan1"
go_intent = 12
connect_method = "pin:12345670"
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert_eq!(config.wifi_direct.interface, "wlan1");
    assert_eq!(config.wifi_direct.go_intent, 12);
    assert_eq!(config.wifi_direct.connect_method, "pin:12345670");
}

#[test]
fn wifi_direct_go_intent_default_is_neutral() {
    let config = Config::from_toml_str(MINIMAL_CONFIG).unwrap();
    assert_eq!(config.wifi_direct.go_intent, 7);
}

#[test]
fn config_round_trip_with_wifi_direct_section() {
    let config = Config::from_toml_str(FULL_CONFIG).unwrap();
    assert!(!config.wifi_direct.enabled);
    assert_eq!(config.wifi_direct.interface, "wlan0");
    assert_eq!(config.wifi_direct.go_intent, 7);
    assert_eq!(config.wifi_direct.connect_method, "pbc");
    let serialized = config.to_toml_string().unwrap();
    let reparsed = Config::from_toml_str(&serialized).unwrap();
    assert_eq!(config, reparsed);
}
