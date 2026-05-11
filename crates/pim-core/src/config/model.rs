//! Configuration data model.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[allow(unused_imports)]
use super::defaults::*;
use super::peer::PeerConfig;
use super::peer_cleanup::PeerCleanupConfig;

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
    /// Optional private-mesh membership settings. Absent or `mode = "open"` →
    /// open mesh (any peer can connect; default behaviour). When configured
    /// with `mode = "private"` and a passphrase, only peers that derive the
    /// same mesh secret can complete the handshake or decrypt discovery
    /// advertisements. See [`MeshConfig`] for the schema.
    #[serde(default)]
    pub mesh: MeshConfig,
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
    /// Bluetooth RFCOMM direct-channel discovery and TCP bridge settings.
    #[serde(default)]
    pub bluetooth_rfcomm: BluetoothRfcommConfig,
    /// User-to-user encrypted messaging settings, including broadcast policy.
    #[serde(default)]
    pub messaging: MessagingConfig,
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
///
/// Mesh addresses are derived deterministically from each node's
/// `NodeId` via [`pim_core::derive_mesh_ipv4`] / [`derive_mesh_ipv6`].
/// Configure only the mesh **prefix**; the per-node host bits come from
/// the derivation. Two daemons sharing a prefix will never collide on
/// IPv6 (`/64` is collision-free at PIM scale) and only rarely on IPv4
/// in small prefixes; collisions degrade gracefully — see the routing
/// docs.
pub struct InterfaceConfig {
    /// Requested interface name, for example `pim0`.
    #[serde(default = "default_interface_name")]
    pub name: String,
    /// Interface MTU in bytes.
    #[serde(default = "default_mtu")]
    pub mtu: u32,
    /// Mesh IPv4 prefix (e.g. `"10.77.0.0/16"`). Defaults to
    /// [`pim_core::DEFAULT_MESH_IPV4_PREFIX`]. Each node's host bits
    /// inside this prefix are derived from its `NodeId`.
    #[serde(default)]
    pub mesh_ipv4_prefix: Option<String>,
    /// Mesh IPv6 prefix (e.g. `"fd77::/64"`). Defaults to
    /// [`pim_core::DEFAULT_MESH_IPV6_PREFIX`]. `/64` recommended.
    #[serde(default)]
    pub mesh_ipv6_prefix: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
/// Mesh-membership mode. `Open` admits any authenticated peer (mode default
/// when `[mesh]` is absent). `Private` requires every peer to derive the
/// same mesh secret from a shared passphrase.
pub enum MeshMode {
    /// No mesh-level gating. Behaves identically to a daemon configured
    /// without a `[mesh]` section.
    #[default]
    Open,
    /// Passphrase-gated mesh: only peers that derive the same secret can
    /// complete the handshake or decrypt discovery advertisements.
    Private,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
/// Private-mesh membership configuration.
///
/// All fields are optional; an absent `[mesh]` section is treated as
/// `mode = "open"`. When `mode = "private"`, `passphrase` must be a
/// non-empty UTF-8 string. The passphrase is stretched once at daemon
/// startup via Argon2id (see [`MeshKdfConfig`]) into a 32-byte master,
/// then split via HKDF-SHA256 into purpose-bound sub-keys for discovery
/// encryption and handshake binding.
pub struct MeshConfig {
    /// Mesh mode. Defaults to [`MeshMode::Open`] (open mesh).
    #[serde(default)]
    pub mode: MeshMode,
    /// Passphrase used to derive the mesh secret. Required when
    /// `mode = "private"`. Empty string in private mode is rejected at
    /// startup. Ignored entirely when `mode = "open"`.
    #[serde(default)]
    pub passphrase: Option<String>,
    /// Optional cosmetic mesh label used by the UI / CLI. Also mixed into
    /// the Argon2id salt, so two meshes that happen to share a passphrase
    /// but use different `mesh_id` values derive distinct secrets and
    /// cannot interconnect. Renaming `mesh_id` therefore invalidates
    /// every existing peer connection — UI surfaces a warning.
    #[serde(default)]
    pub mesh_id: Option<String>,
    /// Argon2id parameters used to stretch the passphrase. The default
    /// targets ~100 ms on a desktop. Tunable for embedded targets.
    #[serde(default)]
    pub kdf: MeshKdfConfig,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
/// Argon2id KDF parameters used when stretching the mesh passphrase. The
/// derivation runs once at daemon startup and the result is cached, so
/// values can be conservative without affecting per-handshake latency.
pub struct MeshKdfConfig {
    /// Memory cost in KiB. Default 65536 (64 MiB).
    #[serde(default = "default_mesh_kdf_m_cost_kib")]
    pub m_cost_kib: u32,
    /// Number of iterations. Default 3.
    #[serde(default = "default_mesh_kdf_t_cost")]
    pub t_cost: u32,
    /// Parallelism. Default 1.
    #[serde(default = "default_mesh_kdf_p_cost")]
    pub p_cost: u32,
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
    /// Maximum reconnect attempts per peer before giving up.
    #[serde(default = "default_max_reconnect_attempts")]
    pub max_reconnect_attempts: u32,
    /// Timeout for outbound TCP connect attempts in milliseconds.
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    /// Periodic cleanup of `ReconnectManager.discovered_targets` —
    /// see [`PeerCleanupConfig`]. Discovered targets are
    /// `(NodeId, ConnectTarget)` pairs accumulated as the daemon
    /// learns peers from various discovery sources; without
    /// cleanup the set grows monotonically. Ephemeral defaults
    /// (7 days / 1 h).
    #[serde(default = "PeerCleanupConfig::ephemeral_default")]
    pub peer_cleanup: PeerCleanupConfig,
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
    /// DNS resolvers handed to the system resolver (`resolvectl dns
    /// pim0 …` on Linux/systemd-resolved) when split-default routing
    /// is engaged. Without this list the resolver keeps its
    /// DHCP-provided upstream — which becomes unreachable the moment
    /// the user disables their wifi/wired uplink, so `curl gmail.com`
    /// fails even though the IP path through the mesh is live.
    /// Defaults to a small public anycast set; override for corporate
    /// split-DNS (`["10.0.0.53"]`), pi-hole on the gateway
    /// (`["10.77.0.1"]`), or IPv6-only resolvers.
    #[serde(default = "default_dns_servers")]
    pub dns_servers: Vec<String>,
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
    /// Periodic cleanup of unreachable Wi-Fi Direct peers — see
    /// [`PeerCleanupConfig`]. The destructive action drops the
    /// peer's row from the in-daemon `wfd_peer_lifecycle` table.
    /// Bluetooth-flavoured defaults (2 h / 1 h) since the radio-
    /// layer reachability characteristics are similar.
    #[serde(default = "PeerCleanupConfig::bluetooth_default")]
    pub peer_cleanup: PeerCleanupConfig,
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
    /// Periodic cleanup of unreachable PAN-paired peers — see
    /// [`PeerCleanupConfig`]. Bluetooth-flavoured defaults (2 h /
    /// 1 h). The destructive action is `bluetoothctl remove
    /// <bd_addr>`. Coordinates with `[bluetooth_rfcomm.peer_cleanup]`
    /// via the BlueZ `Connected` check so an active session in
    /// either subsystem suppresses the unpair.
    #[serde(default = "PeerCleanupConfig::bluetooth_default")]
    pub peer_cleanup: PeerCleanupConfig,
}

/// Bluetooth RFCOMM direct-channel configuration.
///
/// RFCOMM is independent from Bluetooth PAN/NAP. It opens a Bluetooth RFCOMM
/// channel to paired devices whose names match `device_name_prefix`, exchanges
/// PIM identity frames, and can bridge the resulting byte stream to the local
/// TCP transport listener so the rest of the daemon sees a normal peer session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BluetoothRfcommConfig {
    /// Enable the Linux RFCOMM service. Defaults to `false` (opt-in).
    #[serde(default)]
    pub enabled: bool,
    /// RFCOMM channel to bind and dial.
    #[serde(default = "default_bluetooth_rfcomm_channel")]
    pub channel: u8,
    /// Prefix used to identify paired PIM peers by Bluetooth device name.
    #[serde(default = "default_bluetooth_device_name_prefix")]
    pub device_name_prefix: String,
    /// Enable outbound paired-device scanning and dialing.
    #[serde(default = "default_bluetooth_rfcomm_outbound_enabled")]
    pub outbound_enabled: bool,
    /// Poll interval for outbound paired-device scans, in milliseconds.
    #[serde(default = "default_bluetooth_rfcomm_poll_interval_ms")]
    pub poll_interval_ms: u64,
    /// Bridge established RFCOMM sessions to the local TCP transport listener.
    #[serde(default = "default_bluetooth_rfcomm_bridge_to_tcp")]
    pub bridge_to_tcp: bool,
    /// Periodic cleanup of unreachable paired peers — see
    /// [`PeerCleanupConfig`]. The destructive action is
    /// `bluetoothctl remove <bd_addr>`. Bluetooth-flavoured defaults:
    /// enabled, 2 h lifetime, 1 h sweep cadence.
    #[serde(default = "PeerCleanupConfig::bluetooth_default")]
    pub peer_cleanup: PeerCleanupConfig,
}

/// Messaging subsystem configuration. Exposes the
/// `[messaging.broadcast]` policy that governs how this node
/// advertises its identity across the mesh, plus a peer-cleanup
/// subsection that ages out stale entries from the daemon's
/// `peers_seen` identity keystore.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessagingConfig {
    /// Identity-broadcast policy for the multi-hop mesh.
    #[serde(default)]
    pub broadcast: BroadcastConfig,
    /// Periodic cleanup of unreachable mesh-identity entries in the
    /// daemon's `peers_seen` table — see [`PeerCleanupConfig`]. The
    /// destructive action drops the row + emits `peer_forgotten`.
    /// Mesh-identity defaults (90 days / daily): identity loss
    /// forces a fresh handshake on next contact (cheap), so a long
    /// horizon keeps the keystore from growing forever on a long-
    /// running node.
    #[serde(default = "PeerCleanupConfig::mesh_identity_default")]
    pub peer_cleanup: PeerCleanupConfig,
}

impl Default for MessagingConfig {
    fn default() -> Self {
        Self {
            broadcast: BroadcastConfig::default(),
            peer_cleanup: PeerCleanupConfig::mesh_identity_default(),
        }
    }
}

/// Identity-broadcast policy.
///
/// Broadcasts are routed `PeerInfo` control frames sent to every node
/// in the local routing table — they let multi-hop peers learn each
/// other's `node_id + x25519_pubkey` without an out-of-band exchange.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BroadcastConfig {
    /// Cadence between scheduled outbound broadcast cycles, in seconds.
    /// `None` (or omitted) disables the periodic task; one-shot
    /// `peers.broadcast_identity_now` calls still work. Values below
    /// `MIN_OUTGOING_INTERVAL_S` are rejected at the RPC layer.
    #[serde(default)]
    pub outgoing_interval_s: Option<u64>,
    /// When `true`, routed `PeerInfo` arrivals (the inbound side of a
    /// broadcast) are surfaced as `peer_seen` events and a discovered
    /// row in the UI. When `false`, the X25519 key is still cached
    /// (replies need it) but no event is emitted — useful on noisy
    /// meshes where the local node does not want surfaced discovery.
    #[serde(default = "default_broadcast_watch_incoming")]
    pub watch_incoming: bool,
    /// Minimum seconds that must elapse between accepted broadcasts
    /// from any single peer. Subsequent broadcasts from the same peer
    /// arriving sooner are dropped before the keystore is touched —
    /// a single misbehaving peer cannot flood the keystore or the UI.
    #[serde(default = "default_broadcast_min_peer_interval_s")]
    pub min_peer_interval_s: u64,
    /// Periodic cleanup of `state.broadcast_peer_last_seen` rate-
    /// limit entries — see [`PeerCleanupConfig`]. Ephemeral defaults
    /// (7 days / 1 h): the rate-limit map costs ~32 bytes per peer,
    /// so the horizon is generous; the goal is bounding worst-case
    /// growth on long-running nodes that see thousands of unique
    /// broadcasters.
    #[serde(default = "PeerCleanupConfig::ephemeral_default")]
    pub peer_cleanup: PeerCleanupConfig,
}

