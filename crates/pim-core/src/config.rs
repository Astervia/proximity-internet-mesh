use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::PimError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub node: NodeConfig,
    #[serde(default)]
    pub interface: InterfaceConfig,
    #[serde(default)]
    pub discovery: DiscoveryConfig,
    #[serde(default)]
    pub transport: TransportConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub gateway: GatewayConfig,
    #[serde(default)]
    pub security: SecurityConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeConfig {
    pub name: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InterfaceConfig {
    #[serde(default = "default_interface_name")]
    pub name: String,
    #[serde(default = "default_mtu")]
    pub mtu: u32,
    #[serde(default = "default_mesh_ip")]
    pub mesh_ip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiscoveryConfig {
    #[serde(default = "default_broadcast_interval_ms")]
    pub broadcast_interval_ms: u64,
    #[serde(default = "default_peer_timeout_ms")]
    pub peer_timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransportConfig {
    #[serde(default = "default_transport_type")]
    pub r#type: String,
    #[serde(default = "default_listen_port")]
    pub listen_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingConfig {
    #[serde(default = "default_max_hops")]
    pub max_hops: u8,
    #[serde(default = "default_route_algorithm")]
    pub algorithm: String,
    #[serde(default = "default_route_expiry_s")]
    pub route_expiry_s: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GatewayConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_nat_interface")]
    pub nat_interface: String,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecurityConfig {
    #[serde(default = "default_key_file")]
    pub key_file: PathBuf,
    #[serde(default = "default_require_encryption")]
    pub require_encryption: bool,
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

fn default_broadcast_interval_ms() -> u64 {
    5000
}

fn default_peer_timeout_ms() -> u64 {
    30000
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
            broadcast_interval_ms: default_broadcast_interval_ms(),
            peer_timeout_ms: default_peer_timeout_ms(),
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
broadcast_interval_ms = 5000
peer_timeout_ms = 30000

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
}
