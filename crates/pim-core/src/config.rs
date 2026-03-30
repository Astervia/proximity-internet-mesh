//! Configuration structures shared by the CLI and daemon.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

impl Default for InterfaceConfig {
    fn default() -> Self {
        Self {
            name: default_interface_name(),
            mtu: default_mtu(),
            mesh_ip: default_mesh_ip(),
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
        }
    }
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self { enabled: false }
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

/// A statically configured peer (used for Phase 1 before discovery is active).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerConfig {
    /// TCP address to connect to, e.g. "192.168.1.1:9100".
    pub address: String,
    /// Optional human-readable label.
    #[serde(default)]
    pub label: String,
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

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            key_file: default_key_file(),
            require_encryption: default_require_encryption(),
        }
    }
}

impl Config {
    /// Load configuration from a TOML file.
    pub fn load(path: &Path) -> Result<Self, PimError> {
        let content = std::fs::read_to_string(path).map_err(PimError::Io)?;
        Self::from_str(&content)
    }

    /// Parse configuration from a TOML string.
    pub fn from_str(s: &str) -> Result<Self, PimError> {
        toml::from_str(s).map_err(|e| PimError::Config(e.to_string()))
    }

    /// Serialize configuration to a TOML string.
    pub fn to_toml_string(&self) -> Result<String, PimError> {
        toml::to_string_pretty(self).map_err(|e| PimError::Config(e.to_string()))
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

[discovery]
enabled = true
port = 9101
broadcast_interval_ms = 5000
peer_timeout_ms = 30000
connect_relays = true
connect_gateways = true

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

[wifi_direct]
enabled = false
interface = "wlan0"
go_intent = 7
listen_channel = 6
op_channel = 6
connect_method = "pbc"
"#;

    #[test]
    fn parse_minimal_config() {
        let config = Config::from_str(MINIMAL_CONFIG).unwrap();
        assert_eq!(config.node.name, "test-node");
        // Defaults should be applied
        assert_eq!(config.interface.name, "pim0");
        assert_eq!(config.interface.mtu, 1400);
        assert_eq!(config.routing.max_hops, 10);
        assert!(!config.gateway.enabled);
        assert!(config.security.require_encryption);
    }

    #[test]
    fn parse_full_config() {
        let config = Config::from_str(FULL_CONFIG).unwrap();
        assert_eq!(config.node.name, "my-device");
        assert_eq!(config.node.data_dir, PathBuf::from("/tmp/pim"));
        assert!(config.gateway.enabled);
        assert_eq!(config.gateway.nat_interface, "wlan0");
        assert_eq!(config.transport.listen_port, 9100);
    }

    #[test]
    fn config_round_trip() {
        let config = Config::from_str(FULL_CONFIG).unwrap();
        let serialized = config.to_toml_string().unwrap();
        let reparsed = Config::from_str(&serialized).unwrap();
        assert_eq!(config, reparsed);
    }

    #[test]
    fn invalid_toml_returns_error() {
        let result = Config::from_str("not valid toml {{{}}}");
        assert!(result.is_err());
        assert!(format!("{}", result.unwrap_err()).contains("config error"));
    }

    #[test]
    fn discovery_defaults_when_section_absent() {
        let config = Config::from_str(MINIMAL_CONFIG).unwrap();
        assert!(config.discovery.enabled);
        assert_eq!(config.discovery.port, 9101);
        assert_eq!(config.discovery.broadcast_interval_ms, 5000);
        assert_eq!(config.discovery.peer_timeout_ms, 30000);
        assert!(config.discovery.connect_relays);
        assert!(config.discovery.connect_gateways);
    }

    #[test]
    fn discovery_enabled_false_round_trips() {
        let toml = r#"
[node]
name = "t"
[discovery]
enabled = false
"#;
        let config = Config::from_str(toml).unwrap();
        assert!(!config.discovery.enabled);
        let serialized = config.to_toml_string().unwrap();
        let reparsed = Config::from_str(&serialized).unwrap();
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
        let config = Config::from_str(toml).unwrap();
        assert_eq!(config.discovery.port, 19101);
        let serialized = config.to_toml_string().unwrap();
        let reparsed = Config::from_str(&serialized).unwrap();
        assert_eq!(reparsed.discovery.port, 19101);
    }

    #[test]
    fn relay_config_defaults_to_disabled() {
        let config = Config::from_str(MINIMAL_CONFIG).unwrap();
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
        let config = Config::from_str(toml).unwrap();
        assert!(config.relay.enabled);
    }

    #[test]
    fn peers_section_is_optional() {
        let config = Config::from_str(MINIMAL_CONFIG).unwrap();
        assert!(config.peers.is_empty());
    }

    #[test]
    fn config_round_trip_with_all_discovery_fields() {
        let config = Config::from_str(FULL_CONFIG).unwrap();
        assert!(config.discovery.enabled);
        assert_eq!(config.discovery.port, 9101);
        assert!(config.discovery.connect_relays);
        assert!(config.discovery.connect_gateways);
        assert!(!config.relay.enabled);
        let serialized = config.to_toml_string().unwrap();
        let reparsed = Config::from_str(&serialized).unwrap();
        assert_eq!(config, reparsed);
    }

    #[test]
    fn wifi_direct_defaults_to_disabled() {
        let config = Config::from_str(MINIMAL_CONFIG).unwrap();
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
        let config = Config::from_str(toml).unwrap();
        assert!(config.wifi_direct.enabled);
        let serialized = config.to_toml_string().unwrap();
        let reparsed = Config::from_str(&serialized).unwrap();
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
        let config = Config::from_str(toml).unwrap();
        assert_eq!(config.wifi_direct.interface, "wlan1");
        assert_eq!(config.wifi_direct.go_intent, 12);
        assert_eq!(config.wifi_direct.connect_method, "pin:12345670");
    }

    #[test]
    fn wifi_direct_go_intent_default_is_neutral() {
        let config = Config::from_str(MINIMAL_CONFIG).unwrap();
        assert_eq!(config.wifi_direct.go_intent, 7);
    }

    #[test]
    fn config_round_trip_with_wifi_direct_section() {
        let config = Config::from_str(FULL_CONFIG).unwrap();
        assert!(!config.wifi_direct.enabled);
        assert_eq!(config.wifi_direct.interface, "wlan0");
        assert_eq!(config.wifi_direct.go_intent, 7);
        assert_eq!(config.wifi_direct.connect_method, "pbc");
        let serialized = config.to_toml_string().unwrap();
        let reparsed = Config::from_str(&serialized).unwrap();
        assert_eq!(config, reparsed);
    }
}
