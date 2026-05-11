//! Shared runtime debug snapshot models used by the daemon and CLI.

#![allow(missing_docs)]

use serde::{Deserialize, Serialize};

/// Versioned snapshot of live daemon state for debug/inspection commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugSnapshot {
    /// Snapshot schema version.
    pub version: u32,
    /// Unix timestamp when the snapshot was generated.
    pub generated_at_unix_secs: u64,
    /// Local node runtime information.
    pub node: DebugNodeSnapshot,
    /// High-level counters and rates exposed by the daemon.
    pub stats: DebugStatsSnapshot,
    /// Live connected peers.
    pub peers: Vec<DebugPeerSnapshot>,
    /// Peers observed via discovery, whether connected or not.
    pub discovered_peers: Vec<DebugDiscoveredPeerSnapshot>,
    /// Installed routes currently used for forwarding.
    pub routes: Vec<DebugRouteSnapshot>,
    /// Known gateways sorted by preference.
    pub gateways: Vec<DebugGatewaySnapshot>,
}

/// Local node runtime information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugNodeSnapshot {
    pub name: String,
    pub node_id: String,
    pub short_id: String,
    pub is_gateway: bool,
    pub mesh_ip: String,
    pub mesh_prefix_len: u8,
}

/// Common stats for operator-facing debug commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugStatsSnapshot {
    pub peers: usize,
    pub routes: usize,
    pub packets_forwarded: u64,
    pub bytes_forwarded: u64,
    pub packets_dropped: u64,
    pub congestion_drops: u64,
    pub conntrack_size: usize,
    pub uptime_secs: u64,
}

/// Live peer session information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugPeerSnapshot {
    pub node_id: String,
    pub short_id: String,
    pub addr: Option<String>,
    pub mechanism: Option<String>,
    pub direct: bool,
    pub configured: bool,
    pub discovered: bool,
    pub last_heartbeat_age_ms: Option<u64>,
}

/// Discovery-layer peer observation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugDiscoveredPeerSnapshot {
    pub node_id: String,
    pub short_id: String,
    pub addr: String,
    pub is_client: bool,
    pub is_relay: bool,
    pub is_gateway: bool,
    pub last_seen_age_ms: u64,
}

/// Installed route entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugRouteSnapshot {
    pub destination_id: String,
    pub destination_short_id: String,
    pub next_hop_id: String,
    pub next_hop_short_id: String,
    pub learned_from_id: String,
    pub learned_from_short_id: String,
    pub hops: u8,
    pub is_gateway: bool,
    pub gateway_load: u8,
    pub rtt_ms: Option<u32>,
    pub mesh_ip: Option<String>,
    pub age_ms: u64,
    pub next_hop_blacklisted: bool,
}

/// Gateway ranking details derived from installed routes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugGatewaySnapshot {
    pub node_id: String,
    pub short_id: String,
    pub next_hop_id: String,
    pub next_hop_short_id: String,
    pub hops: u8,
    pub gateway_load: u8,
    pub rtt_ms: Option<u32>,
    pub score: u32,
    pub selected: bool,
    pub mesh_ip: Option<String>,
}
