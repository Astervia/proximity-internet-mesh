use super::super::*;
use super::MINIMAL_CONFIG;

#[test]
fn transport_reconnect_limit_defaults_to_twenty() {
    let config = Config::from_toml_str(MINIMAL_CONFIG).unwrap();
    assert_eq!(config.transport.max_reconnect_attempts, 20);
    assert_eq!(config.transport.connect_timeout_ms, 3000);
}

#[test]
fn transport_reconnect_limit_round_trips() {
    let toml = r#"
[node]
name = "t"
[transport]
max_reconnect_attempts = 7
connect_timeout_ms = 1500
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert_eq!(config.transport.max_reconnect_attempts, 7);
    assert_eq!(config.transport.connect_timeout_ms, 1500);
    let serialized = config.to_toml_string().unwrap();
    let reparsed = Config::from_toml_str(&serialized).unwrap();
    assert_eq!(reparsed.transport.max_reconnect_attempts, 7);
    assert_eq!(reparsed.transport.connect_timeout_ms, 1500);
}
