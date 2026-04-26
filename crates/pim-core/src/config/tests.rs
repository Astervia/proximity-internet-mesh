use super::*;
use std::path::PathBuf;

const MINIMAL_CONFIG: &str = r#"
[node]
name = "test-node"
"#;

const FULL_CONFIG: &str = r#"
[node]
name = "my-device"
data_dir = "/tmp/pim"

[interface]
name = "pim0"
mtu = 1400
mesh_ip = "auto"
mesh_ipv6 = "fd77::10/64"

[discovery]
enabled = true
port = 9101
broadcast_interval_ms = 5000
peer_timeout_ms = 30000
connect_relays = true
connect_gateways = true
shared_key = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"

[relay]
enabled = false

[transport]
type = "tcp"
listen_port = 9100
max_reconnect_attempts = 20
connect_timeout_ms = 3000

[routing]
max_hops = 10
algorithm = "distance-vector"
route_expiry_s = 300

[gateway]
enabled = true
nat_interface = "wlan0"
max_connections = 200

[security]
key_file = "/tmp/pim/node.key"
require_encryption = true
authorization_policy = "allow_list"
authorized_peers = ["abababababababababababababababab"]
trust_store_file = "/tmp/pim/trusted-peers.toml"

[wifi_direct]
enabled = false
interface = "wlan0"
go_intent = 7
listen_channel = 6
op_channel = 6
connect_method = "pbc"

