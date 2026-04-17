//! Configuration structures shared by the CLI and daemon.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::PimError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Top-level configuration loaded from the node TOML file.
pub struct Config {
    /// Identity and local state settings for this node.
    pub node: NodeConfig,
    /// TUN interface settings for the mesh dataplane.
    #[serde(default)]
    pub interface: InterfaceConfig,
    /// LAN peer discovery settings.
    #[serde(default)]
    pub discovery: DiscoveryConfig,
    /// Peer-to-peer transport settings.
    #[serde(default)]
    pub transport: TransportConfig,
    /// Route propagation and expiry settings.
    #[serde(default)]
    pub routing: RoutingConfig,
    /// Internet gateway behaviour and NAT settings.
    #[serde(default)]
    pub gateway: GatewayConfig,
    /// Relay forwarding settings. When enabled this node forwards traffic for other mesh peers.
    #[serde(default)]
    pub relay: RelayConfig,
    /// Key material and encryption policy settings.
    #[serde(default)]
    pub security: SecurityConfig,
    /// Wi-Fi Direct (IEEE 802.11 P2P) peer discovery and group formation settings.
    #[serde(default)]
    pub wifi_direct: WifiDirectConfig,
    /// Bluetooth PAN peer link monitoring and address handoff settings.
    #[serde(default)]
    pub bluetooth: BluetoothConfig,
    /// Statically configured peers. Optional — nodes can rely entirely on discovery when empty.
    #[serde(default)]
    pub peers: Vec<PeerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Node-local identity and filesystem settings.
pub struct NodeConfig {
    /// Human-readable node name used in logs and operator-facing output.
    pub name: String,
    /// Directory for persistent node state such as keys and runtime metadata.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Settings for the Linux TUN interface that carries mesh IP traffic.
pub struct InterfaceConfig {
    /// Requested interface name, for example `pim0`.
    #[serde(default = "default_interface_name")]
    pub name: String,
    /// Interface MTU in bytes.
    #[serde(default = "default_mtu")]
    pub mtu: u32,
    /// Mesh IPv4 address or the string `\"auto\"` to request assignment automatically.
    #[serde(default = "default_mesh_ip")]
    pub mesh_ip: String,
    /// Optional mesh IPv6 CIDR assigned to the local TUN interface.
    #[serde(default)]
    pub mesh_ipv6: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// UDP broadcast discovery timing and policy configuration.
pub struct DiscoveryConfig {
    /// Enable or disable the discovery service entirely. When `false` the daemon connects only
    /// to statically configured `[[peers]]`.
    #[serde(default = "default_discovery_enabled")]
    pub enabled: bool,
    /// UDP port used for sending and receiving discovery broadcasts.
    #[serde(default = "default_discovery_port")]
    pub port: u16,
    /// Interval between outgoing discovery broadcasts, in milliseconds.
    #[serde(default = "default_broadcast_interval_ms")]
    pub broadcast_interval_ms: u64,
    /// Time after which an unseen peer is considered stale, in milliseconds.
    #[serde(default = "default_peer_timeout_ms")]
    pub peer_timeout_ms: u64,
    /// Automatically initiate connections to discovered peers advertising relay capability.
    #[serde(default = "default_connect_relays")]
    pub connect_relays: bool,
    /// Automatically initiate connections to discovered peers advertising gateway capability.
    #[serde(default = "default_connect_gateways")]
    pub connect_gateways: bool,
    /// Optional 32-byte discovery group key encoded as 64 hex characters.
    #[serde(default)]
    pub shared_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
/// Settings that control whether this node acts as a relay, forwarding traffic for other peers.
pub struct RelayConfig {
    /// Enables relay forwarding when `true`. Gateway nodes are implicitly relays regardless of
    /// this setting.
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Wire transport configuration for direct peer connections.
pub struct TransportConfig {
    /// Transport backend name, currently `tcp`.
    #[serde(default = "default_transport_type")]
    pub r#type: String,
    /// Local port the transport listens on for inbound peer connections.
    #[serde(default = "default_listen_port")]
    pub listen_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Routing behaviour for propagating and aging route advertisements.
pub struct RoutingConfig {
    /// Maximum hop count accepted before a route is considered unusable.
    #[serde(default = "default_max_hops")]
    pub max_hops: u8,
    /// Routing algorithm identifier used for compatibility and diagnostics.
    #[serde(default = "default_route_algorithm")]
    pub algorithm: String,
    /// Lifetime of learned routes before expiry, in seconds.
    #[serde(default = "default_route_expiry_s")]
    pub route_expiry_s: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Settings that control whether this node can act as an internet gateway.
pub struct GatewayConfig {
    /// Enables gateway and NAT behaviour when `true`.
    #[serde(default)]
    pub enabled: bool,
    /// Name of the internet-facing interface used for masquerading.
    #[serde(default = "default_nat_interface")]
    pub nat_interface: String,
    /// Maximum number of concurrent tracked gateway connections.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Encryption policy and key storage configuration.
pub struct SecurityConfig {
    /// Path to the Ed25519 private key file for this node.
    #[serde(default = "default_key_file")]
    pub key_file: PathBuf,
    /// Whether unencrypted sessions should be rejected.
    #[serde(default = "default_require_encryption")]
    pub require_encryption: bool,
    /// Authorization policy applied after peer identity is authenticated.
    #[serde(default)]
    pub authorization_policy: AuthorizationPolicy,
    /// Explicitly authorized peers when `authorization_policy = "allow_list"`.
    #[serde(default)]
    pub authorized_peers: Vec<crate::NodeId>,
    /// Persistent trust store used by `trust_on_first_use`.
    #[serde(default = "default_trust_store_file")]
    pub trust_store_file: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
/// Direct-peer authorization policy.
pub enum AuthorizationPolicy {
    /// Admit any authenticated peer.
    #[default]
    AllowAll,
    /// Admit only peers listed in `authorized_peers`.
    AllowList,
    /// Admit authenticated peers on first contact and persist their identity locally.
    TrustOnFirstUse,
}

/// Wi-Fi Direct (IEEE 802.11 P2P) discovery and group negotiation configuration.
///
/// When `enabled = true` the daemon will start Wi-Fi Direct peer discovery via
/// `wpa_cli` and attempt to form P2P groups with discovered devices. Once a group
/// is established the resulting IP address is used to open a standard TCP transport
/// connection, so all existing security, routing, and gateway logic applies unchanged.
///
/// **Prerequisite:** `wpa_supplicant` compiled with P2P support must be running and
/// controlling the interface specified by `interface`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WifiDirectConfig {
    /// Enable Wi-Fi Direct peer discovery. Defaults to `false` (opt-in).
    #[serde(default)]
    pub enabled: bool,
    /// Physical Wi-Fi interface to use for P2P operations (e.g. `wlan0`).
    #[serde(default = "default_wfd_interface")]
    pub interface: String,
    /// Group Owner intent value (0–15). Higher values make this node more likely to
    /// become the Group Owner during negotiation. Default 7 (neutral).
    #[serde(default = "default_wfd_go_intent")]
    pub go_intent: u8,
    /// P2P listen channel number. Default 6.
    #[serde(default = "default_wfd_listen_channel")]
    pub listen_channel: u8,
    /// P2P operating channel number. Default 6.
    #[serde(default = "default_wfd_op_channel")]
    pub op_channel: u8,
    /// Connection method: `"pbc"` (push-button) or `"pin:<8-digit-pin>"`. Default `"pbc"`.
    #[serde(default = "default_wfd_connect_method")]
    pub connect_method: String,
}

/// Bluetooth PAN link-establishment configuration.
///
/// This mechanism is intentionally narrow: it does not replace the transport
/// layer and it does not manage pairing. Instead, it waits for a Bluetooth PAN
/// interface to appear, then learns peer IPs from the PAN neighbor table and
/// hands them to the daemon so the existing TCP transport and handshake logic
/// can connect normally. Static Bluetooth peers are configured under
/// `[[peers]]` with `mechanism = "bluetooth"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BluetoothConfig {
    /// Enable Bluetooth PAN link monitoring. Defaults to `false` (opt-in).
    #[serde(default)]
    pub enabled: bool,
    /// Preferred PAN-facing interface name or `"auto"` for runtime resolution.
    #[serde(default = "default_bluetooth_interface")]
    pub interface: String,
    /// Enable radio-level Bluetooth discovery and pairing for new peers.
    #[serde(default = "default_bluetooth_radio_discovery_enabled")]
    pub radio_discovery_enabled: bool,
    /// Prefix used to identify PIM peers by Bluetooth device name.
    #[serde(default = "default_bluetooth_device_name_prefix")]
    pub device_name_prefix: String,
    /// Local Bluetooth controller alias to advertise. Empty means derived from node name.
    #[serde(default)]
    pub local_alias: String,
    /// Allow outbound PAN/NAP connection attempts to discovered peers.
    #[serde(default = "default_bluetooth_connect_pan")]
    pub connect_pan: bool,
    /// Start and supervise a local Linux NAP server process.
    #[serde(default)]
    pub serve_nap: bool,
    /// Linux bridge/interface to expose through the local NAP server.
    #[serde(default = "default_bluetooth_nap_bridge")]
    pub nap_bridge: String,
    /// IPv4 address/CIDR assigned to `nap_bridge` when the daemon manages it.
    #[serde(default = "default_bluetooth_nap_bridge_addr")]
    pub nap_bridge_addr: String,
    /// Run a daemon-supervised DHCP server on `nap_bridge` when serving NAP.
    #[serde(default = "default_bluetooth_dhcp_enabled")]
    pub dhcp_enabled: bool,
    /// Explicit DHCP range (`start,end`). When unset, derived from `nap_bridge_addr`.
    #[serde(default)]
    pub dhcp_range: Option<String>,
    /// DHCP lease time passed to dnsmasq (e.g. `"12h"`, `"infinite"`).
    #[serde(default = "default_bluetooth_dhcp_lease_time")]
    pub dhcp_lease_time: String,
    /// Comma-separated DNS server list advertised to DHCP clients. When unset,
    /// inherited from the host's `/etc/resolv.conf` at runtime.
    #[serde(default)]
    pub dhcp_dns: Option<String>,
    /// Automatically request DHCP on the resolved PAN interface when acting as a
    /// Linux PAN client (`connect_pan = true`, `serve_nap = false`).
    #[serde(default = "default_bluetooth_request_dhcp")]
    pub request_dhcp: bool,
    /// Automatically discover peer IPs from the PAN interface neighbor table.
    #[serde(default = "default_bluetooth_auto_discover_peers")]
    pub auto_discover_peers: bool,
    /// Poll interval used while waiting for the PAN interface to become ready.
    #[serde(default = "default_bluetooth_poll_interval_ms")]
    pub poll_interval_ms: u64,
    /// Poll interval used for radio-level device scans.
    #[serde(default = "default_bluetooth_scan_interval_ms")]
    pub scan_interval_ms: u64,
    /// Poll interval used for automatic peer discovery after the interface is ready.
    #[serde(default = "default_bluetooth_peer_discovery_interval_ms")]
    pub peer_discovery_interval_ms: u64,
    /// Timeout for `bluetoothctl` operations, in seconds.
    #[serde(default = "default_bluetoothctl_timeout_s")]
    pub bluetoothctl_timeout_s: u64,
    /// How long the controller remains discoverable after startup.
    #[serde(default = "default_bluetooth_discoverable_timeout_s")]
    pub discoverable_timeout_s: u64,
    /// Maximum time to wait for the PAN interface to appear before giving up.
    #[serde(default = "default_bluetooth_startup_timeout_ms")]
    pub startup_timeout_ms: u64,
}

// Default value functions

fn default_data_dir() -> PathBuf {
    PathBuf::from("~/.pim")
}

fn default_interface_name() -> String {
    "pim0".into()
}

fn default_mtu() -> u32 {
    1400
}

fn default_mesh_ip() -> String {
    "auto".into()
}

fn default_discovery_enabled() -> bool {
    true
}

fn default_discovery_port() -> u16 {
    9101
}

fn default_broadcast_interval_ms() -> u64 {
    5000
}

fn default_peer_timeout_ms() -> u64 {
    30000
}

fn default_connect_relays() -> bool {
    true
}

fn default_connect_gateways() -> bool {
    true
}

fn default_transport_type() -> String {
    "tcp".into()
}

fn default_listen_port() -> u16 {
    9100
}

fn default_max_hops() -> u8 {
    10
}

fn default_route_algorithm() -> String {
    "distance-vector".into()
}

fn default_route_expiry_s() -> u64 {
    300
}

fn default_nat_interface() -> String {
    "eth0".into()
}

fn default_max_connections() -> u32 {
    200
}

fn default_key_file() -> PathBuf {
    PathBuf::from("~/.pim/node.key")
}

fn default_require_encryption() -> bool {
    true
}

fn default_trust_store_file() -> PathBuf {
    PathBuf::from("~/.pim/trusted-peers.toml")
}

fn default_wfd_interface() -> String {
    "wlan0".into()
}

fn default_wfd_go_intent() -> u8 {
    7
}

fn default_wfd_listen_channel() -> u8 {
    6
}

fn default_wfd_op_channel() -> u8 {
    6
}

fn default_wfd_connect_method() -> String {
    "pbc".into()
}

fn default_bluetooth_interface() -> String {
    #[cfg(target_os = "macos")]
    {
        return "bridge0".into();
    }

    #[cfg(not(target_os = "macos"))]
    {
        "auto".into()
    }
}

fn default_bluetooth_radio_discovery_enabled() -> bool {
    true
}

fn default_bluetooth_device_name_prefix() -> String {
    "PIM-".into()
}

fn default_bluetooth_connect_pan() -> bool {
    true
}

fn default_bluetooth_nap_bridge() -> String {
    "br-bt".into()
}

fn default_bluetooth_nap_bridge_addr() -> String {
    "192.168.44.1/24".into()
}

fn default_bluetooth_dhcp_enabled() -> bool {
    true
}

fn default_bluetooth_dhcp_lease_time() -> String {
    "12h".into()
}

fn default_bluetooth_request_dhcp() -> bool {
    true
}

fn default_bluetooth_auto_discover_peers() -> bool {
    true
}

fn default_bluetooth_poll_interval_ms() -> u64 {
    2_000
}

fn default_bluetooth_scan_interval_ms() -> u64 {
    5_000
}

fn default_bluetooth_peer_discovery_interval_ms() -> u64 {
    2_000
}

fn default_bluetoothctl_timeout_s() -> u64 {
    15
}

fn default_bluetooth_discoverable_timeout_s() -> u64 {
    180
}

fn default_bluetooth_startup_timeout_ms() -> u64 {
    15_000
}

impl Default for InterfaceConfig {
    fn default() -> Self {
        Self {
            name: default_interface_name(),
            mtu: default_mtu(),
            mesh_ip: default_mesh_ip(),
            mesh_ipv6: None,
        }
    }
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            enabled: default_discovery_enabled(),
            port: default_discovery_port(),
            broadcast_interval_ms: default_broadcast_interval_ms(),
            peer_timeout_ms: default_peer_timeout_ms(),
            connect_relays: default_connect_relays(),
            connect_gateways: default_connect_gateways(),
            shared_key: None,
        }
    }
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            r#type: default_transport_type(),
            listen_port: default_listen_port(),
        }
    }
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            max_hops: default_max_hops(),
            algorithm: default_route_algorithm(),
            route_expiry_s: default_route_expiry_s(),
        }
    }
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            nat_interface: default_nat_interface(),
            max_connections: default_max_connections(),
        }
    }
}

