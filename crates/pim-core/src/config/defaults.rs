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

/// Public anycast resolvers handed to the system resolver
/// (`resolvectl dns pim0 …`) when the daemon engages split-default
/// routing. Cloudflare 1.1.1.1 + 1.0.0.1 (privacy-conscious, fast,
/// global anycast) plus Google 8.8.8.8 as a third opinion so a
/// single-provider outage doesn't break name resolution while the
/// mesh is the only egress. Lifted from the previous hard-coded
/// `MESH_DNS_SERVERS` constant in `pim-daemon::route_installer` —
/// users who want to override (corporate split-DNS, pi-hole on the
/// gateway, IPv6-only resolvers) edit `[routing].dns_servers` and
/// the route installer picks them up on the next reconcile tick.
pub(super) fn default_dns_servers() -> Vec<String> {
    vec!["1.1.1.1".into(), "1.0.0.1".into(), "8.8.8.8".into()]
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

pub(super) fn default_bluetooth_rfcomm_peer_cleanup_enabled() -> bool {
    // Default ON. The mesh use case treats a paired peer that hasn't
    // been in contact for hours as gone; keeping the BlueZ paired list
    // tidy is more valuable than preserving a stale pairing the user
    // probably no longer wants. A user who manages pairings manually
    // can opt out via TOML or the Settings pane.
    true
}

pub(super) fn default_bluetooth_rfcomm_max_unreachable_lifetime_s() -> u64 {
    // 2 h. Tuned for mesh-peer churn rather than user-managed
    // pairings: a peer that has been unreachable for two hours is
    // almost certainly gone (battery, range, retired). Floor of 1 h
    // (`MIN_LIFETIME_S` in `rfcomm_cleanup.rs`) means the sweep is
    // never finer than a 2x margin on this default.
    7_200
}

pub(super) fn default_bluetooth_rfcomm_cleanup_interval_s() -> u64 {
    // 1 h. With a 2 h lifetime the sweep should fire at least twice
    // per stale-window so the latency between "peer ages out" and
    // "BlueZ pairing removed" is bounded by ~1 h.
    3_600
}
