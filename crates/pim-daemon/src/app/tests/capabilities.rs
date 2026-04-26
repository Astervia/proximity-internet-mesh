use super::super::*;

fn cfg_gateway() -> Config {
    Config::from_toml_str("[node]\nname=\"t\"\n[gateway]\nenabled=true\n").unwrap()
}

fn cfg_relay() -> Config {
    Config::from_toml_str("[node]\nname=\"t\"\n[relay]\nenabled=true\n").unwrap()
}

fn cfg_client() -> Config {
    Config::from_toml_str("[node]\nname=\"t\"\n").unwrap()
}

#[test]
fn gateway_config_yields_gateway_caps() {
    let caps = node_capabilities(&cfg_gateway());
    assert!(caps.is_gateway(), "gateway flag expected");
    assert!(caps.is_relay(), "relay flag expected on gateway");
    assert!(caps.is_client(), "client flag expected on gateway");
}

#[test]
fn relay_config_yields_relay_caps() {
    let caps = node_capabilities(&cfg_relay());
    assert!(caps.is_relay(), "relay flag expected");
    assert!(caps.is_client(), "client flag expected on relay");
    assert!(!caps.is_gateway(), "gateway flag must NOT be set on relay");
}

#[test]
fn client_config_yields_client_caps_only() {
    let caps = node_capabilities(&cfg_client());
    assert!(caps.is_client(), "client flag expected");
    assert!(!caps.is_relay(), "relay flag must NOT be set on client");
    assert!(!caps.is_gateway(), "gateway flag must NOT be set on client");
}

#[test]
fn gateway_caps_bits_are_correct() {
    let caps = node_capabilities(&cfg_gateway());
    assert_eq!(
        caps.bits(),
        0x07,
        "gateway caps must be CLIENT|RELAY|GATEWAY = 0x07"
    );
}