/// A statically configured peer target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerConfig {
    /// Optional human-readable label.
    #[serde(default)]
    pub label: String,
    /// Connection mechanism and endpoint details for this peer.
    #[serde(flatten)]
    pub endpoint: PeerEndpointConfig,
}

/// Mechanism-specific peer connection settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "mechanism", rename_all = "snake_case")]
pub enum PeerEndpointConfig {
    /// A directly reachable transport endpoint.
    Tcp {
        /// TCP address to connect to, e.g. "192.168.1.1:9100".
        address: String,
    },
    /// A peer expected to be reachable over a Bluetooth PAN link.
    Bluetooth {
        /// IPv4 or IPv6 address reachable on the Bluetooth PAN interface.
        ip: String,
    },
}

impl Default for WifiDirectConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interface: default_wfd_interface(),
            go_intent: default_wfd_go_intent(),
            listen_channel: default_wfd_listen_channel(),
            op_channel: default_wfd_op_channel(),
            connect_method: default_wfd_connect_method(),
        }
    }
}

impl Default for BluetoothConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interface: default_bluetooth_interface(),
            radio_discovery_enabled: default_bluetooth_radio_discovery_enabled(),
            device_name_prefix: default_bluetooth_device_name_prefix(),
            local_alias: String::new(),
            connect_pan: default_bluetooth_connect_pan(),
            serve_nap: false,
            nap_bridge: default_bluetooth_nap_bridge(),
            nap_bridge_addr: default_bluetooth_nap_bridge_addr(),
            dhcp_enabled: default_bluetooth_dhcp_enabled(),
            dhcp_range: None,
            dhcp_lease_time: default_bluetooth_dhcp_lease_time(),
            dhcp_dns: None,
            request_dhcp: default_bluetooth_request_dhcp(),
            auto_discover_peers: default_bluetooth_auto_discover_peers(),
            poll_interval_ms: default_bluetooth_poll_interval_ms(),
            scan_interval_ms: default_bluetooth_scan_interval_ms(),
            peer_discovery_interval_ms: default_bluetooth_peer_discovery_interval_ms(),
            bluetoothctl_timeout_s: default_bluetoothctl_timeout_s(),
            discoverable_timeout_s: default_bluetooth_discoverable_timeout_s(),
            startup_timeout_ms: default_bluetooth_startup_timeout_ms(),
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            key_file: default_key_file(),
            require_encryption: default_require_encryption(),
            authorization_policy: AuthorizationPolicy::default(),
            authorized_peers: Vec::new(),
            trust_store_file: default_trust_store_file(),
        }
    }
}

impl Config {
    /// Load configuration from a TOML file.
    pub fn load(path: &Path) -> Result<Self, PimError> {
        let content = std::fs::read_to_string(path).map_err(PimError::Io)?;
        content.parse()
    }

    /// Parse configuration from a TOML string.
    pub fn from_toml_str(s: &str) -> Result<Self, PimError> {
        s.parse()
    }

    /// Serialize configuration to a TOML string.
    pub fn to_toml_string(&self) -> Result<String, PimError> {
        toml::to_string_pretty(self).map_err(|e| PimError::Config(e.to_string()))
    }
}

impl FromStr for Config {
    type Err = PimError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        toml::from_str(s).map_err(|e| PimError::Config(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
