#![allow(dead_code)]

//! Default values for config deserialization.

use std::path::PathBuf;

pub(super) fn default_data_dir() -> PathBuf {
    PathBuf::from("~/.pim")
}

pub(super) fn default_interface_name() -> String {
    "pim0".into()
}

pub(super) fn default_mtu() -> u32 {
    1400
}

pub(super) fn default_mesh_ip() -> String {
    "auto".into()
}

pub(super) fn default_discovery_enabled() -> bool {
    true
}

pub(super) fn default_discovery_port() -> u16 {
    9101
}

pub(super) fn default_broadcast_interval_ms() -> u64 {
    5000
}

pub(super) fn default_peer_timeout_ms() -> u64 {
    30000
}

pub(super) fn default_connect_relays() -> bool {
    true
}

pub(super) fn default_connect_gateways() -> bool {
    true
}

pub(super) fn default_transport_type() -> String {
    "tcp".into()
}

pub(super) fn default_listen_port() -> u16 {
    9100
}

pub(super) fn default_max_reconnect_attempts() -> u32 {
    20
}

pub(super) fn default_connect_timeout_ms() -> u64 {
    3_000
}

pub(super) fn default_max_hops() -> u8 {
    10
}

pub(super) fn default_route_algorithm() -> String {
    "distance-vector".into()
}

pub(super) fn default_route_expiry_s() -> u64 {
    300
}

pub(super) fn default_nat_interface() -> String {
    "eth0".into()
}

pub(super) fn default_max_connections() -> u32 {
    200
}

pub(super) fn default_key_file() -> PathBuf {
    PathBuf::from("~/.pim/node.key")
}

pub(super) fn default_require_encryption() -> bool {
    true
}

pub(super) fn default_trust_store_file() -> PathBuf {
    PathBuf::from("~/.pim/trusted-peers.toml")
}

pub(super) fn default_wfd_interface() -> String {
    "wlan0".into()
}

pub(super) fn default_wfd_go_intent() -> u8 {
    7
}

pub(super) fn default_wfd_listen_channel() -> u8 {
    6
}

pub(super) fn default_wfd_op_channel() -> u8 {
    6
}

pub(super) fn default_wfd_connect_method() -> String {
    "pbc".into()
}

pub(super) fn default_bluetooth_interface() -> String {
    #[cfg(target_os = "macos")]
    {
        "bridge0".into()
    }

    #[cfg(not(target_os = "macos"))]
    {
        "auto".into()
    }
}

pub(super) fn default_bluetooth_radio_discovery_enabled() -> bool {
    true
}

pub(super) fn default_bluetooth_device_name_prefix() -> String {
    "PIM-".into()
}

pub(super) fn default_bluetooth_connect_pan() -> bool {
    true
}

pub(super) fn default_bluetooth_nap_bridge() -> String {
    "br-bt".into()
}

pub(super) fn default_bluetooth_nap_bridge_addr() -> String {
    "192.168.44.1/24".into()
}

pub(super) fn default_bluetooth_dhcp_enabled() -> bool {
    true
}

pub(super) fn default_bluetooth_dhcp_lease_time() -> String {
    "12h".into()
}

pub(super) fn default_bluetooth_request_dhcp() -> bool {
    true
}

pub(super) fn default_bluetooth_auto_discover_peers() -> bool {
    true
}

pub(super) fn default_bluetooth_poll_interval_ms() -> u64 {
    2_000
}

pub(super) fn default_bluetooth_scan_interval_ms() -> u64 {
    5_000
}

pub(super) fn default_bluetooth_peer_discovery_interval_ms() -> u64 {
    2_000
}

pub(super) fn default_bluetoothctl_timeout_s() -> u64 {
    15
}

pub(super) fn default_bluetooth_discoverable_timeout_s() -> u64 {
    180
}

pub(super) fn default_bluetooth_startup_timeout_ms() -> u64 {
    15_000
}

pub(super) fn default_bluetooth_rfcomm_channel() -> u8 {
    22
}

pub(super) fn default_bluetooth_rfcomm_outbound_enabled() -> bool {
    true
}

pub(super) fn default_bluetooth_rfcomm_poll_interval_ms() -> u64 {
    30_000
}

pub(super) fn default_bluetooth_rfcomm_bridge_to_tcp() -> bool {
    true
}