[bluetooth]
enabled = false
interface = "auto"
radio_discovery_enabled = true
device_name_prefix = "PIM-"
local_alias = ""
connect_pan = true
serve_nap = false
nap_bridge = "br-bt"
auto_discover_peers = true
poll_interval_ms = 2000
scan_interval_ms = 5000
peer_discovery_interval_ms = 2000
bluetoothctl_timeout_s = 15
discoverable_timeout_s = 180
startup_timeout_ms = 15000
"#;

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
    assert!(config.discovery.shared_key.is_none());
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
    assert_eq!(
        config.discovery.shared_key.as_deref(),
        Some("00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff")
    );
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
fn discovery_defaults_when_section_absent() {
    let config = Config::from_toml_str(MINIMAL_CONFIG).unwrap();
    assert!(config.discovery.enabled);
    assert_eq!(config.discovery.port, 9101);
    assert_eq!(config.discovery.broadcast_interval_ms, 5000);
    assert_eq!(config.discovery.peer_timeout_ms, 30000);
    assert!(config.discovery.connect_relays);
    assert!(config.discovery.connect_gateways);
    assert!(config.discovery.shared_key.is_none());
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

#[test]
fn peers_section_is_optional() {
    let config = Config::from_toml_str(MINIMAL_CONFIG).unwrap();
    assert!(config.peers.is_empty());
}

#[test]
fn config_round_trip_with_all_discovery_fields() {
    let config = Config::from_toml_str(FULL_CONFIG).unwrap();
    assert!(config.discovery.enabled);
    assert_eq!(config.discovery.port, 9101);
    assert!(config.discovery.connect_relays);
    assert!(config.discovery.connect_gateways);
    assert!(config.discovery.shared_key.is_some());
    assert!(!config.relay.enabled);
    let serialized = config.to_toml_string().unwrap();
    let reparsed = Config::from_toml_str(&serialized).unwrap();
    assert_eq!(config, reparsed);
}

#[test]
fn authorization_policy_round_trips() {
    let toml = r#"
[node]
name = "t"
[security]
authorization_policy = "trust_on_first_use"
authorized_peers = ["abababababababababababababababab"]
trust_store_file = "/tmp/pim/trusted-peers.toml"
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert_eq!(
        config.security.authorization_policy,
        AuthorizationPolicy::TrustOnFirstUse
    );
    assert_eq!(config.security.authorized_peers.len(), 1);
    assert_eq!(
        config.security.trust_store_file,
        PathBuf::from("/tmp/pim/trusted-peers.toml")
    );
    let serialized = config.to_toml_string().unwrap();
    let reparsed = Config::from_toml_str(&serialized).unwrap();
    assert_eq!(config, reparsed);
}

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

#[test]
fn bluetooth_defaults_to_disabled() {
    let config = Config::from_toml_str(MINIMAL_CONFIG).unwrap();
    assert!(!config.bluetooth.enabled);
    #[cfg(target_os = "macos")]
    assert_eq!(config.bluetooth.interface, "bridge0");
    #[cfg(not(target_os = "macos"))]
    assert_eq!(config.bluetooth.interface, "auto");
    assert!(config.bluetooth.radio_discovery_enabled);
    assert_eq!(config.bluetooth.device_name_prefix, "PIM-");
    assert_eq!(config.bluetooth.local_alias, "");
    assert!(config.bluetooth.connect_pan);
    assert!(!config.bluetooth.serve_nap);
    assert_eq!(config.bluetooth.nap_bridge, "br-bt");
    assert_eq!(config.bluetooth.nap_bridge_addr, "192.168.44.1/24");
    assert!(config.bluetooth.dhcp_enabled);
    assert!(config.bluetooth.dhcp_range.is_none());
    assert_eq!(config.bluetooth.dhcp_lease_time, "12h");
    assert!(config.bluetooth.dhcp_dns.is_none());
    assert!(config.bluetooth.request_dhcp);
    assert!(config.bluetooth.auto_discover_peers);
    assert_eq!(config.bluetooth.poll_interval_ms, 2_000);
    assert_eq!(config.bluetooth.scan_interval_ms, 5_000);
    assert_eq!(config.bluetooth.peer_discovery_interval_ms, 2_000);
    assert_eq!(config.bluetooth.bluetoothctl_timeout_s, 15);
    assert_eq!(config.bluetooth.discoverable_timeout_s, 180);
    assert_eq!(config.bluetooth.startup_timeout_ms, 15_000);
}

#[test]
fn bluetooth_enabled_round_trips() {
    let toml = r#"
[node]
name = "t"
[bluetooth]
enabled = true
radio_discovery_enabled = true
device_name_prefix = "PIM-"
local_alias = "PIM-t"
connect_pan = false
serve_nap = true
nap_bridge = "br-pan0"
auto_discover_peers = false
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert!(config.bluetooth.enabled);
    assert!(config.bluetooth.radio_discovery_enabled);
    assert_eq!(config.bluetooth.device_name_prefix, "PIM-");
    assert_eq!(config.bluetooth.local_alias, "PIM-t");
    assert!(!config.bluetooth.connect_pan);
    assert!(config.bluetooth.serve_nap);
    assert_eq!(config.bluetooth.nap_bridge, "br-pan0");
    assert!(!config.bluetooth.auto_discover_peers);
    let serialized = config.to_toml_string().unwrap();
    let reparsed = Config::from_toml_str(&serialized).unwrap();
    assert!(reparsed.bluetooth.enabled);
    assert!(!reparsed.bluetooth.connect_pan);
    assert!(reparsed.bluetooth.serve_nap);
    assert!(!reparsed.bluetooth.auto_discover_peers);
}

#[test]
fn bluetooth_custom_interface_and_timeouts_parse() {
    let toml = r#"
[node]
name = "t"
[bluetooth]
enabled = true
interface = "bnep1"
radio_discovery_enabled = true
device_name_prefix = "MESH-"
local_alias = "MESH-t"
connect_pan = true
serve_nap = false
nap_bridge = "br-bt"
auto_discover_peers = true
poll_interval_ms = 500
scan_interval_ms = 750
peer_discovery_interval_ms = 750
bluetoothctl_timeout_s = 20
discoverable_timeout_s = 60
startup_timeout_ms = 10000
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert_eq!(config.bluetooth.interface, "bnep1");
    assert!(config.bluetooth.radio_discovery_enabled);
    assert_eq!(config.bluetooth.device_name_prefix, "MESH-");
    assert_eq!(config.bluetooth.local_alias, "MESH-t");
    assert!(config.bluetooth.connect_pan);
    assert!(!config.bluetooth.serve_nap);
    assert_eq!(config.bluetooth.nap_bridge, "br-bt");
    assert!(config.bluetooth.auto_discover_peers);
    assert_eq!(config.bluetooth.poll_interval_ms, 500);
    assert_eq!(config.bluetooth.scan_interval_ms, 750);
    assert_eq!(config.bluetooth.peer_discovery_interval_ms, 750);
    assert_eq!(config.bluetooth.bluetoothctl_timeout_s, 20);
    assert_eq!(config.bluetooth.discoverable_timeout_s, 60);
    assert_eq!(config.bluetooth.startup_timeout_ms, 10_000);
}

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
