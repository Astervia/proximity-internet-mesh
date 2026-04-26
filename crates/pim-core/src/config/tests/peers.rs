use super::super::*;

#[test]
fn peer_configs_parse_with_mechanism_specific_fields() {
    let toml = r#"
[node]
name = "t"

[[peers]]
label = "relay"
mechanism = "tcp"
address = "relay:9100"

[[peers]]
label = "phone"
mechanism = "bluetooth"
ip = "192.168.44.2"
"#;

    let config = Config::from_toml_str(toml).unwrap();
    assert_eq!(config.peers.len(), 2);
    assert_eq!(config.peers[0].label, "relay");
    assert_eq!(
        config.peers[0].endpoint,
        PeerEndpointConfig::Tcp {
            address: "relay:9100".into()
        }
    );
    assert_eq!(config.peers[1].label, "phone");
    assert_eq!(
        config.peers[1].endpoint,
        PeerEndpointConfig::Bluetooth {
            ip: "192.168.44.2".into()
        }
    );
}
