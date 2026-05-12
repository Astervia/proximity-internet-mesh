use super::super::*;
use super::MINIMAL_CONFIG;

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
fn bluetooth_rfcomm_defaults_to_disabled() {
    let config = Config::from_toml_str(MINIMAL_CONFIG).unwrap();
    assert!(!config.bluetooth_rfcomm.enabled);
    assert_eq!(config.bluetooth_rfcomm.channel, 22);
    assert_eq!(config.bluetooth_rfcomm.device_name_prefix, "PIM-");
    assert!(config.bluetooth_rfcomm.outbound_enabled);
    assert_eq!(config.bluetooth_rfcomm.poll_interval_ms, 30_000);
    assert!(config.bluetooth_rfcomm.bridge_to_tcp);
    // Phase 2 discovery defaults: opt-in by default; cadence 60 s
    // (mirrors `BluetoothPlugin.kt` Android side). Regression-guard
    // these so a future schema bump can't silently flip them.
    assert!(config.bluetooth_rfcomm.discovery_enabled);
    assert_eq!(config.bluetooth_rfcomm.inquiry_interval_ms, 60_000);
}

#[test]
fn bluetooth_rfcomm_discovery_knobs_round_trip() {
    // discovery_enabled = false + a custom inquiry cadence must
    // survive a config save/reload cycle so users can opt out on
    // headless gateways.
    let toml = r#"
[node]
name = "t"

[bluetooth_rfcomm]
enabled = true
discovery_enabled = false
inquiry_interval_ms = 30000
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert!(!config.bluetooth_rfcomm.discovery_enabled);
    assert_eq!(config.bluetooth_rfcomm.inquiry_interval_ms, 30_000);

    let serialized = config.to_toml_string().unwrap();
    let reparsed = Config::from_toml_str(&serialized).unwrap();
    assert!(!reparsed.bluetooth_rfcomm.discovery_enabled);
    assert_eq!(reparsed.bluetooth_rfcomm.inquiry_interval_ms, 30_000);
}

#[test]
fn bluetooth_coc_defaults_to_enabled() {
    // Phase 5 flipped the default: CoC is now the shipped Bluetooth
    // transport. Lock the flip explicitly so a future refactor can't
    // silently revert it (the audio-leak regression that motivated
    // this would come back).
    let config = Config::from_toml_str(MINIMAL_CONFIG).unwrap();
    assert!(config.bluetooth_coc.enabled);
    // Default PSM must be inside the LE dynamic range
    // 0x0080..=0x00FF; SIG-assigned 0x0001..=0x007F is forbidden.
    assert_eq!(config.bluetooth_coc.psm, 0x0083);
    assert!((0x0080..=0x00FF).contains(&config.bluetooth_coc.psm));
    assert_eq!(config.bluetooth_coc.device_name_prefix, "PIM-");
    assert!(config.bluetooth_coc.outbound_enabled);
    assert_eq!(config.bluetooth_coc.poll_interval_ms, 30_000);
    assert!(config.bluetooth_coc.bridge_to_tcp);
    // Phase 4 (GAP-scan discovery) defaults to off until that phase
    // ships; locked in here so a future schema bump can't silently
    // flip it ahead of the discovery implementation.
    assert!(!config.bluetooth_coc.discovery_enabled);
    assert_eq!(config.bluetooth_coc.inquiry_interval_ms, 60_000);
    assert_eq!(config.bluetooth_coc.peer_bdaddr_type, 0x01);
}

#[test]
fn bluetooth_coc_enabled_round_trips() {
    let toml = r#"
[node]
name = "t"

[bluetooth_coc]
enabled = true
psm = 0x0091
device_name_prefix = "MESH-"
outbound_enabled = false
poll_interval_ms = 15000
bridge_to_tcp = false
discovery_enabled = true
inquiry_interval_ms = 45000
peer_bdaddr_type = 2
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert!(config.bluetooth_coc.enabled);
    assert_eq!(config.bluetooth_coc.psm, 0x0091);
    assert_eq!(config.bluetooth_coc.device_name_prefix, "MESH-");
    assert!(!config.bluetooth_coc.outbound_enabled);
    assert_eq!(config.bluetooth_coc.poll_interval_ms, 15_000);
    assert!(!config.bluetooth_coc.bridge_to_tcp);
    assert!(config.bluetooth_coc.discovery_enabled);
    assert_eq!(config.bluetooth_coc.inquiry_interval_ms, 45_000);
    assert_eq!(config.bluetooth_coc.peer_bdaddr_type, 0x02);

    let serialized = config.to_toml_string().unwrap();
    let reparsed = Config::from_toml_str(&serialized).unwrap();
    assert!(reparsed.bluetooth_coc.enabled);
    assert_eq!(reparsed.bluetooth_coc.psm, 0x0091);
    assert_eq!(reparsed.bluetooth_coc.peer_bdaddr_type, 0x02);
}

#[test]
fn bluetooth_rfcomm_enabled_round_trips() {
    let toml = r#"
[node]
name = "t"

[bluetooth_rfcomm]
enabled = true
channel = 23
device_name_prefix = "MESH-"
outbound_enabled = false
poll_interval_ms = 15000
bridge_to_tcp = false
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert!(config.bluetooth_rfcomm.enabled);
    assert_eq!(config.bluetooth_rfcomm.channel, 23);
    assert_eq!(config.bluetooth_rfcomm.device_name_prefix, "MESH-");
    assert!(!config.bluetooth_rfcomm.outbound_enabled);
    assert_eq!(config.bluetooth_rfcomm.poll_interval_ms, 15_000);
    assert!(!config.bluetooth_rfcomm.bridge_to_tcp);

    let serialized = config.to_toml_string().unwrap();
    let reparsed = Config::from_toml_str(&serialized).unwrap();
    assert!(reparsed.bluetooth_rfcomm.enabled);
    assert_eq!(reparsed.bluetooth_rfcomm.channel, 23);
    assert_eq!(reparsed.bluetooth_rfcomm.device_name_prefix, "MESH-");
    assert!(!reparsed.bluetooth_rfcomm.outbound_enabled);
    assert_eq!(reparsed.bluetooth_rfcomm.poll_interval_ms, 15_000);
    assert!(!reparsed.bluetooth_rfcomm.bridge_to_tcp);
}