impl Default for BroadcastConfig {
    fn default() -> Self {
        Self {
            outgoing_interval_s: None,
            watch_incoming: default_broadcast_watch_incoming(),
            min_peer_interval_s: default_broadcast_min_peer_interval_s(),
            peer_cleanup: PeerCleanupConfig::ephemeral_default(),
        }
    }
}

impl BroadcastConfig {
    /// Floor enforced by the RPC layer when a non-`None`
    /// `outgoing_interval_s` is supplied. Below this we'd start
    /// hammering the mesh.
    pub const MIN_OUTGOING_INTERVAL_S: u64 = 30;
}

fn default_broadcast_watch_incoming() -> bool {
    true
}

fn default_broadcast_min_peer_interval_s() -> u64 {
    60
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

fn default_max_reconnect_attempts() -> u32 {
    20
}

fn default_connect_timeout_ms() -> u64 {
    3_000
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

fn default_dns_servers() -> Vec<String> {
    vec!["1.1.1.1".into(), "1.0.0.1".into(), "8.8.8.8".into()]
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
        "bridge0".into()
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

fn default_bluetooth_rfcomm_channel() -> u8 {
    22
}

fn default_bluetooth_rfcomm_outbound_enabled() -> bool {
    true
}

fn default_bluetooth_rfcomm_poll_interval_ms() -> u64 {
    30_000
}

fn default_bluetooth_rfcomm_bridge_to_tcp() -> bool {
    true
}

impl Default for InterfaceConfig {
    fn default() -> Self {
        Self {
            name: default_interface_name(),
            mtu: default_mtu(),
            mesh_ipv4_prefix: None,
            mesh_ipv6_prefix: None,
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

impl Default for MeshKdfConfig {
    fn default() -> Self {
        Self {
            m_cost_kib: default_mesh_kdf_m_cost_kib(),
            t_cost: default_mesh_kdf_t_cost(),
            p_cost: default_mesh_kdf_p_cost(),
        }
    }
}

fn default_mesh_kdf_m_cost_kib() -> u32 {
    65536
}

fn default_mesh_kdf_t_cost() -> u32 {
    3
}

fn default_mesh_kdf_p_cost() -> u32 {
    1
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            r#type: default_transport_type(),
            listen_port: default_listen_port(),
            max_reconnect_attempts: default_max_reconnect_attempts(),
            connect_timeout_ms: default_connect_timeout_ms(),
            peer_cleanup: PeerCleanupConfig::ephemeral_default(),
        }
    }
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            max_hops: default_max_hops(),
            algorithm: default_route_algorithm(),
            route_expiry_s: default_route_expiry_s(),
            dns_servers: default_dns_servers(),
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

impl Default for WifiDirectConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interface: default_wfd_interface(),
            go_intent: default_wfd_go_intent(),
            listen_channel: default_wfd_listen_channel(),
            op_channel: default_wfd_op_channel(),
            connect_method: default_wfd_connect_method(),
            peer_cleanup: PeerCleanupConfig::bluetooth_default(),
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
            peer_cleanup: PeerCleanupConfig::bluetooth_default(),
        }
    }
}

impl Default for BluetoothRfcommConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            channel: default_bluetooth_rfcomm_channel(),
            device_name_prefix: default_bluetooth_device_name_prefix(),
            outbound_enabled: default_bluetooth_rfcomm_outbound_enabled(),
            poll_interval_ms: default_bluetooth_rfcomm_poll_interval_ms(),
            bridge_to_tcp: default_bluetooth_rfcomm_bridge_to_tcp(),
            peer_cleanup: PeerCleanupConfig::bluetooth_default(),
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
