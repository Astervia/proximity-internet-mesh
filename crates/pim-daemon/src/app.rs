//! pim-daemon — multi-hop mesh daemon (Phase 2)
//!
//! Phase 2 additions over Phase 1:
//!   - Multi-peer session map (`HashMap<NodeId, Arc<Session>>`) (2.5)
//!   - Relay forwarding with TTL decrement and routing table lookup (2.1)
//!   - E2E encryption for internet-bound traffic (2.2)
//!   - Fragmentation / reassembly (2.4)
//!   - Distance-vector route advertisements (2.3)
//!   - Heartbeat keepalives
//!
//! Configuration (TOML):
//!
//! ```toml
//! [node]
//! name = "my-node"
//!
//! [interface]
//! mesh_ip  = "10.77.0.2/24"   # or "10.77.0.1/24" for the gateway
//! mtu      = 1400
//!
//! [transport]
//! listen_port = 9100
//!
//! [security]
//! key_file = "/etc/pim/node.key"
//!
//! [gateway]
//! enabled       = false        # set true on the gateway node
//! nat_interface = "eth0"
//!
//! [[peers]]
//! mechanism = "tcp"
//! address = "10.0.0.1:9100"   # peer's transport address
//! ```

#[path = "auth.rs"]
mod auth;
#[path = "bluetooth_env.rs"]
mod bluetooth_env;
#[path = "data_plane.rs"]
mod data_plane;
#[path = "discovery_tasks.rs"]
mod discovery_tasks;
#[path = "fs_util.rs"]
mod fs_util;
#[path = "gateway_tasks.rs"]
mod gateway_tasks;
#[path = "handshake.rs"]
mod handshake;
#[path = "ip_control.rs"]
mod ip_control;
#[path = "logs_subscriber.rs"]
mod logs_subscriber;
#[path = "net.rs"]
mod net;
#[path = "observability.rs"]
mod observability;
#[path = "peer_tasks.rs"]
mod peer_tasks;
#[path = "rate_limiter.rs"]
mod rate_limiter;
#[path = "reconnect.rs"]
mod reconnect;
#[path = "reconnect_task.rs"]
mod reconnect_task;
#[path = "reputation.rs"]
mod reputation;
#[path = "rpc.rs"]
mod rpc;
#[path = "runtime_config.rs"]
mod runtime_config;
#[path = "runtime_paths.rs"]
mod runtime_paths;
#[path = "send_buffer.rs"]
mod send_buffer;
#[path = "session.rs"]
mod session;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use bytes::BytesMut;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

#[cfg(test)]
use auth::AuthorizationDecision;
use auth::{expand_tilde, parse_discovery_shared_key, AuthorizationManager};
use bluetooth_env::*;
use data_plane::{
    reassemble_or_deliver, run_heartbeats, run_reassembly_gc, run_route_advertisements,
    send_mesh_data, send_single_mesh,
};
use discovery_tasks::{
    initiate_peer_connection, run_bluetooth_consumer, run_discovery_consumer,
    run_wifidirect_consumer,
};
use ed25519_dalek::VerifyingKey;
#[cfg(test)]
use gateway_tasks::PENDING_PING_TTL;
use gateway_tasks::{ensure_gateway_ipv6_engine, run_gateway_probes, run_gateway_return};
use handshake::{decode_handshake_wire, handshake_responder};
use ip_control::{
    apply_dynamic_ip_assignment, cancel_pending_outbound_for_ip, classify_ip_request,
    maybe_request_dynamic_ip, send_routed_control, IpRequestDisposition, PendingOutbound,
};
use net::{
    find_any_ipv6_uplink, lookup_interface_ipv4, lookup_interface_ipv6_with_retry,
    packet_ip_version, InternetGatewayLink,
};
#[cfg(test)]
use net::{ipv4_destination, parse_interface_ipv4_output};
#[cfg(test)]
use observability::{format_stats, StatsSnapshot};
use observability::{run_debug_snapshot_writer, run_stats_writer};
#[cfg(test)]
use peer_tasks::should_buffer_under_congestion;
use peer_tasks::{
    remove_peer, run_buffer_flush, run_buffer_gc, run_conntrack_gc, run_peer_liveness, send_control,
};
use pim_bluetooth::BluetoothDiscovery;
#[cfg(test)]
use pim_core::AuthorizationPolicy;
use pim_core::{Config, DiscoveryConfig, FrameCodec, NodeId};
use pim_crypto::{e2e_decrypt_in_place, e2e_encrypt, x25519_public_from_seed, Identity};
#[cfg(test)]
use pim_discovery::NodeCapabilities;
use pim_discovery::{DiscoveryService, PeerTable};
use pim_gateway::{GatewayEngine, GatewayEngineV6, IpPool};
use pim_protocol::{
    ControlFrame, DataFlags, FrameType, HandshakeFrameType, HandshakeWireFrame, HeartbeatFrame,
    MeshDataFrame, Reassembler, RouteUpdateFrame,
};
use pim_routing::{signing::verify_route_update, RoutingTable};
use pim_transport::{TcpTransport, Transport};
use pim_tun::TunInterface;
use pim_wifidirect::WifiDirectDiscovery;
use rate_limiter::{RateLimiter, DEFAULT_BURST, DEFAULT_RATE};
#[cfg(test)]
use reconnect::backoff_base_ms;
#[cfg(test)]
use reconnect::backoff_duration;
#[cfg(test)]
use reconnect::ConnectTarget;
use reconnect::ReconnectManager;
use reconnect_task::run_reconnect_task;
use reputation::ReputationTracker;
use runtime_config::{
    first_host_in_subnet, first_host_in_subnet_v6, icmp_echo_reply, install_signal_handler,
    node_capabilities, parse_cidr, parse_ipv6_cidr, resolve_configured_peer_targets,
};
use send_buffer::{SendBuffer, DEFAULT_CAPACITY, DEFAULT_TIMEOUT};
use session::Session;
#[cfg(test)]
use std::path::PathBuf;

