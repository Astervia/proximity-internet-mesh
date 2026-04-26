//! Statically configured peer targets.

use serde::{Deserialize, Serialize};

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
