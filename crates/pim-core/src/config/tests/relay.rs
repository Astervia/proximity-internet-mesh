use super::super::*;
use super::MINIMAL_CONFIG;

#[test]
fn relay_config_defaults_to_disabled() {
    let config = Config::from_toml_str(MINIMAL_CONFIG).unwrap();
    assert!(!config.relay.enabled);
}

#[test]
fn relay_enabled_true_parses() {
    let toml = r#"
[node]
name = "t"
[relay]
enabled = true
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert!(config.relay.enabled);
}