// ── Shared daemon state ───────────────────────────────────────────────────────

type SessionMap = Arc<RwLock<HashMap<NodeId, Arc<Session>>>>;
/// Pending handshakes: maps peer_id → channel for routing incoming HS frames
type HsChannels = Arc<Mutex<HashMap<NodeId, mpsc::Sender<HandshakeWireFrame>>>>;

struct DaemonState {
    node_name: String,
    self_id: NodeId,
    identity: Arc<Identity>,
    is_gateway: bool,
    /// Our mesh-local IP (e.g. 10.77.0.1 for gateway). Stored as u32 to allow
    /// atomic update when a dynamic IP is assigned.
    mesh_ip: AtomicU32,
    /// Mesh prefix length used when deriving the first gateway host in the subnet.
    mesh_prefix_len: AtomicU8,
    /// Optional mesh IPv6 address and prefix configured on the TUN interface.
    mesh_ipv6: Arc<RwLock<Option<(Ipv6Addr, u8)>>>,
    /// Whether this node expects a dynamic mesh IP from a gateway.
    request_dynamic_ip: bool,
    /// Our own X25519 public key (set only when is_gateway = true).
    own_x25519_pub: [u8; 32],
    sessions: SessionMap,
    hs_channels: HsChannels,
    pending_outbound: Arc<Mutex<HashMap<IpAddr, PendingOutbound>>>,
    cancelled_outbounds: Arc<Mutex<HashSet<NodeId>>>,
    routing: Arc<Mutex<RoutingTable>>,
    /// Per-source reassembly buffers (keyed by sender NodeId).
    reassemblers: Arc<Mutex<HashMap<NodeId, Reassembler>>>,
    frag_id: Arc<AtomicU32>,
    transport: Arc<TcpTransport>,
    tun: Arc<TunInterface>,
    gw_engine: Option<Arc<GatewayEngine>>,
    gw_engine_v6: Arc<RwLock<Option<Arc<GatewayEngineV6>>>>,
    gateway_nat_interface: Option<String>,
    internet_link: Option<Arc<InternetGatewayLink>>,
    /// IP address pool — gateway only.
    ip_pool: Option<Arc<Mutex<IpPool>>>,
    /// Requesters currently being serviced for mesh IP assignment.
    pending_ip_assignments: Arc<Mutex<HashSet<NodeId>>>,
    /// Selected gateway for the current outstanding dynamic IP request, if any.
    pending_dynamic_ip_gateway: Arc<Mutex<Option<NodeId>>>,
    /// Last heartbeat received per peer, used for liveness detection.
    peer_last_hb: Arc<Mutex<HashMap<NodeId, Instant>>>,
    /// Reconnect manager for configured peers.
    reconnect: Arc<ReconnectManager>,
    /// Maximum reconnect attempts per target before giving up.
    max_reconnect_attempts: u32,
    /// Timeout for outbound TCP connect attempts.
    outbound_connect_timeout: Duration,
    /// Store-and-forward buffer for temporarily unreachable peers.
    send_buffer: Arc<SendBuffer>,
    /// Total data frames dropped due to peer send-queue congestion.
    congestion_drops: Arc<AtomicU64>,
    /// Total data packets forwarded (TUN → mesh or relay).
    packets_forwarded: Arc<AtomicU64>,
    /// Total bytes forwarded (payload bytes only).
    bytes_forwarded: Arc<AtomicU64>,
    /// Total packets dropped (TTL expired, no route, etc.).
    packets_dropped: Arc<AtomicU64>,
    /// Time when the daemon started (for uptime calculation).
    start_time: std::time::SystemTime,
    /// Pending gateway Ping probes: nonce → (gateway NodeId, sent_time).
    pending_pings: Arc<Mutex<HashMap<u64, (NodeId, Instant)>>>,
    /// Ed25519 verifying key (raw bytes) per peer — populated after handshake.
    peer_pubkeys: Arc<RwLock<HashMap<NodeId, [u8; 32]>>>,
    /// Per-peer token-bucket rate limiter.
    rate_limiter: Arc<Mutex<RateLimiter>>,
    /// Peer reputation tracker; drives automatic blacklisting.
    reputation: Arc<Mutex<ReputationTracker>>,
    authorization: Arc<AuthorizationManager>,
    cancel: CancellationToken,
    /// Discovery configuration — cloned at startup for use in the consumer task.
    discovery_config: DiscoveryConfig,
    /// Discovery peer table when the discovery service is enabled.
    discovery_peer_table: Option<Arc<Mutex<PeerTable>>>,
    /// On-disk path of `pim.toml`, retained for the JSON-RPC `config.get`
    /// / `config.save` surface so callers don't have to pass it back in.
    config_path: std::path::PathBuf,
    /// Whether split-default routing is currently engaged. Mutated by
    /// the JSON-RPC `route.set_split_default` handler; surfaced in
    /// `build_status`. NOTE: the actual packet-routing side-effect
    /// (re-pointing default route through the mesh) is not yet wired —
    /// this flag is purely state-tracking so the UI's RouteTogglePanel
    /// sees a coherent flip and the corresponding `status.event` fires.
    /// Wiring the forwarder to honour this is a follow-up.
    pub(crate) route_on: AtomicBool,
    /// Broadcast channel for JSON-RPC `status.event` notifications.
    /// Senders push complete notification objects (with `jsonrpc: "2.0"`,
    /// `method: "status.event"`, and `params: { kind, ... }`); each
    /// connection's `status.subscribe` forwarder pumps them into the
    /// per-connection writer.
    pub(crate) status_events_tx: tokio::sync::broadcast::Sender<serde_json::Value>,
}

impl DaemonState {
    fn next_frag_id(&self) -> u32 {
        self.frag_id.fetch_add(1, Ordering::Relaxed)
    }
}

// ── Main event loop ───────────────────────────────────────────────────────────

mod event_loop;

use event_loop::run_event_loop;

pub(crate) async fn run() -> Result<()> {
    // Compose two layers:
    //   - the existing fmt layer keeps stdout/stderr output identical
    //     to before (developer terminal experience unchanged).
    //   - logs_subscriber::LogsLayer fans out events to the JSON-RPC
    //     `logs.event` notification stream so the UI's Logs page
    //     receives live data.
    //
    // Default level is `info` when `RUST_LOG` is unset — without this
    // the daemon emits only ERROR events, and the UI's Logs view stays
    // empty on a healthy daemon. Devs can still override with
    // `RUST_LOG=debug` or `RUST_LOG=trace` for noisier debugging.
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer())
        .with(logs_subscriber::init())
        .init();

    let config_path = std::env::args().nth(1).unwrap_or_else(|| {
        runtime_paths::default_config_path()
            .to_string_lossy()
            .into_owned()
    });
    let pid_file = std::env::args().nth(2).unwrap_or_else(|| {
        runtime_paths::default_pid_file()
            .to_string_lossy()
            .into_owned()
    });

    let config =
        Config::load(std::path::Path::new(&config_path)).context("failed to load config")?;
    info!(config = %config_path, node = %config.node.name, "daemon starting");

    let mut pid_options = tokio::fs::OpenOptions::new();
    pid_options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        pid_options.mode(0o644);
    }
    let mut pid_f = pid_options
        .open(&pid_file)
        .await
        .context("failed to open PID file")?;
    use tokio::io::AsyncWriteExt;
    pid_f
        .write_all(std::process::id().to_string().as_bytes())
        .await
        .context("failed to write PID file")?;

    let key_path = expand_tilde(&config.security.key_file);
    let identity = Arc::new(
        Identity::load_or_generate(&key_path).context("failed to load/generate identity")?,
    );
    let self_id = identity.node_id();
    info!(%self_id, "identity loaded");

    let is_gateway = config.gateway.enabled;
    if is_gateway && !cfg!(any(target_os = "linux", target_os = "macos")) {
        bail!("gateway mode is only supported on Linux and macOS");
    }
    let own_x25519_pub = x25519_public_from_seed(&identity.signing_key().to_bytes());

    // ── TUN setup ──────────────────────────────────────────────────────────
    let mesh_cidr = config.interface.mesh_ip.clone();
    let request_dynamic_ip = mesh_cidr == "auto";
    let (mesh_ip, prefix_len) = if request_dynamic_ip {
        // Placeholder; real address will be assigned via IpAssign control frame.
        (Ipv4Addr::UNSPECIFIED, 24u8)
    } else {
        parse_cidr(&mesh_cidr)?
    };
    let mesh_ipv6 = config
        .interface
        .mesh_ipv6
        .as_deref()
        .map(parse_ipv6_cidr)
        .transpose()
        .context("invalid interface.mesh_ipv6")?;
    let tun = Arc::new(
        TunInterface::create(&config.interface.name).context("failed to create TUN interface")?,
    );
    if !request_dynamic_ip {
        tun.set_ip(mesh_ip, prefix_len).context("set TUN IP")?;
    }
    if let Some((mesh_ipv6_addr, mesh_ipv6_prefix)) = mesh_ipv6 {
        tun.set_ipv6(mesh_ipv6_addr, mesh_ipv6_prefix)
            .context("set TUN IPv6")?;
    }
    tun.set_mtu(config.interface.mtu).context("set TUN MTU")?;
    tun.up().context("bring TUN up")?;
    info!(iface = %tun.name(), addr = %mesh_ip, prefix = prefix_len, "TUN up");

    // ── Cancellation (created early so transport listeners honour it) ─────
    let cancel = CancellationToken::new();

    // ── Transport ─────────────────────────────────────────────────────────
    let listen_addr: SocketAddr = format!("0.0.0.0:{}", config.transport.listen_port)
        .parse()
        .context("invalid listen address")?;
    let transport = TcpTransport::new_with_cancel(listen_addr, self_id, cancel.clone())
        .await
        .context("failed to start TCP transport")?;
    info!(listen = %transport.listen_addr, "transport listening");

    // ── Gateway NAT setup ─────────────────────────────────────────────────
    let gateway_external_ip = if is_gateway {
        Some(
            lookup_interface_ipv4(&config.gateway.nat_interface).with_context(|| {
                format!(
                    "failed to resolve IPv4 address for {}",
                    config.gateway.nat_interface
                )
            })?,
        )
    } else {
        None
    };
    let gateway_external_ipv6 = if is_gateway {
        match lookup_interface_ipv6_with_retry(&config.gateway.nat_interface).await {
            Ok(ip) => Some(ip),
            Err(e) => match find_any_ipv6_uplink(&[&config.gateway.nat_interface, "pim0", "lo"]) {
                Some((iface, ip)) => {
                    info!(
                        configured = %config.gateway.nat_interface,
                        detected = %iface,
                        "configured nat_interface has no IPv6; using auto-detected uplink"
                    );
                    Some(ip)
                }
                None => {
                    warn!(
                        iface = %config.gateway.nat_interface,
                        "IPv6 gateway uplink unavailable: {e}"
                    );
                    None
                }
            },
        }
    } else {
        None
    };

    let gw_engine: Option<Arc<GatewayEngine>> = if is_gateway {
        let gw = Arc::new(GatewayEngine::new(
            gateway_external_ip.expect("gateway external IP set when gateway is enabled"),
            &config.gateway.nat_interface,
        ));
        let cidr = format!("{}/{}", mesh_ip, prefix_len);
        if let Err(e) = gw.setup_masquerade(&cidr) {
            warn!("iptables setup failed (may need root): {e}");
        }
        Some(gw)
    } else {
        None
    };
    let gw_engine_v6: Option<Arc<GatewayEngineV6>> = if is_gateway {
        gateway_external_ipv6.map(|external_ip| {
            let gw = Arc::new(GatewayEngineV6::new(
                external_ip,
                &config.gateway.nat_interface,
            ));
            if let Err(e) = gw.setup_masquerade() {
                warn!("ip6tables setup failed (may need root): {e}");
            }
            gw
        })
    } else {
        None
    };

    let internet_link: Option<Arc<InternetGatewayLink>> = if is_gateway {
        Some(Arc::new(
            InternetGatewayLink::new(&config.gateway.nat_interface).with_context(|| {
                format!(
                    "failed to initialize gateway internet link on {}",
                    config.gateway.nat_interface
                )
            })?,
        ))
    } else {
        None
    };

    // ── IP pool (gateway) ─────────────────────────────────────────────────
    let ip_pool: Option<Arc<Mutex<IpPool>>> = if is_gateway {
        // Network address is the subnet base (e.g. 10.77.0.0 for 10.77.0.1/24)
        let n = u32::from(mesh_ip);
        let mask: u32 = if prefix_len >= 32 {
            0xffff_ffff
        } else {
            !((1u32 << (32 - prefix_len)) - 1)
        };
        let network_addr = Ipv4Addr::from(n & mask);
        Some(Arc::new(Mutex::new(IpPool::new(network_addr, prefix_len))))
    } else {
        None
    };

    // ── Signal handler ────────────────────────────────────────────────────
    {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            install_signal_handler(cancel).await;
        });
    }

    // ── Build reconnect manager ───────────────────────────────────────────
    let configured_targets =
        resolve_configured_peer_targets(&config).context("failed to resolve configured peers")?;
    let reconnect = Arc::new(ReconnectManager::new(
        configured_targets.reconnect_targets.iter().copied(),
    ));
    let authorization = Arc::new(
        AuthorizationManager::new(
            config.security.authorization_policy.clone(),
            config.security.authorized_peers.iter().copied(),
            config.security.trust_store_file.clone(),
        )
        .context("build authorization manager")?,
    );
    let discovery_shared_key = config
        .discovery
        .shared_key
        .as_deref()
        .map(parse_discovery_shared_key)
        .transpose()
        .context("parse discovery shared key")?;

    let discovery_runtime = if config.discovery.enabled {
        let pubkey: [u8; 32] = identity.signing_key().verifying_key().to_bytes();
        let caps = node_capabilities(&config);
        let (discovery_svc, new_peer_rx) =
            DiscoveryService::new(self_id, pubkey, caps, config.transport.listen_port);
        let discovery_svc = discovery_svc
            .with_port(config.discovery.port)
            .with_broadcast_interval(Duration::from_millis(
                config.discovery.broadcast_interval_ms,
            ))
            .with_peer_timeout(Duration::from_millis(config.discovery.peer_timeout_ms));
        let discovery_svc = if let Some(key) = discovery_shared_key {
            discovery_svc.with_shared_key(key)
        } else {
            discovery_svc
        };
        let discovery_svc = Arc::new(discovery_svc);
        Some((discovery_svc, new_peer_rx))
    } else {
        None
    };

    // ── Build shared state ────────────────────────────────────────────────
    let mut routing_table = RoutingTable::new(self_id, is_gateway);
    if !request_dynamic_ip {
        routing_table.set_self_mesh_ip(mesh_ip);
    }
    let routing = Arc::new(Mutex::new(routing_table));
    let state = Arc::new(DaemonState {
        node_name: config.node.name.clone(),
        self_id,
        identity: identity.clone(),
        is_gateway,
        mesh_ip: AtomicU32::new(u32::from(mesh_ip)),
        mesh_prefix_len: AtomicU8::new(prefix_len),
        mesh_ipv6: Arc::new(RwLock::new(mesh_ipv6)),
        request_dynamic_ip,
        own_x25519_pub,
        sessions: Arc::new(RwLock::new(HashMap::new())),
        hs_channels: Arc::new(Mutex::new(HashMap::new())),
        pending_outbound: Arc::new(Mutex::new(HashMap::new())),
        cancelled_outbounds: Arc::new(Mutex::new(HashSet::new())),
        routing,
        reassemblers: Arc::new(Mutex::new(HashMap::new())),
        frag_id: Arc::new(AtomicU32::new(1)),
        transport,
        tun,
        gw_engine,
        gw_engine_v6: Arc::new(RwLock::new(gw_engine_v6)),
        gateway_nat_interface: is_gateway.then(|| config.gateway.nat_interface.clone()),
        internet_link,
        ip_pool,
        pending_ip_assignments: Arc::new(Mutex::new(HashSet::new())),
        pending_dynamic_ip_gateway: Arc::new(Mutex::new(None)),
        peer_last_hb: Arc::new(Mutex::new(HashMap::new())),
        reconnect,
        max_reconnect_attempts: config.transport.max_reconnect_attempts,
        outbound_connect_timeout: Duration::from_millis(config.transport.connect_timeout_ms),
        send_buffer: Arc::new(SendBuffer::new(DEFAULT_CAPACITY, DEFAULT_TIMEOUT)),
        congestion_drops: Arc::new(AtomicU64::new(0)),
        packets_forwarded: Arc::new(AtomicU64::new(0)),
        bytes_forwarded: Arc::new(AtomicU64::new(0)),
        packets_dropped: Arc::new(AtomicU64::new(0)),
        start_time: std::time::SystemTime::now(),
        pending_pings: Arc::new(Mutex::new(HashMap::new())),
        peer_pubkeys: Arc::new(RwLock::new(HashMap::new())),
        rate_limiter: Arc::new(Mutex::new(RateLimiter::new(DEFAULT_BURST, DEFAULT_RATE))),
        reputation: Arc::new(Mutex::new(ReputationTracker::new())),
        authorization,
        cancel: cancel.clone(),
        discovery_config: config.discovery.clone(),
        discovery_peer_table: discovery_runtime
            .as_ref()
            .map(|(discovery_svc, _)| discovery_svc.peer_table()),
        config_path: std::path::PathBuf::from(&config_path),
        route_on: AtomicBool::new(false),
        // 64-frame ring is enough for the typical bursts (interface up,
        // gateway selected, route on/off) without dropping; lagged
        // subscribers receive RecvError::Lagged and re-sync via the
        // next `status` RPC.
        status_events_tx: tokio::sync::broadcast::channel::<serde_json::Value>(64).0,
    });

    // ── Initiate connections to configured peers ───────────────────────────
    for target in configured_targets.startup_targets.iter().copied() {
        initiate_peer_connection(state.clone(), target).await;
    }

    // ── Discovery service ──────────────────────────────────────────────────
    if let Some((discovery_svc, new_peer_rx)) = discovery_runtime {
        info!(port = config.discovery.port, "discovery enabled");
        tokio::spawn({
            let svc = discovery_svc.clone();
            let c = cancel.clone();
            async move {
                svc.run(c).await.ok();
            }
        });
        tokio::spawn(run_discovery_consumer(state.clone(), new_peer_rx));
    } else {
        info!("discovery disabled by config");
    }

    // ── Wi-Fi Direct discovery ─────────────────────────────────────────────
    if config.wifi_direct.enabled {
        info!(
            interface = %config.wifi_direct.interface,
            "starting Wi-Fi Direct discovery"
        );
        let (wd_svc, addr_rx) = WifiDirectDiscovery::new(
            config.node.name.clone(),
            config.wifi_direct.clone(),
            config.transport.listen_port,
        );
        let c = cancel.clone();
        tokio::spawn(async move { wd_svc.run(c).await });
        tokio::spawn(run_wifidirect_consumer(state.clone(), addr_rx));
    } else {
        debug!("Wi-Fi Direct disabled by config");
    }

    // ── Bluetooth PAN discovery ────────────────────────────────────────────
    let mut bt_handle: Option<tokio::task::JoinHandle<()>> = None;
    if config.bluetooth.enabled {
        let mut bluetooth_config = config.bluetooth.clone();
        if bluetooth_config.local_alias.is_empty() {
            bluetooth_config.local_alias = format!(
                "{}{}",
                bluetooth_config.device_name_prefix, config.node.name
            );
        }
        #[cfg(target_os = "macos")]
        for message in macos_bluetooth_config_warnings(&bluetooth_config) {
            warn!("{message}");
        }
        let bluetooth_sysfs_root = bluetooth_sysfs_root();
        let bluetooth_ip_command = bluetooth_ip_command();
        let bluetoothctl_command = bluetoothctl_command();
        let bt_network_command = bt_network_command();
        let bluetooth_iptables_command = bluetooth_iptables_command();
        let bluetooth_dnsmasq_command = bluetooth_dnsmasq_command();
        let bluetooth_dhclient_command = bluetooth_dhclient_command();
        let bluetooth_resolv_conf_path = bluetooth_resolv_conf_path();
        let bluetooth_nat_interface = config
            .gateway
            .enabled
            .then(|| config.gateway.nat_interface.clone());
        info!(
            interface = %bluetooth_config.interface,
            connect_pan = bluetooth_config.connect_pan,
            serve_nap = bluetooth_config.serve_nap,
            nap_bridge = %bluetooth_config.nap_bridge,
            nap_bridge_addr = %bluetooth_config.nap_bridge_addr,
            dhcp_enabled = bluetooth_config.dhcp_enabled,
            request_dhcp = bluetooth_config.request_dhcp,
            static_peers = configured_targets.bluetooth_static_targets.len(),
            sysfs_root = %bluetooth_sysfs_root.display(),
            ip_command = %bluetooth_ip_command.display(),
            bluetoothctl_command = %bluetoothctl_command.display(),
            bt_network_command = %bt_network_command.display(),
            iptables_command = %bluetooth_iptables_command.display(),
            dnsmasq_command = %bluetooth_dnsmasq_command.display(),
            dhclient_command = %bluetooth_dhclient_command.display(),
            resolv_conf_path = %bluetooth_resolv_conf_path.display(),
            nat_interface = bluetooth_nat_interface.as_deref().unwrap_or("<disabled>"),
            local_alias = %bluetooth_config.local_alias,
            "starting Bluetooth PAN watcher"
        );
        let (bt_svc, addr_rx) = BluetoothDiscovery::new_with_system_paths(
            bluetooth_config,
            config.transport.listen_port,
            configured_targets.bluetooth_static_targets.clone(),
            bluetooth_sysfs_root,
            bluetooth_ip_command,
            bluetoothctl_command,
            bt_network_command,
            bluetooth_iptables_command,
            bluetooth_dnsmasq_command,
            bluetooth_dhclient_command,
            bluetooth_resolv_conf_path,
            bluetooth_nat_interface,
        )
        .context("failed to construct Bluetooth PAN watcher")?;
        let c = cancel.clone();
        let handle = tokio::spawn(async move {
            if let Err(err) = bt_svc.run(c).await {
                tracing::warn!("Bluetooth PAN watcher exited with error: {err}");
            }
        });
        bt_handle = Some(handle);
        tokio::spawn(run_bluetooth_consumer(state.clone(), addr_rx));
    } else {
        debug!("Bluetooth PAN disabled by config");
    }

    // ── Phase 7: Bluetooth RFCOMM auto-discovery (Linux-only impl) ─────────
    //
    // Independent of BT-PAN above. Binds RFCOMM channel 22, advertises
    // `PIM-<node>` identity, dials any paired peer whose name starts
    // with `PIM-`. Mac side pairs via the `pim-bt-rfcomm-mac` Swift
    // sidecar in the pim-ui Tauri bundle. macOS / Windows builds skip
    // this block entirely — the service returns UnsupportedPlatform.
    #[cfg(target_os = "linux")]
    let mut rfcomm_handle: Option<(
        pim_bluetooth::rfcomm::RfcommService,
        tokio::task::JoinHandle<()>,
    )> = None;
    #[cfg(target_os = "linux")]
    if config.bluetooth.enabled {
        use pim_bluetooth::rfcomm::{LocalIdentity, RfcommConfig, RfcommEvent, RfcommService};
        let local_identity = LocalIdentity {
            node_id_hex: identity.node_id().to_hex(),
            name: format!(
                "{}{}",
                config.bluetooth.device_name_prefix, config.node.name
            ),
            caps: {
                let mut caps = vec!["mesh-v1".to_string()];
                if config.gateway.enabled {
                    caps.push("gateway-v1".to_string());
                }
                caps
            },
        };
        let rfcomm_cfg = RfcommConfig {
            enabled: true,
            channel: pim_bluetooth::rfcomm::DEFAULT_CHANNEL,
            prefix: if config.bluetooth.device_name_prefix.is_empty() {
                pim_bluetooth::rfcomm::DEFAULT_PREFIX.to_string()
            } else {
                config.bluetooth.device_name_prefix.clone()
            },
            poll_interval: std::time::Duration::from_secs(30),
            outbound_enabled: true,
            // Bridge post-handshake bytes onto the existing TCP
            // transport listener via loopback so an RFCOMM peer looks
            // like a normal TCP peer to the rest of the kernel.
            local_bridge_addr: Some(std::net::SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                transport.listen_addr.port(),
            )),
        };
        let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(64);
        match RfcommService::start(rfcomm_cfg, local_identity, events_tx) {
            Ok(svc) => {
                info!("Bluetooth RFCOMM service started");
                let log_handle = tokio::spawn(async move {
                    while let Some(ev) = events_rx.recv().await {
                        match &ev {
                            RfcommEvent::Listening { channel } => {
                                info!(channel = *channel, "rfcomm listening")
                            }
                            RfcommEvent::Discovered {
                                bd_addr,
                                node_id,
                                name,
                                platform,
                                caps,
                                initiator,
                                ..
                            } => info!(
                                bd_addr = %bd_addr,
                                node_id = %&node_id[..16.min(node_id.len())],
                                name = %name,
                                platform = %platform,
                                caps = ?caps,
                                initiator = *initiator,
                                "rfcomm peer discovered"
                            ),
                            RfcommEvent::Lost { bd_addr, reason } => {
                                info!(bd_addr = %bd_addr, reason = %reason, "rfcomm peer lost")
                            }
                            RfcommEvent::OpenFailed {
                                bd_addr, reason, ..
                            } => {
                                debug!(bd_addr = %bd_addr, reason = %reason, "rfcomm open failed")
                            }
                            RfcommEvent::Error { code, message } => {
                                warn!(code = *code, message = %message, "rfcomm error")
                            }
                        }
                    }
                });
                rfcomm_handle = Some((svc, log_handle));
            }
            Err(e) => warn!("rfcomm service failed to start: {e}"),
        }
    }

    // ── Background tasks ──────────────────────────────────────────────────
    tokio::spawn(run_reassembly_gc(state.clone()));
    tokio::spawn(run_route_advertisements(state.clone()));
    tokio::spawn(run_heartbeats(state.clone()));
    tokio::spawn(run_peer_liveness(state.clone()));
    tokio::spawn(run_buffer_gc(state.clone()));
    tokio::spawn(run_buffer_flush(state.clone()));
    tokio::spawn(run_stats_writer(state.clone()));
    tokio::spawn(run_debug_snapshot_writer(state.clone()));
    // JSON-RPC 2.0 server (docs/RPC.md). Single tokio task that owns the
    // Unix listener; spawns one task per accepted connection. Survives
    // for the lifetime of the daemon.
    tokio::spawn(rpc::run_rpc_server(
        state.clone(),
        runtime_paths::rpc_socket_path(),
    ));
    tokio::spawn(run_gateway_probes(state.clone()));
    if state.is_gateway {
        tokio::spawn(run_gateway_return(state.clone()));
        tokio::spawn(run_conntrack_gc(state.clone()));
    }

    // ── Main event loop ───────────────────────────────────────────────────
    let event_result = run_event_loop(state).await;

    // Wait for the Bluetooth task to finish its teardown (bridge delete,
    // MASQUERADE removal, child kill) before the process exits. Bounded so
    // a wedged teardown can't hold the daemon open indefinitely.
    if let Some(handle) = bt_handle {
        match tokio::time::timeout(Duration::from_secs(10), handle).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!("Bluetooth task join error: {err}"),
            Err(_) => warn!("Bluetooth teardown did not complete within 10s; continuing shutdown"),
        }
    }

    event_result
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
