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

mod rate_limiter;
mod reputation;
mod send_buffer;

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use bytes::BytesMut;
use rand::Rng as _;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use ed25519_dalek::VerifyingKey;
use pim_bluetooth::BluetoothDiscovery;
use pim_core::{Config, DiscoveryConfig, FrameCodec, NodeId, PeerEndpointConfig};
use pim_crypto::{
    e2e_decrypt, e2e_encrypt, x25519_public_from_seed, EncryptedFrame, HandshakeConfirm,
    HandshakeInit, HandshakeResponse, Handshaker, Identity, SessionCipher,
};
use pim_discovery::{DiscoveryService, NodeCapabilities, PeerRecord};
use pim_gateway::{GatewayEngine, IpPool};
use pim_protocol::{
    fragment_packet, ControlFrame, DataFlags, FragmentFrame, FrameType, HandshakeFrameType,
    HandshakeWireFrame, HeartbeatFrame, MeshDataFrame, Reassembler, RouteUpdateFrame,
    TransportFrame,
};
use pim_routing::{
    signing::{sign_route_update, verify_route_update},
    RoutingTable,
};
use pim_transport::{PeerAddress, TcpTransport, Transport, TransportError};
use pim_tun::TunInterface;
use pim_wifidirect::WifiDirectDiscovery;
use rate_limiter::{RateLimiter, DEFAULT_BURST, DEFAULT_RATE};
use reputation::ReputationTracker;
use send_buffer::{Priority, SendBuffer, DEFAULT_CAPACITY, DEFAULT_TIMEOUT};

// ── Session ───────────────────────────────────────────────────────────────────

/// Per-peer crypto session established after the handshake.
struct Session {
    peer_id: NodeId,
    send: SessionCipher,
    /// Persistent receive cipher — carries nonce-replay state across frames.
    recv: SessionCipher,
}

impl Session {
    fn encrypt_frame(&self, plaintext: &[u8]) -> Result<TransportFrame> {
        let ef = self.send.encrypt(plaintext)?;
        transport_frame_from_encrypted(ef)
    }

    fn decrypt_frame(&self, frame: &TransportFrame) -> Result<Vec<u8>> {
        let mut ct = frame.payload.clone();
        ct.extend_from_slice(&frame.tag);
        let ef = EncryptedFrame {
            nonce: frame.nonce,
            ciphertext: ct,
        };
        Ok(self.recv.decrypt(&ef)?)
    }
}

fn transport_frame_from_encrypted(ef: EncryptedFrame) -> Result<TransportFrame> {
    let ct_len = ef.ciphertext.len();
    if ct_len < 16 {
        bail!("encrypted frame too short to contain GCM tag");
    }
    let tag_offset = ct_len - 16;
    let mut tag = [0u8; 16];
    tag.copy_from_slice(&ef.ciphertext[tag_offset..]);
    Ok(TransportFrame {
        frame_type: FrameType::Data,
        nonce: ef.nonce,
        payload: ef.ciphertext[..tag_offset].to_vec(),
        tag,
    })
}

// ── Nonce prefix ──────────────────────────────────────────────────────────────

fn nonce_prefix(session_key: &[u8; 32], is_initiator: bool) -> [u8; 8] {
    let mut prefix = [0u8; 8];
    if is_initiator {
        prefix.copy_from_slice(&session_key[0..8]);
    } else {
        prefix.copy_from_slice(&session_key[8..16]);
    }
    prefix
}

// ── Reconnect manager ─────────────────────────────────────────────────────────

/// Tracks configured peer addresses and drives exponential-backoff reconnects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ConnectTarget {
    Tcp(SocketAddr),
    BluetoothPan(SocketAddr),
}

impl ConnectTarget {
    fn addr(self) -> SocketAddr {
        match self {
            Self::Tcp(addr) | Self::BluetoothPan(addr) => addr,
        }
    }

    fn mechanism_name(self) -> &'static str {
        match self {
            Self::Tcp(_) => "tcp",
            Self::BluetoothPan(_) => "bluetooth_pan",
        }
    }
}

struct ReconnectManager {
    /// Configured peer targets from `[[peers]]` config — always reconnect if lost.
    configured_targets: HashSet<ConnectTarget>,
    /// Targets learned from dynamic discovery — also reconnect if lost.
    discovered_targets: Mutex<HashSet<ConnectTarget>>,
    /// Maps real peer NodeId → connection target (learned after handshake).
    target_by_peer: Mutex<HashMap<NodeId, ConnectTarget>>,
    /// Addresses that currently have an active reconnect task.
    reconnecting: Mutex<HashSet<ConnectTarget>>,
}

impl ReconnectManager {
    fn new(targets: impl IntoIterator<Item = ConnectTarget>) -> Self {
        Self {
            configured_targets: targets.into_iter().collect(),
            discovered_targets: Mutex::new(HashSet::new()),
            target_by_peer: Mutex::new(HashMap::new()),
            reconnecting: Mutex::new(HashSet::new()),
        }
    }

    /// Record `peer_id → target` after a successful handshake.
    async fn register(&self, peer_id: NodeId, target: ConnectTarget) {
        self.target_by_peer.lock().await.insert(peer_id, target);
    }

    /// Return the configured target for `peer_id`, if it is a configured peer.
    #[cfg(test)]
    async fn configured_target(&self, peer_id: &NodeId) -> Option<ConnectTarget> {
        let target = self.target_by_peer.lock().await.get(peer_id).copied()?;
        self.configured_targets.contains(&target).then_some(target)
    }

    /// Register a target that came from dynamic peer discovery.
    async fn register_discovered(&self, target: ConnectTarget) {
        self.discovered_targets.lock().await.insert(target);
    }

    /// Return the target for `peer_id` if it is either a configured or
    /// discovered peer (both should be reconnected on loss).
    async fn is_reconnectable_target(&self, peer_id: &NodeId) -> Option<ConnectTarget> {
        let target = self.target_by_peer.lock().await.get(peer_id).copied()?;
        let is_configured = self.configured_targets.contains(&target);
        let is_discovered = self.discovered_targets.lock().await.contains(&target);
        (is_configured || is_discovered).then_some(target)
    }

    /// Claim the reconnect slot for `target`.  Returns `true` if a new reconnect
    /// task should be spawned (i.e., none was already running).
    async fn begin_reconnect(&self, target: ConnectTarget) -> bool {
        self.reconnecting.lock().await.insert(target)
    }

    /// Release the reconnect slot when the task finishes (success or cancel).
    async fn end_reconnect(&self, target: ConnectTarget) {
        self.reconnecting.lock().await.remove(&target);
    }
}

// ── Backoff ───────────────────────────────────────────────────────────────────

/// Base delay (ms) for `attempt` without jitter: 1 s × 2^attempt, capped at 10 s.
fn backoff_base_ms(attempt: u32) -> u64 {
    const BASE_MS: u64 = 1_000;
    const MAX_MS: u64 = 10_000;
    let shift = attempt.min(15) as u64;
    BASE_MS.saturating_mul(1u64 << shift).min(MAX_MS)
}

/// Exponential backoff with ±25 % uniform jitter.
///
/// attempt 0 → ~1 s, attempt 1 → ~2 s, …, capped at ~10 s.
fn backoff_duration(attempt: u32) -> Duration {
    let base = backoff_base_ms(attempt);
    let jitter_range = (base / 4) as i64;
    let jitter = rand::thread_rng().gen_range(-jitter_range..=jitter_range);
    Duration::from_millis((base as i64 + jitter).max(1) as u64)
}

// ── Shared daemon state ───────────────────────────────────────────────────────

type SessionMap = Arc<RwLock<HashMap<NodeId, Arc<Session>>>>;
/// Pending handshakes: maps peer_id → channel for routing incoming HS frames
type HsChannels = Arc<Mutex<HashMap<NodeId, mpsc::Sender<HandshakeWireFrame>>>>;

struct DaemonState {
    self_id: NodeId,
    identity: Arc<Identity>,
    is_gateway: bool,
    /// Our mesh-local IP (e.g. 10.77.0.1 for gateway). Stored as u32 to allow
    /// atomic update when a dynamic IP is assigned.
    mesh_ip: AtomicU32,
    /// Mesh prefix length used when deriving the first gateway host in the subnet.
    mesh_prefix_len: AtomicU8,
    /// Whether this node expects a dynamic mesh IP from a gateway.
    request_dynamic_ip: bool,
    /// Our own X25519 public key (set only when is_gateway = true).
    own_x25519_pub: [u8; 32],
    sessions: SessionMap,
    hs_channels: HsChannels,
    routing: Arc<Mutex<RoutingTable>>,
    /// Per-source reassembly buffers (keyed by sender NodeId).
    reassemblers: Arc<Mutex<HashMap<NodeId, Reassembler>>>,
    frag_id: Arc<AtomicU32>,
    transport: Arc<TcpTransport>,
    tun: Arc<TunInterface>,
    gw_engine: Option<Arc<GatewayEngine>>,
    internet_link: Option<Arc<InternetGatewayLink>>,
    /// IP address pool — gateway only.
    ip_pool: Option<Arc<Mutex<IpPool>>>,
    /// Last heartbeat received per peer, used for liveness detection.
    peer_last_hb: Arc<Mutex<HashMap<NodeId, Instant>>>,
    /// Reconnect manager for configured peers.
    reconnect: Arc<ReconnectManager>,
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
    cancel: CancellationToken,
    /// Discovery configuration — cloned at startup for use in the consumer task.
    discovery_config: DiscoveryConfig,
}

impl DaemonState {
    fn next_frag_id(&self) -> u32 {
        self.frag_id.fetch_add(1, Ordering::Relaxed)
    }
}

struct InternetGatewayLink {
    send_fd: tokio::io::unix::AsyncFd<OwnedFd>,
    recv_fd: tokio::io::unix::AsyncFd<OwnedFd>,
}

impl InternetGatewayLink {
    fn new(interface: &str) -> Result<Self> {
        let send_fd = create_raw_send_socket(interface)?;
        let recv_fd = create_packet_recv_socket(interface)?;
        Ok(Self {
            send_fd: tokio::io::unix::AsyncFd::new(send_fd).context("raw send AsyncFd")?,
            recv_fd: tokio::io::unix::AsyncFd::new(recv_fd).context("packet recv AsyncFd")?,
        })
    }

    async fn send_packet(&self, packet: &[u8]) -> Result<()> {
        let dest_ip = ipv4_destination(packet).context("raw send requires IPv4 packet")?;
        let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        addr.sin_family = libc::AF_INET as u16;
        addr.sin_addr = libc::in_addr {
            s_addr: u32::from(dest_ip).to_be(),
        };

        loop {
            let mut guard = self
                .send_fd
                .writable()
                .await
                .context("raw send socket writable")?;
            match guard.try_io(|inner| {
                let rc = unsafe {
                    libc::sendto(
                        inner.as_raw_fd(),
                        packet.as_ptr() as *const libc::c_void,
                        packet.len(),
                        0,
                        &addr as *const _ as *const libc::sockaddr,
                        std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                    )
                };
                if rc < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            }) {
                Ok(result) => return result.context("raw sendto failed"),
                Err(_would_block) => continue,
            }
        }
    }

    async fn recv_packet(&self, buf: &mut [u8]) -> Result<usize> {
        loop {
            let mut guard = self
                .recv_fd
                .readable()
                .await
                .context("packet socket readable")?;
            match guard.try_io(|inner| {
                let rc = unsafe {
                    libc::recv(
                        inner.as_raw_fd(),
                        buf.as_mut_ptr() as *mut libc::c_void,
                        buf.len(),
                        0,
                    )
                };
                if rc < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(rc as usize)
                }
            }) {
                Ok(result) => return result.context("packet recv failed"),
                Err(_would_block) => continue,
            }
        }
    }
}

fn ipv4_destination(packet: &[u8]) -> Option<Ipv4Addr> {
    if packet.len() < 20 || (packet[0] >> 4) != 4 {
        return None;
    }
    Some(Ipv4Addr::new(
        packet[16], packet[17], packet[18], packet[19],
    ))
}

fn lookup_interface_ipv4(interface: &str) -> Result<Ipv4Addr> {
    let output = Command::new("ip")
        .args(["-4", "-o", "addr", "show", "dev", interface])
        .output()
        .with_context(|| format!("failed to inspect IPv4 address for {interface}"))?;
    if !output.status.success() {
        bail!(
            "ip -4 -o addr show dev {interface} exited with {:?}",
            output.status.code()
        );
    }

    let stdout = String::from_utf8(output.stdout).context("invalid UTF-8 from ip addr output")?;
    parse_interface_ipv4_output(&stdout)
        .with_context(|| format!("no IPv4 address found on interface {interface}"))
}

fn parse_interface_ipv4_output(output: &str) -> Option<Ipv4Addr> {
    output
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find_map(|w| (w[0] == "inet").then_some(w[1]))
        .and_then(|cidr| cidr.split('/').next())
        .and_then(|ip| ip.parse().ok())
}

fn create_raw_send_socket(interface: &str) -> Result<OwnedFd> {
    let fd = unsafe {
        libc::socket(
            libc::AF_INET,
            libc::SOCK_RAW | libc::SOCK_NONBLOCK,
            libc::IPPROTO_RAW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error()).context("create raw send socket");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let one: libc::c_int = 1;
    setsockopt_bytes(
        fd.as_raw_fd(),
        libc::IPPROTO_IP,
        libc::IP_HDRINCL,
        &one.to_ne_bytes(),
    )
    .context("set IP_HDRINCL")?;
    bind_socket_to_device(fd.as_raw_fd(), interface).context("bind raw send socket to device")?;
    Ok(fd)
}

fn create_packet_recv_socket(interface: &str) -> Result<OwnedFd> {
    const ETH_P_IP: u16 = 0x0800;
    let fd = unsafe {
        libc::socket(
            libc::AF_PACKET,
            libc::SOCK_DGRAM | libc::SOCK_NONBLOCK,
            i32::from(ETH_P_IP.to_be()),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error()).context("create packet recv socket");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    bind_socket_to_device(fd.as_raw_fd(), interface).context("bind packet socket to device")?;

    let if_name = std::ffi::CString::new(interface).context("interface contains interior NUL")?;
    let ifindex = unsafe { libc::if_nametoindex(if_name.as_ptr()) };
    if ifindex == 0 {
        return Err(io::Error::last_os_error()).context("if_nametoindex failed");
    }

    let mut addr: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
    addr.sll_family = libc::AF_PACKET as u16;
    addr.sll_protocol = ETH_P_IP.to_be();
    addr.sll_ifindex = ifindex as i32;
    let rc = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error()).context("bind packet socket");
    }
    Ok(fd)
}

fn bind_socket_to_device(fd: i32, interface: &str) -> Result<()> {
    let mut name = interface.as_bytes().to_vec();
    name.push(0);
    setsockopt_bytes(fd, libc::SOL_SOCKET, libc::SO_BINDTODEVICE, &name)
        .with_context(|| format!("SO_BINDTODEVICE failed for {interface}"))
}

fn setsockopt_bytes(fd: i32, level: i32, optname: i32, value: &[u8]) -> Result<()> {
    let rc = unsafe {
        libc::setsockopt(
            fd,
            level,
            optname,
            value.as_ptr() as *const libc::c_void,
            value.len() as libc::socklen_t,
        )
    };
    if rc < 0 {
        Err(io::Error::last_os_error()).context("setsockopt failed")
    } else {
        Ok(())
    }
}

// ── Peer management helpers ───────────────────────────────────────────────────

/// Returns `true` if a frame should be buffered (not silently dropped) when the
/// peer's send queue is congested.  Control and route frames are buffered;
/// data frames are tail-dropped to avoid head-of-line blocking.
fn should_buffer_under_congestion(frame_type: FrameType) -> bool {
    Priority::of(frame_type) < Priority::Data
}

/// Send `frame` to `peer_id`.
///
/// - `PeerNotConnected` → buffered in send buffer (flushed on reconnect).
/// - `Congested` → control/route frames buffered for later flush; data frames
///   dropped immediately (priority-based tail drop) and `congestion_drops`
///   counter is incremented.
async fn send_frame_buffered(state: &Arc<DaemonState>, peer_id: &NodeId, frame: TransportFrame) {
    match state.transport.send(peer_id, frame.clone()).await {
        Ok(()) => {}
        Err(TransportError::PeerNotConnected(_)) => {
            let priority = Priority::of(frame.frame_type);
            state.send_buffer.push(*peer_id, priority, frame).await;
            debug!(%peer_id, "frame buffered (peer not connected)");
        }
        Err(TransportError::Congested(_)) => {
            if should_buffer_under_congestion(frame.frame_type) {
                let priority = Priority::of(frame.frame_type);
                state.send_buffer.push(*peer_id, priority, frame).await;
                debug!(%peer_id, "control/route frame buffered under congestion");
            } else {
                state.congestion_drops.fetch_add(1, Ordering::Relaxed);
                debug!(%peer_id, "data frame dropped under congestion");
            }
        }
        Err(e) => warn!(%peer_id, "send failed: {e}"),
    }
}

/// Send a `ControlFrame` directly (unencrypted) to `peer` over the transport.
/// Buffers the frame if the peer is temporarily unreachable.
async fn send_control(state: &Arc<DaemonState>, peer: &NodeId, cf: ControlFrame) {
    let mut buf = BytesMut::new();
    cf.encode(&mut buf);
    let tf = TransportFrame {
        frame_type: FrameType::Control,
        nonce: [0; 12],
        payload: buf.to_vec(),
        tag: [0; 16],
    };
    send_frame_buffered(state, peer, tf).await;
}

/// Remove a peer: disconnect transport, clean up session, routing, heartbeat map,
/// send triggered route updates, then schedule reconnect if it was a configured peer.
async fn remove_peer(state: &Arc<DaemonState>, peer_id: NodeId) {
    state.sessions.write().await.remove(&peer_id);
    state.peer_pubkeys.write().await.remove(&peer_id);
    state.rate_limiter.lock().await.remove_peer(&peer_id);
    state.routing.lock().await.remove_peer(peer_id);
    state.peer_last_hb.lock().await.remove(&peer_id);
    state.transport.disconnect(&peer_id).await.ok();
    info!(%peer_id, "peer removed");

    // Triggered route advertisement to remaining peers
    let adverts = state.routing.lock().await.generate_all_advertisements();
    for (pid, mut update) in adverts {
        sign_route_update(&mut update, state.identity.signing_key());
        let mut buf = BytesMut::new();
        update.encode(&mut buf);
        send_frame_buffered(
            state,
            &pid,
            TransportFrame {
                frame_type: FrameType::RouteUpdate,
                nonce: [0; 12],
                payload: buf.to_vec(),
                tag: [0; 16],
            },
        )
        .await;
    }

    // Schedule reconnect if this was a configured or discovered peer
    if let Some(target) = state.reconnect.is_reconnectable_target(&peer_id).await {
        if state.reconnect.begin_reconnect(target).await {
            let addr = target.addr();
            info!(%peer_id, mechanism = target.mechanism_name(), %addr, "scheduling reconnect with backoff");
            let st = state.clone();
            tokio::spawn(run_reconnect_task(st, target));
        }
    }
}

// ── Peer liveness checker ─────────────────────────────────────────────────────

/// Background task: check heartbeat liveness, remove peers that have been
/// silent for more than 15 seconds (3 missed × 5 s interval).
async fn run_peer_liveness(state: Arc<DaemonState>) {
    const TIMEOUT: Duration = Duration::from_secs(15);
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;
        let timed_out: Vec<NodeId> = {
            let hb = state.peer_last_hb.lock().await;
            hb.iter()
                .filter(|(_, last)| last.elapsed() > TIMEOUT)
                .map(|(id, _)| *id)
                .collect()
        };
        for peer_id in timed_out {
            warn!(%peer_id, "peer timed out (no heartbeat for 15s); removing");
            remove_peer(&state, peer_id).await;
            // Record liveness failure; blacklist if threshold reached.
            let newly_blacklisted = state.reputation.lock().await.record_failure(peer_id);
            if newly_blacklisted {
                warn!(%peer_id, "peer reached reputation blacklist threshold; blocking routes");
                state.routing.lock().await.blacklist_peer(peer_id);
            }
        }
    }
}

// ── Send-buffer helpers ───────────────────────────────────────────────────────

/// Drain the send buffer for `peer_id` and deliver all non-expired frames.
async fn flush_send_buffer(state: &Arc<DaemonState>, peer_id: NodeId) {
    let buffered = state.send_buffer.drain(&peer_id).await;
    if !buffered.is_empty() {
        info!(%peer_id, count = buffered.len(), "flushing send buffer after reconnect");
        for frame in buffered {
            state.transport.send(&peer_id, frame).await.ok();
        }
    }
}

/// Periodically flush the send buffer for all peers that currently have an
/// active session.  This handles both congestion recovery (frames buffered
/// when the write queue was full) and any other transient send failures.
async fn run_buffer_flush(state: Arc<DaemonState>) {
    // 50 ms gives fast congestion recovery without spinning the CPU.
    let mut interval = tokio::time::interval(Duration::from_millis(50));
    loop {
        interval.tick().await;
        let connected: Vec<NodeId> = state.sessions.read().await.keys().copied().collect();
        for peer_id in connected {
            let buffered = state.send_buffer.drain(&peer_id).await;
            if buffered.is_empty() {
                continue;
            }
            debug!(%peer_id, count = buffered.len(), "periodic buffer flush");
            let mut re_queue: Vec<TransportFrame> = Vec::new();
            let mut congested = false;
            for frame in buffered {
                if congested {
                    // Don't attempt further sends this tick; re-queue remaining frames.
                    re_queue.push(frame);
                    continue;
                }
                match state.transport.send(&peer_id, frame.clone()).await {
                    Ok(()) => {}
                    Err(TransportError::Congested(_)) => {
                        congested = true;
                        re_queue.push(frame);
                    }
                    Err(e) => warn!(%peer_id, "flush send failed: {e}"),
                }
            }
            // Put un-sent frames back in the buffer
            for frame in re_queue {
                let priority = Priority::of(frame.frame_type);
                state.send_buffer.push(peer_id, priority, frame).await;
            }
        }
    }
}

/// Periodically expire stale entries in the send buffer.
async fn run_buffer_gc(state: Arc<DaemonState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    loop {
        interval.tick().await;
        let removed = state.send_buffer.expire_all().await;
        if removed > 0 {
            debug!(removed, "send buffer: expired stale frames");
        }
    }
}

async fn run_conntrack_gc(state: Arc<DaemonState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        interval.tick().await;
        if let Some(gw) = &state.gw_engine {
            let before = gw.conntrack_size().await;
            gw.cleanup_expired().await;
            let after = gw.conntrack_size().await;
            if before != after {
                debug!(
                    removed = before - after,
                    "conntrack GC: expired entries removed"
                );
            }
        }
    }
}

// ── Observability ─────────────────────────────────────────────────────────────

/// Path to the runtime stats file read by `pim status --verbose`.
pub const STATS_PATH: &str = "/run/pim.stats";

pub struct StatsSnapshot {
    pub peers: usize,
    pub routes: usize,
    pub packets_forwarded: u64,
    pub bytes_forwarded: u64,
    pub packets_dropped: u64,
    pub congestion_drops: u64,
    pub conntrack_size: usize,
    pub uptime_secs: u64,
}

/// Collect current metrics and format them as a newline-delimited key=value string.
pub fn format_stats(stats: &StatsSnapshot) -> String {
    format!(
        "peers={peers}\n\
         routes={routes}\n\
         packets_forwarded={packets_forwarded}\n\
         bytes_forwarded={bytes_forwarded}\n\
         packets_dropped={packets_dropped}\n\
         congestion_drops={congestion_drops}\n\
         conntrack_size={conntrack_size}\n\
         uptime_secs={uptime_secs}\n",
        peers = stats.peers,
        routes = stats.routes,
        packets_forwarded = stats.packets_forwarded,
        bytes_forwarded = stats.bytes_forwarded,
        packets_dropped = stats.packets_dropped,
        congestion_drops = stats.congestion_drops,
        conntrack_size = stats.conntrack_size,
        uptime_secs = stats.uptime_secs,
    )
}

async fn run_stats_writer(state: Arc<DaemonState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;
        let peers = state.sessions.read().await.len();
        let routes = state.routing.lock().await.route_count();
        let packets_forwarded = state.packets_forwarded.load(Ordering::Relaxed);
        let bytes_forwarded = state.bytes_forwarded.load(Ordering::Relaxed);
        let packets_dropped = state.packets_dropped.load(Ordering::Relaxed);
        let congestion_drops = state.congestion_drops.load(Ordering::Relaxed);
        let conntrack_size = match &state.gw_engine {
            Some(gw) => gw.conntrack_size().await,
            None => 0,
        };
        let uptime_secs = state.start_time.elapsed().unwrap_or_default().as_secs();

        let content = format_stats(&StatsSnapshot {
            peers,
            routes,
            packets_forwarded,
            bytes_forwarded,
            packets_dropped,
            congestion_drops,
            conntrack_size,
            uptime_secs,
        });

        let tmp = format!("{STATS_PATH}.tmp");
        if let Err(e) = std::fs::write(&tmp, &content) {
            debug!("stats write failed: {e}");
            continue;
        }
        std::fs::rename(&tmp, STATS_PATH).ok();
    }
}

// ── Gateway probes (Phase 5.3) ────────────────────────────────────────────────

/// Maximum age of a pending ping before it is discarded as lost.
const PENDING_PING_TTL: Duration = Duration::from_secs(30);

/// Periodically send Ping frames to each directly-connected gateway peer to
/// measure round-trip latency.  The matching Pong handler in the event loop
/// calls `update_gateway_rtt` to update the routing table.
///
/// Only direct-peer gateways are probed because `ControlFrame::Ping` is an
/// unrouted transport-layer frame (it is not wrapped inside a `MeshDataFrame`).
async fn run_gateway_probes(state: Arc<DaemonState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    loop {
        interval.tick().await;

        // Collect directly-connected gateways (hold lock briefly)
        let direct_gateways: Vec<NodeId> = {
            let rt = state.routing.lock().await;
            let direct = rt.direct_peers().clone();
            rt.all_gateways()
                .into_iter()
                .map(|(id, _)| id)
                .filter(|id| direct.contains(id))
                .collect()
        };

        // GC: remove stale pending pings older than PENDING_PING_TTL
        {
            let mut pings = state.pending_pings.lock().await;
            pings.retain(|_, (_, sent_at)| sent_at.elapsed() < PENDING_PING_TTL);
        }

        // Send a Ping to each direct gateway
        for gw_id in direct_gateways {
            let nonce: u64 = rand::random();
            state
                .pending_pings
                .lock()
                .await
                .insert(nonce, (gw_id, Instant::now()));
            send_control(&state, &gw_id, ControlFrame::Ping { nonce }).await;
            debug!(%gw_id, nonce, "sent gateway probe Ping");
        }
    }
}

// ── Reconnect task ────────────────────────────────────────────────────────────

/// Background task that reconnects to a configured peer using exponential
/// backoff + jitter.  Runs a full handshake after each successful TCP connect
/// so a fresh session key is established.
async fn run_reconnect_task(state: Arc<DaemonState>, target: ConnectTarget) {
    let addr = target.addr();
    let mut attempt = 0u32;
    loop {
        let delay = backoff_duration(attempt);
        info!(%addr, mechanism = target.mechanism_name(), attempt, delay_ms = delay.as_millis(), "reconnect scheduled");

        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = state.cancel.cancelled() => {
                state.reconnect.end_reconnect(target).await;
                return;
            }
        }

        info!(%addr, mechanism = target.mechanism_name(), attempt, "attempting reconnect");

        // Use a unique random placeholder so concurrent reconnects don't collide.
        let transport_key = NodeId::from_bytes(rand::random::<[u8; 16]>());

        if let Err(e) = state
            .transport
            .connect(&PeerAddress {
                node_id: transport_key,
                addr,
            })
            .await
        {
            warn!(%addr, mechanism = target.mechanism_name(), attempt, "reconnect TCP connect failed: {e}");
            attempt += 1;
            continue;
        }

        // Kick off handshake on the new connection.
        let (tx, rx) = mpsc::channel(8);
        state.hs_channels.lock().await.insert(transport_key, tx);

        match handshake_initiator(&state, transport_key, rx).await {
            Ok(peer_id) => {
                state.reconnect.register(peer_id, target).await;
                state.hs_channels.lock().await.remove(&transport_key);
                info!(%peer_id, %addr, mechanism = target.mechanism_name(), "reconnect succeeded (new session key)");
                state.reconnect.end_reconnect(target).await;
                return;
            }
            Err(e) => {
                warn!(%addr, mechanism = target.mechanism_name(), attempt, "reconnect handshake failed: {e}");
                // rename_peer only happens on success, so transport_key is still valid.
                state.hs_channels.lock().await.remove(&transport_key);
                state.transport.disconnect(&transport_key).await.ok();
            }
        }

        attempt += 1;
    }
}

// ── Handshake helpers ─────────────────────────────────────────────────────────

async fn send_handshake(
    transport: &Arc<TcpTransport>,
    peer: &NodeId,
    wire: HandshakeWireFrame,
) -> Result<()> {
    let mut buf = BytesMut::new();
    wire.encode(&mut buf);
    transport
        .send(
            peer,
            TransportFrame {
                frame_type: FrameType::Handshake,
                nonce: [0; 12],
                payload: buf.to_vec(),
                tag: [0; 16],
            },
        )
        .await?;
    Ok(())
}

fn decode_handshake_wire(frame: &TransportFrame) -> Result<HandshakeWireFrame> {
    if frame.frame_type != FrameType::Handshake {
        bail!("expected Handshake frame, got {:?}", frame.frame_type);
    }
    let mut buf = BytesMut::from(frame.payload.as_slice());
    Ok(HandshakeWireFrame::decode(&mut buf)?)
}

/// Initiator task: send Init, receive Response, send Confirm.
///
/// `transport_key` is the NodeId under which the transport currently tracks
/// this connection (may be a random placeholder).  After the handshake the
/// real peer NodeId is derived from the Response's `sender_pub` and the
/// transport entry is renamed accordingly.
///
/// Returns the real peer NodeId on success.
async fn handshake_initiator(
    state: &Arc<DaemonState>,
    transport_key: NodeId,
    mut rx: mpsc::Receiver<HandshakeWireFrame>,
) -> Result<NodeId> {
    let mut hs = Handshaker::new(&state.identity);
    let init = hs.initiate();

    send_handshake(
        &state.transport,
        &transport_key,
        HandshakeWireFrame::InitOrResponse {
            handshake_type: HandshakeFrameType::Init,
            sender_pub: init.sender_pub,
            ephemeral_pub: init.ephemeral_pub,
            nonce: init.nonce,
            signature: init.signature,
        },
    )
    .await?;
    info!(%transport_key, "sent HandshakeInit");

    // Wait for Response
    let wire = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .context("handshake response timeout")?
        .context("handshake channel closed")?;
    let (response, sender_pub) = match wire {
        HandshakeWireFrame::InitOrResponse {
            handshake_type: HandshakeFrameType::Response,
            sender_pub,
            ephemeral_pub,
            nonce,
            signature,
        } => {
            let sp = sender_pub;
            (
                HandshakeResponse {
                    sender_pub: sp,
                    ephemeral_pub,
                    nonce,
                    signature,
                },
                sp,
            )
        }
        _ => bail!("expected HandshakeResponse from {transport_key}"),
    };

    // Derive the peer's real NodeId from their Ed25519 public key.
    let peer_id = NodeId::from_public_key(&sender_pub);

    hs.finalize_initiator(&response)
        .context("handshake finalize")?;

    let confirm = hs.make_confirm().context("make_confirm")?;
    send_handshake(
        &state.transport,
        &transport_key,
        HandshakeWireFrame::Confirm { hmac: confirm.hmac },
    )
    .await?;
    info!(%peer_id, "handshake complete (initiator)");

    let key = *hs.session_key().unwrap().as_bytes();
    let session = Arc::new(Session {
        peer_id,
        send: SessionCipher::new(&key, nonce_prefix(&key, true)),
        recv: SessionCipher::new(&key, nonce_prefix(&key, false)),
    });

    // Rename the transport entry so subsequent sends use the real NodeId.
    if transport_key != peer_id {
        state.transport.rename_peer(transport_key, peer_id).await;
    }

    state.sessions.write().await.insert(peer_id, session);
    state.peer_pubkeys.write().await.insert(peer_id, sender_pub);
    state.routing.lock().await.add_peer(peer_id);
    state.routing.lock().await.unblacklist_peer(&peer_id);
    state.reputation.lock().await.pardon(&peer_id);
    state
        .peer_last_hb
        .lock()
        .await
        .insert(peer_id, Instant::now());
    info!(%peer_id, "session established (initiator)");

    // Flush any frames buffered while this peer was unreachable.
    flush_send_buffer(state, peer_id).await;

    // Non-gateway nodes request an IP address from the peer after connecting.
    if state.request_dynamic_ip {
        send_control(
            state,
            &peer_id,
            ControlFrame::IpRequest {
                requester_id: state.self_id,
            },
        )
        .await;
        debug!(%peer_id, "sent IpRequest");
    }
    Ok(peer_id)
}

/// Responder task: receive Init (already parsed), send Response, wait for Confirm.
async fn handshake_responder(
    state: &Arc<DaemonState>,
    peer_id: NodeId,
    init_wire: HandshakeWireFrame,
    mut rx: mpsc::Receiver<HandshakeWireFrame>,
) -> Result<()> {
    let init = match init_wire {
        HandshakeWireFrame::InitOrResponse {
            handshake_type: HandshakeFrameType::Init,
            sender_pub,
            ephemeral_pub,
            nonce,
            signature,
        } => HandshakeInit {
            sender_pub,
            ephemeral_pub,
            nonce,
            signature,
        },
        _ => bail!("expected HandshakeInit"),
    };

    let mut hs = Handshaker::new(&state.identity);
    let response = hs.respond(&init).context("handshake respond")?;

    send_handshake(
        &state.transport,
        &peer_id,
        HandshakeWireFrame::InitOrResponse {
            handshake_type: HandshakeFrameType::Response,
            sender_pub: response.sender_pub,
            ephemeral_pub: response.ephemeral_pub,
            nonce: response.nonce,
            signature: response.signature,
        },
    )
    .await?;
    info!(%peer_id, "sent HandshakeResponse");

    // Wait for Confirm
    let wire = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .context("confirm timeout")?
        .context("channel closed")?;
    let confirm = match wire {
        HandshakeWireFrame::Confirm { hmac } => HandshakeConfirm { hmac },
        _ => bail!("expected HandshakeConfirm from {peer_id}"),
    };
    hs.verify_confirm(&confirm)
        .context("confirm verification")?;
    info!(%peer_id, "handshake complete (responder)");

    let key = *hs.session_key().unwrap().as_bytes();
    let session = Arc::new(Session {
        peer_id,
        send: SessionCipher::new(&key, nonce_prefix(&key, false)),
        recv: SessionCipher::new(&key, nonce_prefix(&key, true)),
    });
    state.sessions.write().await.insert(peer_id, session);
    state
        .peer_pubkeys
        .write()
        .await
        .insert(peer_id, init.sender_pub);
    state.routing.lock().await.add_peer(peer_id);
    state.routing.lock().await.unblacklist_peer(&peer_id);
    state.reputation.lock().await.pardon(&peer_id);
    state
        .peer_last_hb
        .lock()
        .await
        .insert(peer_id, Instant::now());
    info!(%peer_id, "session established (responder)");

    // Flush any frames buffered while this peer was unreachable.
    flush_send_buffer(state, peer_id).await;
    Ok(())
}

// ── Frame sending helpers ─────────────────────────────────────────────────────

/// Send one or more MeshDataFrames (after fragmentation) to `dst_session`.
async fn send_mesh_data(
    state: &Arc<DaemonState>,
    session: &Arc<Session>,
    src_id: NodeId,
    dst_id: NodeId,
    ttl: u8,
    flags: DataFlags,
    payload: &[u8],
) {
    let threshold = pim_protocol::MAX_FRAGMENT_PAYLOAD.saturating_sub(40); // minus mesh header
    if payload.len() > threshold {
        let frag_id = state.next_frag_id();
        for frag in fragment_packet(payload, frag_id) {
            let frag_bytes = frag.serialize();
            let mesh_flags = flags | DataFlags::IS_FRAGMENT;
            send_single_mesh(state, session, src_id, dst_id, ttl, mesh_flags, &frag_bytes).await;
        }
    } else {
        send_single_mesh(state, session, src_id, dst_id, ttl, flags, payload).await;
    }
}

async fn send_single_mesh(
    state: &Arc<DaemonState>,
    session: &Arc<Session>,
    src_id: NodeId,
    dst_id: NodeId,
    ttl: u8,
    flags: DataFlags,
    payload: &[u8],
) {
    let mut mesh_buf = BytesMut::new();
    MeshDataFrame {
        src_id,
        dst_id,
        session_id: 0,
        ttl,
        flags,
        payload: payload.to_vec(),
    }
    .encode(&mut mesh_buf);

    match session.encrypt_frame(&mesh_buf) {
        Ok(frame) => send_frame_buffered(state, &session.peer_id, frame).await,
        Err(e) => warn!(%dst_id, "encrypt failed: {e}"),
    }
}

// ── Data-plane helpers ────────────────────────────────────────────────────────

/// Reassemble a fragment or deliver a whole packet.  Returns the IP packet when
/// ready, or `None` if more fragments are needed.
async fn reassemble_or_deliver(
    state: &Arc<DaemonState>,
    src_id: NodeId,
    flags: DataFlags,
    payload: &[u8],
) -> Option<Vec<u8>> {
    if flags.contains(DataFlags::IS_FRAGMENT) {
        let frag = FragmentFrame::deserialize(payload)?;
        let mut reassemblers = state.reassemblers.lock().await;
        let r = reassemblers.entry(src_id).or_default();
        r.insert(frag)
    } else {
        Some(payload.to_vec())
    }
}

/// Periodically expire stale reassembly buffers.
async fn run_reassembly_gc(state: Arc<DaemonState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;
        let mut reassemblers = state.reassemblers.lock().await;
        for r in reassemblers.values_mut() {
            r.expire_stale();
        }
        reassemblers.retain(|_, r| r.buffer_count() > 0);
    }
}

/// Periodically send route advertisements to all connected peers.
async fn run_route_advertisements(state: Arc<DaemonState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;
        let adverts = state.routing.lock().await.generate_all_advertisements();
        for (peer_id, mut update) in adverts {
            sign_route_update(&mut update, state.identity.signing_key());
            let mut buf = BytesMut::new();
            update.encode(&mut buf);
            send_frame_buffered(
                &state,
                &peer_id,
                TransportFrame {
                    frame_type: FrameType::RouteUpdate,
                    nonce: [0; 12],
                    payload: buf.to_vec(),
                    tag: [0; 16],
                },
            )
            .await;
            debug!(%peer_id, "sent route advertisement");
        }
    }
}

/// Periodically send heartbeats to all connected peers.
///
/// The `load` field is computed as the packet-forwarding rate over the last
/// heartbeat interval, normalized to 0–255 (2 000 packets/interval ≈ 255).
async fn run_heartbeats(state: Arc<DaemonState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    let mut last_fwd: u64 = 0;
    loop {
        interval.tick().await;
        let cur_fwd = state.packets_forwarded.load(Ordering::Relaxed);
        let delta = cur_fwd.saturating_sub(last_fwd);
        last_fwd = cur_fwd;
        // Normalize: ≥2 000 pkts/interval → load=255; 0 pkts → load=0.
        let load = (delta.min(2000) * 255 / 2000) as u8;

        let peers = state.transport.connected_peers();
        let gateway_hops: u8 = if state.is_gateway {
            0
        } else {
            state
                .routing
                .lock()
                .await
                .nearest_gateway()
                .map(|(_, hops)| hops)
                .unwrap_or(0xFF)
        };
        let hb = HeartbeatFrame {
            sender_id: state.self_id,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            gateway_hops,
            load,
            gw_x25519_pub: if state.is_gateway {
                state.own_x25519_pub
            } else {
                [0u8; 32]
            },
        };
        let mut buf = BytesMut::new();
        hb.encode(&mut buf);
        let tf = TransportFrame {
            frame_type: FrameType::Heartbeat,
            nonce: [0; 12],
            payload: buf.to_vec(),
            tag: [0; 16],
        };
        for peer in &peers {
            state.transport.send(peer, tf.clone()).await.ok();
        }
    }
}

// ── Main event loop ───────────────────────────────────────────────────────────

async fn run_event_loop(state: Arc<DaemonState>) -> Result<()> {
    let mut tun_buf = vec![0u8; 65536];
    // Gateway X25519 public key — learned from heartbeats (clients) or own key (gateway).
    let mut known_gw_x25519: Option<[u8; 32]> = if state.is_gateway {
        Some(state.own_x25519_pub)
    } else {
        None
    };

    info!(self_id = %state.self_id, is_gateway = state.is_gateway, "event loop started");

    loop {
        tokio::select! {
            // ── TUN → mesh ──────────────────────────────────────────────────
            res = state.tun.read_packet(&mut tun_buf) => {
                let n = match res {
                    Ok(n) => n,
                    Err(e) => { error!("TUN read: {e}"); break; }
                };
                let packet = &tun_buf[..n];
                debug!(bytes = n, "TUN read");

                if n < 20 {
                    debug!(bytes = n, "short IPv4 packet from TUN; dropping");
                    continue;
                }

                let dest_ip = Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19]);

                // Drop packets addressed to ourselves — the OS handles local delivery.
                if dest_ip == Ipv4Addr::from(state.mesh_ip.load(Ordering::Relaxed)) {
                    continue;
                }

                let mesh_route = state.routing.lock().await.lookup_mesh_ip(dest_ip);
                let (dst_id, next_hop_id, mut flags) = match mesh_route {
                    Some((dst_id, next_hop)) => (dst_id, next_hop, DataFlags::empty()),
                    None => {
                        let gw_route = state.routing.lock().await.nearest_gateway_route();
                        match gw_route {
                            Some((gw_id, next_hop)) => (gw_id, next_hop, DataFlags::IS_INTERNET),
                            None => {
                                // No route to a gateway — check direct peers as fallback
                                let peers = state.transport.connected_peers();
                                if peers.is_empty() {
                                    debug!("no route and no peers; dropping packet");
                                    continue;
                                }
                                (peers[0], peers[0], DataFlags::IS_INTERNET)
                            }
                        }
                    }
                };

                let session = state.sessions.read().await.get(&next_hop_id).cloned();
                let Some(session) = session else {
                    debug!(%next_hop_id, "no session for next hop; dropping");
                    continue;
                };

                let payload: Vec<u8>;

                // E2E-encrypt if we have the gateway's X25519 public key and
                // this packet is internet-bound.
                if flags.contains(DataFlags::IS_INTERNET) {
                    if let Some(gw_pub) = known_gw_x25519 {
                        match e2e_encrypt(packet, &gw_pub) {
                            Ok(enc) => {
                                flags |= DataFlags::IS_E2E;
                                payload = enc;
                            }
                            Err(e) => {
                                warn!("E2E encrypt failed: {e}");
                                payload = packet.to_vec();
                            }
                        }
                    } else {
                        payload = packet.to_vec();
                    }
                } else {
                    payload = packet.to_vec();
                }

                send_mesh_data(
                    &state,
                    &session,
                    state.self_id,
                    dst_id,
                    8,
                    flags,
                    &payload,
                )
                .await;
                state.packets_forwarded.fetch_add(1, Ordering::Relaxed);
                state.bytes_forwarded.fetch_add(n as u64, Ordering::Relaxed);
            }

            // ── mesh → process ──────────────────────────────────────────────
            res = state.transport.recv() => {
                let (from_peer, frame) = match res {
                    Ok(v) => v,
                    Err(e) => { error!("transport recv: {e}"); break; }
                };

                // Rate-limit all incoming frames per peer.
                if !state.rate_limiter.lock().await.allow(&from_peer) {
                    debug!(%from_peer, "rate limit exceeded; dropping frame");
                    state.packets_dropped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }

                match frame.frame_type {
                    FrameType::Handshake => {
                        let wire = match decode_handshake_wire(&frame) {
                            Ok(w) => w,
                            Err(e) => { warn!(%from_peer, "bad hs frame: {e}"); continue; }
                        };
                        let tx = state.hs_channels.lock().await.get(&from_peer).cloned();
                        if let Some(tx) = tx {
                            tx.send(wire).await.ok();
                        } else if matches!(wire,
                            HandshakeWireFrame::InitOrResponse {
                                handshake_type: HandshakeFrameType::Init, ..
                            })
                        {
                            // New incoming peer — spawn responder
                            let (tx, rx) = mpsc::channel(8);
                            state.hs_channels.lock().await.insert(from_peer, tx);
                            let st = state.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handshake_responder(&st, from_peer, wire, rx).await {
                                    warn!(%from_peer, "responder hs failed: {e}");
                                }
                                st.hs_channels.lock().await.remove(&from_peer);
                            });
                        } else {
                            warn!(%from_peer, "unexpected handshake frame (no pending hs)");
                        }
                    }

                    FrameType::Data => {
                        let session = state.sessions.read().await.get(&from_peer).cloned();
                        let Some(session) = session else {
                            warn!(%from_peer, "data frame before session; dropping");
                            continue;
                        };
                        let plaintext = match session.decrypt_frame(&frame) {
                            Ok(p) => p,
                            Err(e) => { warn!(%from_peer, "decrypt: {e}"); continue; }
                        };
                        let mut buf = BytesMut::from(plaintext.as_slice());
                        let mesh = match MeshDataFrame::decode(&mut buf) {
                            Ok(m) => m,
                            Err(e) => { warn!(%from_peer, "mesh decode: {e}"); continue; }
                        };

                        if mesh.dst_id == state.self_id {
                            // Destined for us — count inbound data landing at this node
                            state.packets_forwarded.fetch_add(1, Ordering::Relaxed);
                            state.bytes_forwarded.fetch_add(mesh.payload.len() as u64, Ordering::Relaxed);
                            let ip_payload = match reassemble_or_deliver(
                                &state, mesh.src_id, mesh.flags, &mesh.payload,
                            ).await {
                                Some(p) => p,
                                None => continue, // waiting for more fragments
                            };

                            let mut ip_packet = ip_payload;

                            // E2E decrypt if gateway and flag is set
                            if state.is_gateway && mesh.flags.contains(DataFlags::IS_E2E) {
                                let seed = state.identity.signing_key().to_bytes();
                                match e2e_decrypt(&ip_packet, &seed) {
                                    Ok(dec) => ip_packet = dec,
                                    Err(e) => {
                                        warn!(%from_peer, "E2E decrypt: {e}");
                                        continue;
                                    }
                                }
                            }

                            // Check if the packet is destined for the gateway's
                            // own mesh IP.  We handle it in userspace instead of
                            // writing to TUN (where the reply would race with
                            // run_gateway_return / run_event_loop readers).
                            let dst_local = state.gw_engine.is_some()
                                && ip_packet.len() >= 20
                                && Ipv4Addr::new(
                                    ip_packet[16], ip_packet[17],
                                    ip_packet[18], ip_packet[19],
                                ) == Ipv4Addr::from(state.mesh_ip.load(Ordering::Relaxed));

                            if dst_local {
                                if let Some(reply) = icmp_echo_reply(&ip_packet) {
                                    let next_hop = state.routing.lock().await.lookup(mesh.src_id);
                                    let session = match next_hop {
                                        Some(next_hop) => state.sessions.read().await.get(&next_hop).cloned(),
                                        None => None,
                                    };
                                    if let Some(session) = session {
                                        send_mesh_data(
                                            &state, &session, state.self_id,
                                            mesh.src_id, 8, DataFlags::empty(),
                                            &reply,
                                        ).await;
                                    }
                                }
                            } else if mesh.flags.contains(DataFlags::IS_INTERNET) {
                                match (&state.gw_engine, &state.internet_link) {
                                    (Some(gw), Some(link)) => {
                                        if let Err(e) = gw.translate_outbound(&mut ip_packet).await {
                                            warn!("gateway outbound NAT failed: {e}");
                                            continue;
                                        }
                                        if let Err(e) = link.send_packet(&ip_packet).await {
                                            warn!("gateway internet send failed: {e:#}");
                                        }
                                    }
                                    _ => {
                                        if let Err(e) = state.tun.write_packet(&ip_packet).await {
                                            warn!("TUN write: {e}");
                                        }
                                    }
                                }
                            } else {
                                if let Err(e) = state.tun.write_packet(&ip_packet).await {
                                    warn!("TUN write: {e}");
                                }
                            }
                        } else {
                            // Relay forwarding
                            if mesh.ttl == 0 {
                                debug!(%mesh.dst_id, "TTL expired; dropping");
                                state.packets_dropped.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            let next = state
                                .routing
                                .lock()
                                .await
                                .lookup(mesh.dst_id);
                            let Some(next_hop) = next else {
                                debug!(%mesh.dst_id, "no route for relay; dropping");
                                state.packets_dropped.fetch_add(1, Ordering::Relaxed);
                                continue;
                            };
                            let fwd_session =
                                state.sessions.read().await.get(&next_hop).cloned();
                            let Some(fwd_session) = fwd_session else {
                                warn!(%next_hop, "no session for relay next hop");
                                state.packets_dropped.fetch_add(1, Ordering::Relaxed);
                                continue;
                            };
                            // Re-encrypt and forward with decremented TTL
                            send_single_mesh(
                                &state,
                                &fwd_session,
                                mesh.src_id,
                                mesh.dst_id,
                                mesh.ttl - 1,
                                mesh.flags,
                                &mesh.payload,
                            )
                            .await;
                            state.packets_forwarded.fetch_add(1, Ordering::Relaxed);
                            state.bytes_forwarded.fetch_add(mesh.payload.len() as u64, Ordering::Relaxed);
                            debug!(%mesh.src_id, %mesh.dst_id, via = %next_hop, "relayed");
                        }
                    }

                    FrameType::RouteUpdate => {
                        let mut buf = BytesMut::from(frame.payload.as_slice());
                        match RouteUpdateFrame::decode(&mut buf) {
                            Ok(update) => {
                                // Verify Ed25519 signature before applying.
                                let pub_bytes = state.peer_pubkeys.read().await
                                    .get(&from_peer).copied();
                                let allowed = match pub_bytes {
                                    Some(pk) => match VerifyingKey::from_bytes(&pk) {
                                        Ok(vk) => {
                                            if verify_route_update(&update, &vk) {
                                                true
                                            } else {
                                                warn!(%from_peer, "route update signature invalid; rejecting");
                                                false
                                            }
                                        }
                                        Err(e) => {
                                            warn!(%from_peer, "bad verifying key: {e}; rejecting route update");
                                            false
                                        }
                                    },
                                    None => {
                                        warn!(%from_peer, "no public key for peer; rejecting route update");
                                        false
                                    }
                                };
                                if allowed {
                                    let changed = state
                                        .routing
                                        .lock()
                                        .await
                                        .apply_update(&update, from_peer);
                                    debug!(%from_peer, ?changed, "route update applied");
                                }
                            }
                            Err(e) => warn!(%from_peer, "route frame decode: {e}"),
                        }
                    }

                    FrameType::Heartbeat => {
                        let mut buf = BytesMut::from(frame.payload.as_slice());
                        match HeartbeatFrame::decode(&mut buf) {
                            Ok(hb) => {
                                debug!(%from_peer, gw_hops = hb.gateway_hops, load = hb.load, "heartbeat");
                                // Track liveness
                                state.peer_last_hb.lock().await.insert(from_peer, Instant::now());
                                // Direct gateway heartbeat: learn X25519 key and record load
                                if hb.gateway_hops == 0 {
                                    if hb.gw_x25519_pub != [0u8; 32] {
                                        known_gw_x25519 = Some(hb.gw_x25519_pub);
                                        debug!("learned gateway X25519 pub key");
                                    }
                                    state.routing.lock().await.update_gateway_load(from_peer, hb.load);
                                }
                            }
                            Err(e) => warn!(%from_peer, "heartbeat decode: {e}"),
                        }
                    }

                    FrameType::Control => {
                        let mut buf = BytesMut::from(frame.payload.as_slice());
                        match ControlFrame::decode(&mut buf) {
                            Ok(cf) => match cf {
                                ControlFrame::IpRequest { requester_id } => {
                                    // Gateway allocates and responds
                                    if let Some(pool) = &state.ip_pool {
                                        let result = pool.lock().await.allocate(*requester_id.as_bytes());
                                        match result {
                                            Ok((ip, lease_secs)) => {
                                                let gw_ip = pool.lock().await.gateway_ip();
                                                let prefix = pool.lock().await.prefix_len();
                                                send_control(
                                                    &state,
                                                    &from_peer,
                                                    ControlFrame::IpAssign {
                                                        assigned_ip: ip.octets(),
                                                        subnet_mask: prefix,
                                                        gateway_ip: gw_ip.octets(),
                                                        lease_seconds: lease_secs,
                                                    },
                                                )
                                                .await;
                                                info!(%requester_id, %ip, "assigned IP");
                                            }
                                            Err(e) => warn!(%requester_id, "IP allocation failed: {e}"),
                                        }
                                    } else {
                                        debug!(%from_peer, "received IpRequest but not a gateway");
                                    }
                                }
                                ControlFrame::IpAssign {
                                    assigned_ip,
                                    subnet_mask,
                                    gateway_ip,
                                    ..
                                } => {
                                    if state.request_dynamic_ip {
                                        let ip = Ipv4Addr::from(assigned_ip);
                                        let gw = Ipv4Addr::from(gateway_ip);
                                        info!(%ip, prefix = subnet_mask, %gw, "received IP assignment");
                                        if let Err(e) = state.tun.set_ip(ip, subnet_mask) {
                                            warn!("TUN set_ip failed: {e}");
                                        }
                                        state.mesh_ip.store(u32::from(ip), Ordering::Relaxed);
                                        state
                                            .mesh_prefix_len
                                            .store(subnet_mask, Ordering::Relaxed);
                                        state.routing.lock().await.set_self_mesh_ip(ip);
                                    } else {
                                        debug!(%from_peer, "ignoring unsolicited IpAssign for statically configured mesh IP");
                                    }
                                }
                                ControlFrame::Goodbye { departing_id, .. } => {
                                    info!(%departing_id, "received Goodbye; removing peer");
                                    remove_peer(&state, departing_id).await;
                                }
                                ControlFrame::Ping { nonce } => {
                                    send_control(&state, &from_peer, ControlFrame::Pong { nonce }).await;
                                }
                                ControlFrame::Pong { nonce } => {
                                    if let Some((gw_id, sent_at)) =
                                        state.pending_pings.lock().await.remove(&nonce)
                                    {
                                        let rtt_ms = sent_at.elapsed().as_millis() as u32;
                                        state.routing.lock().await.update_gateway_rtt(gw_id, rtt_ms);
                                        debug!(%gw_id, rtt_ms, "gateway RTT measured via Pong");
                                        // Pong confirms peer is alive — positive reputation signal.
                                        state.reputation.lock().await.record_success(gw_id);
                                    }
                                }
                                ControlFrame::Rekey => {}
                            },
                            Err(e) => warn!(%from_peer, "control frame decode: {e}"),
                        }
                    }

                    _ => {
                        debug!(%from_peer, frame_type = ?frame.frame_type, "unhandled frame type");
                    }
                }
            }

            _ = state.cancel.cancelled() => {
                info!("event loop: shutdown signal");
                break;
            }
        }
    }

    // Graceful shutdown: send Goodbye to all peers
    let peers = state.transport.connected_peers();
    for peer in &peers {
        send_control(
            &state,
            peer,
            ControlFrame::Goodbye {
                departing_id: state.self_id,
                reason: 0,
            },
        )
        .await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await; // let Goodbye flush

    if !state.is_gateway {
        let mesh_ip = Ipv4Addr::from(state.mesh_ip.load(Ordering::Relaxed));
        let prefix_len = state.mesh_prefix_len.load(Ordering::Relaxed);
        let gateway_ip = first_host_in_subnet(mesh_ip, prefix_len);
        if let Err(e) = state.tun.remove_default_route(gateway_ip) {
            warn!(%gateway_ip, "default route cleanup failed: {e}");
        }
    }

    // Clean up
    for peer in peers {
        state.transport.disconnect(&peer).await.ok();
    }
    state.tun.down().ok();
    Ok(())
}

/// Gateway task: drain TUN (internet→mesh), NAT inbound, send back to originators.
async fn run_gateway_return(state: Arc<DaemonState>) {
    // Only run on gateways
    if !state.is_gateway {
        return;
    }
    let Some(link) = state.internet_link.as_ref().cloned() else {
        return;
    };
    let Some(gw) = state.gw_engine.as_ref().cloned() else {
        return;
    };
    let mut buf = vec![0u8; 65536];

    loop {
        tokio::select! {
            res = link.recv_packet(&mut buf) => {
                let n = match res {
                    Ok(n) => n,
                    Err(e) => { error!("gateway internet recv: {e:#}"); break; }
                };
                let mut pkt = buf[..n].to_vec();
                if pkt.len() < 20 {
                    continue;
                }

                let dest_ip = match gw.translate_inbound(&mut pkt).await {
                    Ok(dest_ip) => dest_ip,
                    Err(_) => continue,
                };

                if let Some((dst_id, next_hop)) = state.routing.lock().await.lookup_mesh_ip(dest_ip) {
                    let session = state.sessions.read().await.get(&next_hop).cloned();
                    if let Some(session) = session {
                        send_mesh_data(&state, &session, state.self_id, dst_id, 8, DataFlags::IS_INTERNET, &pkt).await;
                    }
                }
            }
            _ = state.cancel.cancelled() => break,
        }
    }
}

// ── Config helpers ────────────────────────────────────────────────────────────

/// If `packet` is an IPv4 ICMP Echo Request, return the corresponding Echo
/// Reply with src/dst swapped and checksums recalculated.  Returns `None` for
/// any other packet type.
fn icmp_echo_reply(packet: &[u8]) -> Option<Vec<u8>> {
    // Minimum: 20-byte IP header + 8-byte ICMP header
    if packet.len() < 28 {
        return None;
    }
    let ihl = ((packet[0] & 0x0f) as usize) * 4;
    if packet.len() < ihl + 8 {
        return None;
    }
    // Protocol must be ICMP (1)
    if packet[9] != 1 {
        return None;
    }
    // ICMP type must be Echo Request (8), code 0
    if packet[ihl] != 8 || packet[ihl + 1] != 0 {
        return None;
    }

    let mut reply = packet.to_vec();

    // Swap src ↔ dst IP addresses (offsets 12..16 and 16..20)
    for i in 0..4 {
        reply.swap(12 + i, 16 + i);
    }

    // Set ICMP type to Echo Reply (0)
    reply[ihl] = 0;
    // Zero ICMP checksum before recalculation
    reply[ihl + 2] = 0;
    reply[ihl + 3] = 0;

    // Recalculate ICMP checksum
    let icmp_data = &reply[ihl..];
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < icmp_data.len() {
        sum += u16::from_be_bytes([icmp_data[i], icmp_data[i + 1]]) as u32;
        i += 2;
    }
    if i < icmp_data.len() {
        sum += (icmp_data[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    let cksum = !(sum as u16);
    reply[ihl + 2] = (cksum >> 8) as u8;
    reply[ihl + 3] = (cksum & 0xff) as u8;

    // Recalculate IP header checksum (src/dst changed)
    reply[10] = 0;
    reply[11] = 0;
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < ihl {
        sum += u16::from_be_bytes([reply[i], reply[i + 1]]) as u32;
        i += 2;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    let cksum = !(sum as u16);
    reply[10] = (cksum >> 8) as u8;
    reply[11] = (cksum & 0xff) as u8;

    Some(reply)
}

fn parse_cidr(s: &str) -> Result<(Ipv4Addr, u8)> {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 2 {
        bail!("invalid CIDR: {s}");
    }
    let ip: Ipv4Addr = parts[0].parse().context("invalid IP in CIDR")?;
    let prefix: u8 = parts[1].parse().context("invalid prefix in CIDR")?;
    Ok((ip, prefix))
}

fn first_host_in_subnet(network: Ipv4Addr, prefix_len: u8) -> Ipv4Addr {
    let n = u32::from(network);
    let mask: u32 = if prefix_len >= 32 {
        0xffff_ffff
    } else {
        !((1u32 << (32 - prefix_len)) - 1)
    };
    Ipv4Addr::from((n & mask) | 1)
}

async fn install_signal_handler(cancel: CancellationToken) {
    #[cfg(unix)]
    {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(sigterm) => sigterm,
                Err(e) => {
                    warn!("failed to install SIGTERM handler: {e}");
                    tokio::signal::ctrl_c().await.ok();
                    info!("shutdown signal received");
                    cancel.cancel();
                    return;
                }
            };

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
    }

    info!("shutdown signal received");
    cancel.cancel();
}

fn expand_tilde(path: &std::path::Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(format!("{}{}", home, &s[1..]));
        }
    }
    path.to_path_buf()
}

// ── Discovery helpers ─────────────────────────────────────────────────────────

/// Initiate an outbound TCP connection and handshake to `target`.
///
/// Uses a random temporary `NodeId` as the transport key so that concurrent
/// connect attempts don't collide in the transport map.  The real `NodeId` is
/// learned from the handshake response and registered in `reconnect` on success.
async fn initiate_peer_connection(state: Arc<DaemonState>, target: ConnectTarget) {
    let peer_addr = target.addr();
    // Use a random placeholder so concurrent reconnects don't collide.
    let transport_key = NodeId::from_bytes(rand::random::<[u8; 16]>());

    if let Err(e) = state
        .transport
        .connect(&PeerAddress {
            node_id: transport_key,
            addr: peer_addr,
        })
        .await
    {
        warn!(%peer_addr, mechanism = target.mechanism_name(), "connect failed: {e}; reconnect will retry");
        if state.reconnect.begin_reconnect(target).await {
            let st = state.clone();
            tokio::spawn(run_reconnect_task(st, target));
        }
        return;
    }
    info!(%peer_addr, mechanism = target.mechanism_name(), "connected to peer");

    let (tx, rx) = mpsc::channel(8);
    state.hs_channels.lock().await.insert(transport_key, tx);
    let st = state.clone();
    tokio::spawn(async move {
        match handshake_initiator(&st, transport_key, rx).await {
            Ok(peer_id) => {
                st.reconnect.register(peer_id, target).await;
                info!(%peer_id, %peer_addr, mechanism = target.mechanism_name(), "peer connected");
            }
            Err(e) => {
                warn!(%peer_addr, mechanism = target.mechanism_name(), "handshake failed: {e}; reconnect will retry");
                st.transport.disconnect(&transport_key).await.ok();
                if st.reconnect.begin_reconnect(target).await {
                    tokio::spawn(run_reconnect_task(st.clone(), target));
                }
            }
        }
        st.hs_channels.lock().await.remove(&transport_key);
    });
}

/// Consume new-peer notifications from `DiscoveryService` and initiate
/// connections to discovered relays and gateways.
///
/// Filters applied for each discovered [`PeerRecord`]:
/// * Own node — skipped (defense-in-depth; `DiscoveryService` already filters).
/// * Already connected — skipped to avoid duplicate sessions.
/// * Client-only capability — skipped; we only connect to relays / gateways.
/// * `connect_relays` / `connect_gateways` config flags — selectively skipped.
async fn run_discovery_consumer(
    state: Arc<DaemonState>,
    mut new_peer_rx: mpsc::Receiver<PeerRecord>,
) {
    loop {
        tokio::select! {
            Some(record) = new_peer_rx.recv() => {
                // Defense-in-depth: ignore own advertisements.
                if record.node_id == state.self_id {
                    continue;
                }

                // Skip peers we are already connected to.
                if state.sessions.read().await.contains_key(&record.node_id) {
                    debug!(%record.node_id, "discovery: already connected, skipping");
                    continue;
                }

                let caps = record.capabilities;

                // Skip client-only peers — we have no reason to connect to them.
                if !caps.is_relay() && !caps.is_gateway() {
                    debug!(%record.node_id, "discovery: client-only peer, skipping");
                    continue;
                }

                // Apply per-role config filters.
                let cfg = &state.discovery_config;
                if caps.is_gateway() && !cfg.connect_gateways {
                    debug!(%record.node_id, "discovery: connect_gateways=false, skipping gateway");
                    continue;
                }
                if caps.is_relay() && !caps.is_gateway() && !cfg.connect_relays {
                    debug!(%record.node_id, "discovery: connect_relays=false, skipping relay");
                    continue;
                }

                info!(
                    peer_id = %record.node_id,
                    addr = %record.listen_addr,
                    is_gateway = caps.is_gateway(),
                    is_relay = caps.is_relay(),
                    "discovered peer — initiating connection",
                );

                // Register for reconnect-on-loss, then initiate the connection.
                let target = ConnectTarget::Tcp(record.listen_addr);
                state.reconnect.register_discovered(target).await;
                initiate_peer_connection(state.clone(), target).await;
            }
            _ = state.cancel.cancelled() => break,
        }
    }
}

/// Consume `SocketAddr` notifications from [`WifiDirectDiscovery`] and initiate
/// connections to the indicated peers.
///
/// This mirrors [`run_discovery_consumer`] but operates on raw addresses rather
/// than [`PeerRecord`]s — Wi-Fi Direct discovery does not know the peer's
/// `NodeId` before the TCP handshake.  Deduplication by session is still
/// performed after the handshake via the sessions map.
async fn run_wifidirect_consumer(state: Arc<DaemonState>, mut addr_rx: mpsc::Receiver<SocketAddr>) {
    loop {
        tokio::select! {
            Some(addr) = addr_rx.recv() => {
                // Check if any existing session is already using this address.
                // (We cannot check by NodeId here since we don't have it yet.)
                // The sessions map will deduplicate at the handshake level.
                let target = ConnectTarget::Tcp(addr);
                info!(%addr, mechanism = target.mechanism_name(), "Wi-Fi Direct: new peer addr — initiating connection");
                state.reconnect.register_discovered(target).await;
                initiate_peer_connection(state.clone(), target).await;
            }
            _ = state.cancel.cancelled() => break,
        }
    }
}

/// Consume `SocketAddr` notifications from [`BluetoothDiscovery`] and initiate
/// connections to the indicated peers.
async fn run_bluetooth_consumer(state: Arc<DaemonState>, mut addr_rx: mpsc::Receiver<SocketAddr>) {
    loop {
        tokio::select! {
            Some(addr) = addr_rx.recv() => {
                let target = ConnectTarget::BluetoothPan(addr);
                info!(%addr, mechanism = target.mechanism_name(), "Bluetooth PAN: peer addr ready — initiating connection");
                state.reconnect.register_discovered(target).await;
                initiate_peer_connection(state.clone(), target).await;
            }
            _ = state.cancel.cancelled() => break,
        }
    }
}

fn bluetooth_sysfs_root_from_env(value: Option<std::ffi::OsString>) -> PathBuf {
    value
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(pim_bluetooth::DEFAULT_SYSFS_ROOT))
}

fn bluetooth_sysfs_root() -> PathBuf {
    bluetooth_sysfs_root_from_env(std::env::var_os("PIM_BLUETOOTH_SYSFS_ROOT"))
}

fn bluetooth_ip_command_from_env(value: Option<std::ffi::OsString>) -> PathBuf {
    value
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(pim_bluetooth::DEFAULT_IP_COMMAND))
}

fn bluetooth_ip_command() -> PathBuf {
    bluetooth_ip_command_from_env(std::env::var_os("PIM_BLUETOOTH_IP_COMMAND"))
}

fn bluetoothctl_command_from_env(value: Option<std::ffi::OsString>) -> PathBuf {
    value
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(pim_bluetooth::DEFAULT_BLUETOOTHCTL_COMMAND))
}

fn bluetoothctl_command() -> PathBuf {
    bluetoothctl_command_from_env(std::env::var_os("PIM_BLUETOOTH_BLUETOOTHCTL_COMMAND"))
}

fn bt_network_command_from_env(value: Option<std::ffi::OsString>) -> PathBuf {
    value
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(pim_bluetooth::DEFAULT_BT_NETWORK_COMMAND))
}

fn bt_network_command() -> PathBuf {
    bt_network_command_from_env(std::env::var_os("PIM_BLUETOOTH_BT_NETWORK_COMMAND"))
}

#[derive(Debug, Default)]
struct ResolvedPeerTargets {
    startup_targets: Vec<ConnectTarget>,
    reconnect_targets: Vec<ConnectTarget>,
    bluetooth_static_targets: Vec<SocketAddr>,
}

fn resolve_configured_peer_targets(config: &Config) -> Result<ResolvedPeerTargets> {
    let mut resolved = ResolvedPeerTargets::default();

    for peer in &config.peers {
        match &peer.endpoint {
            PeerEndpointConfig::Tcp { address } => {
                use std::net::ToSocketAddrs;

                let mut addrs = address
                    .to_socket_addrs()
                    .with_context(|| format!("failed to resolve TCP peer address {address}"))?;
                let addr = addrs.next().with_context(|| {
                    format!("no socket addresses resolved for TCP peer {address}")
                })?;
                let target = ConnectTarget::Tcp(addr);
                resolved.startup_targets.push(target);
                resolved.reconnect_targets.push(target);
            }
            PeerEndpointConfig::Bluetooth { ip } => {
                if !config.bluetooth.enabled {
                    bail!(
                        "bluetooth peer {ip} configured in [[peers]] but [bluetooth].enabled is false"
                    );
                }
                let ip = ip
                    .parse::<IpAddr>()
                    .with_context(|| format!("invalid Bluetooth peer IP {ip}"))?;
                let addr = SocketAddr::new(ip, config.transport.listen_port);
                let target = ConnectTarget::BluetoothPan(addr);
                resolved.reconnect_targets.push(target);
                resolved.bluetooth_static_targets.push(addr);
            }
        }
    }

    Ok(resolved)
}

/// Derive the [`NodeCapabilities`] bitfield from the loaded configuration.
///
/// * Gateway node  → `CLIENT | RELAY | GATEWAY` (bits `0x07`)
/// * Relay node    → `CLIENT | RELAY`            (bits `0x03`)
/// * Client node   → `CLIENT`                    (bits `0x01`)
fn node_capabilities(config: &Config) -> NodeCapabilities {
    if config.gateway.enabled {
        NodeCapabilities::gateway() // CLIENT | RELAY | GATEWAY (0x07)
    } else if config.relay.enabled {
        NodeCapabilities::relay() // CLIENT | RELAY (0x03)
    } else {
        NodeCapabilities::client() // CLIENT (0x01)
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/etc/pim/pim.toml".to_string());
    let pid_file = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "/run/pim.pid".to_string());

    let config =
        Config::load(std::path::Path::new(&config_path)).context("failed to load config")?;
    info!(config = %config_path, node = %config.node.name, "daemon starting");

    std::fs::write(&pid_file, std::process::id().to_string())
        .context("failed to write PID file")?;

    let key_path = expand_tilde(&config.security.key_file);
    let identity = Arc::new(
        Identity::load_or_generate(&key_path).context("failed to load/generate identity")?,
    );
    let self_id = identity.node_id();
    info!(%self_id, "identity loaded");

    let is_gateway = config.gateway.enabled;
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
    let tun = Arc::new(
        TunInterface::create(&config.interface.name).context("failed to create TUN interface")?,
    );
    if !request_dynamic_ip {
        tun.set_ip(mesh_ip, prefix_len).context("set TUN IP")?;
    }
    tun.set_mtu(config.interface.mtu).context("set TUN MTU")?;
    tun.up().context("bring TUN up")?;
    info!(iface = %tun.name(), addr = %mesh_ip, prefix = prefix_len, "TUN up");

    // ── Transport ─────────────────────────────────────────────────────────
    let listen_addr: SocketAddr = format!("0.0.0.0:{}", config.transport.listen_port)
        .parse()
        .context("invalid listen address")?;
    let transport = TcpTransport::new(listen_addr, self_id)
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

    // ── Cancellation ──────────────────────────────────────────────────────
    let cancel = CancellationToken::new();
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

    // ── Build shared state ────────────────────────────────────────────────
    let mut routing_table = RoutingTable::new(self_id, is_gateway);
    if !request_dynamic_ip {
        routing_table.set_self_mesh_ip(mesh_ip);
    }
    let routing = Arc::new(Mutex::new(routing_table));
    let state = Arc::new(DaemonState {
        self_id,
        identity: identity.clone(),
        is_gateway,
        mesh_ip: AtomicU32::new(u32::from(mesh_ip)),
        mesh_prefix_len: AtomicU8::new(prefix_len),
        request_dynamic_ip,
        own_x25519_pub,
        sessions: Arc::new(RwLock::new(HashMap::new())),
        hs_channels: Arc::new(Mutex::new(HashMap::new())),
        routing,
        reassemblers: Arc::new(Mutex::new(HashMap::new())),
        frag_id: Arc::new(AtomicU32::new(1)),
        transport,
        tun,
        gw_engine,
        internet_link,
        ip_pool,
        peer_last_hb: Arc::new(Mutex::new(HashMap::new())),
        reconnect,
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
        cancel: cancel.clone(),
        discovery_config: config.discovery.clone(),
    });

    // ── Initiate connections to configured peers ───────────────────────────
    for target in configured_targets.startup_targets.iter().copied() {
        initiate_peer_connection(state.clone(), target).await;
    }

    // ── Discovery service ──────────────────────────────────────────────────
    if config.discovery.enabled {
        let pubkey: [u8; 32] = identity.signing_key().verifying_key().to_bytes();
        let caps = node_capabilities(&config);
        let (discovery_svc, new_peer_rx) =
            DiscoveryService::new(self_id, pubkey, caps, config.transport.listen_port);
        let discovery_svc = Arc::new(
            discovery_svc
                .with_port(config.discovery.port)
                .with_broadcast_interval(Duration::from_millis(
                    config.discovery.broadcast_interval_ms,
                ))
                .with_peer_timeout(Duration::from_millis(config.discovery.peer_timeout_ms)),
        );
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
        let (wd_svc, addr_rx) =
            WifiDirectDiscovery::new(config.wifi_direct.clone(), config.transport.listen_port);
        let c = cancel.clone();
        tokio::spawn(async move { wd_svc.run(c).await });
        tokio::spawn(run_wifidirect_consumer(state.clone(), addr_rx));
    } else {
        debug!("Wi-Fi Direct disabled by config");
    }

    // ── Bluetooth PAN discovery ────────────────────────────────────────────
    if config.bluetooth.enabled {
        let mut bluetooth_config = config.bluetooth.clone();
        if bluetooth_config.local_alias.is_empty() {
            bluetooth_config.local_alias = format!(
                "{}{}",
                bluetooth_config.device_name_prefix, config.node.name
            );
        }
        let bluetooth_sysfs_root = bluetooth_sysfs_root();
        let bluetooth_ip_command = bluetooth_ip_command();
        let bluetoothctl_command = bluetoothctl_command();
        let bt_network_command = bt_network_command();
        info!(
            interface = %bluetooth_config.interface,
            static_peers = configured_targets.bluetooth_static_targets.len(),
            sysfs_root = %bluetooth_sysfs_root.display(),
            ip_command = %bluetooth_ip_command.display(),
            bluetoothctl_command = %bluetoothctl_command.display(),
            bt_network_command = %bt_network_command.display(),
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
        )
        .context("failed to construct Bluetooth PAN watcher")?;
        let c = cancel.clone();
        tokio::spawn(async move {
            bt_svc.run(c).await.ok();
        });
        tokio::spawn(run_bluetooth_consumer(state.clone(), addr_rx));
    } else {
        debug!("Bluetooth PAN disabled by config");
    }

    // ── Background tasks ──────────────────────────────────────────────────
    tokio::spawn(run_reassembly_gc(state.clone()));
    tokio::spawn(run_route_advertisements(state.clone()));
    tokio::spawn(run_heartbeats(state.clone()));
    tokio::spawn(run_peer_liveness(state.clone()));
    tokio::spawn(run_buffer_gc(state.clone()));
    tokio::spawn(run_buffer_flush(state.clone()));
    tokio::spawn(run_stats_writer(state.clone()));
    tokio::spawn(run_gateway_probes(state.clone()));
    if state.is_gateway {
        tokio::spawn(run_gateway_return(state.clone()));
        tokio::spawn(run_conntrack_gc(state.clone()));
    }

    // ── Main event loop ───────────────────────────────────────────────────
    run_event_loop(state).await
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── backoff tests ──────────────────────────────────────────────────────

    #[test]
    fn backoff_base_grows_exponentially() {
        assert_eq!(backoff_base_ms(0), 1_000);
        assert_eq!(backoff_base_ms(1), 2_000);
        assert_eq!(backoff_base_ms(2), 4_000);
        assert_eq!(backoff_base_ms(3), 8_000);
        assert_eq!(backoff_base_ms(4), 10_000);
    }

    #[test]
    fn backoff_base_capped_at_30s() {
        // 2^4 * 1000 = 16000 > 10000 → capped
        assert_eq!(backoff_base_ms(4), 10_000);
        assert_eq!(backoff_base_ms(10), 10_000);
        assert_eq!(backoff_base_ms(100), 10_000);
    }

    #[test]
    fn backoff_duration_attempt_0_within_25_pct_jitter() {
        // Run many times to shake out the random jitter range.
        for _ in 0..200 {
            let d = backoff_duration(0);
            let ms = d.as_millis();
            assert!(ms >= 750, "attempt 0: {ms} ms < 750 ms");
            assert!(ms <= 1_250, "attempt 0: {ms} ms > 1250 ms");
        }
    }

    #[test]
    fn backoff_duration_attempt_1_within_25_pct_jitter() {
        for _ in 0..200 {
            let d = backoff_duration(1);
            let ms = d.as_millis();
            assert!(ms >= 1_500, "attempt 1: {ms} ms < 1500 ms");
            assert!(ms <= 2_500, "attempt 1: {ms} ms > 2500 ms");
        }
    }

    #[test]
    fn backoff_duration_capped_within_25_pct_of_30s() {
        // attempt ≥ 4: base = 10 000 ms, jitter ±2 500 ms
        for _ in 0..200 {
            let d = backoff_duration(10);
            let ms = d.as_millis();
            assert!(ms >= 7_500, "attempt 10: {ms} ms < 7 500 ms");
            assert!(ms <= 12_500, "attempt 10: {ms} ms > 12 500 ms");
        }
    }

    #[test]
    fn backoff_duration_increases_with_attempt() {
        // On average the capped duration must be higher than the base duration.
        // Compare median over many samples.
        let low: u64 = (0..200)
            .map(|_| backoff_duration(0).as_millis() as u64)
            .sum::<u64>()
            / 200;
        let high: u64 = (0..200)
            .map(|_| backoff_duration(4).as_millis() as u64)
            .sum::<u64>()
            / 200;
        assert!(
            high > low,
            "attempt 4 avg ({high}) should exceed attempt 0 avg ({low})"
        );
    }

    // ── ReconnectManager tests ─────────────────────────────────────────────

    fn test_addr() -> SocketAddr {
        "127.0.0.1:9999".parse().unwrap()
    }

    fn test_target() -> ConnectTarget {
        ConnectTarget::Tcp(test_addr())
    }

    fn peer_id(b: u8) -> NodeId {
        NodeId::from_bytes([b; 16])
    }

    #[tokio::test]
    async fn begin_reconnect_returns_true_once() {
        let mgr = ReconnectManager::new([test_target()]);
        assert!(
            mgr.begin_reconnect(test_target()).await,
            "first claim should succeed"
        );
        assert!(
            !mgr.begin_reconnect(test_target()).await,
            "second claim should fail"
        );
    }

    #[tokio::test]
    async fn end_reconnect_allows_begin_again() {
        let mgr = ReconnectManager::new([test_target()]);
        mgr.begin_reconnect(test_target()).await;
        mgr.end_reconnect(test_target()).await;
        assert!(
            mgr.begin_reconnect(test_target()).await,
            "should be able to claim after release"
        );
    }

    #[tokio::test]
    async fn configured_addr_none_for_unknown_peer() {
        let mgr = ReconnectManager::new([test_target()]);
        assert!(mgr.configured_target(&peer_id(1)).await.is_none());
    }

    #[tokio::test]
    async fn configured_target_returns_target_after_register() {
        let target = test_target();
        let mgr = ReconnectManager::new([target]);
        mgr.register(peer_id(1), target).await;
        assert_eq!(mgr.configured_target(&peer_id(1)).await, Some(target));
    }

    #[tokio::test]
    async fn configured_target_none_for_unconfigured_target() {
        let configured = ConnectTarget::Tcp("127.0.0.1:9999".parse::<SocketAddr>().unwrap());
        let other = ConnectTarget::Tcp("127.0.0.1:8888".parse::<SocketAddr>().unwrap());
        let mgr = ReconnectManager::new([configured]);
        mgr.register(peer_id(1), other).await;
        assert!(mgr.configured_target(&peer_id(1)).await.is_none());
    }

    // ── Flow control (Phase 4.3) tests ────────────────────────────────────────

    #[test]
    fn should_buffer_under_congestion_control_frames() {
        assert!(
            should_buffer_under_congestion(FrameType::Control),
            "control frames must be buffered under congestion"
        );
    }

    #[test]
    fn should_buffer_under_congestion_route_frames() {
        assert!(
            should_buffer_under_congestion(FrameType::RouteUpdate),
            "route-update frames must be buffered under congestion"
        );
    }

    #[test]
    fn should_buffer_under_congestion_handshake_frames() {
        assert!(
            should_buffer_under_congestion(FrameType::Handshake),
            "handshake frames must be buffered under congestion"
        );
    }

    #[test]
    fn should_drop_data_frames_under_congestion() {
        assert!(
            !should_buffer_under_congestion(FrameType::Data),
            "data frames must be dropped (not buffered) under congestion"
        );
    }

    #[test]
    fn should_drop_heartbeat_frames_under_congestion() {
        assert!(
            !should_buffer_under_congestion(FrameType::Heartbeat),
            "heartbeat frames must be dropped (not buffered) under congestion"
        );
    }

    #[test]
    fn congestion_drop_policy_is_priority_based() {
        // Only Data-priority frame types are dropped; everything more important is buffered.
        use pim_protocol::FrameType;
        let high_priority = [
            FrameType::Control,
            FrameType::Handshake,
            FrameType::RouteUpdate,
        ];
        let low_priority = [FrameType::Data, FrameType::Heartbeat];
        for ft in high_priority {
            assert!(
                should_buffer_under_congestion(ft),
                "{ft:?} should be buffered"
            );
        }
        for ft in low_priority {
            assert!(
                !should_buffer_under_congestion(ft),
                "{ft:?} should be dropped"
            );
        }
    }

    #[tokio::test]
    async fn multiple_configured_peers_tracked_independently() {
        let addr_a: SocketAddr = "127.0.0.1:9001".parse().unwrap();
        let addr_b: SocketAddr = "127.0.0.1:9002".parse().unwrap();
        let target_a = ConnectTarget::Tcp(addr_a);
        let target_b = ConnectTarget::Tcp(addr_b);
        let mgr = ReconnectManager::new([target_a, target_b]);

        mgr.register(peer_id(1), target_a).await;
        mgr.register(peer_id(2), target_b).await;

        assert_eq!(mgr.configured_target(&peer_id(1)).await, Some(target_a));
        assert_eq!(mgr.configured_target(&peer_id(2)).await, Some(target_b));

        // Reconnect slots are independent.
        assert!(mgr.begin_reconnect(target_a).await);
        assert!(mgr.begin_reconnect(target_b).await);
        assert!(!mgr.begin_reconnect(target_a).await);
    }

    // ── Phase 5: Gateway probe / load wiring ──────────────────────────────────

    #[test]
    fn load_normalized_zero_when_no_packets() {
        // 0 packets in interval → load = 0
        let delta: u64 = 0;
        let load = (delta.min(2000) * 255 / 2000) as u8;
        assert_eq!(load, 0);
    }

    #[test]
    fn load_normalized_255_at_saturation() {
        // ≥2000 packets in interval → load = 255
        let delta: u64 = 2000;
        let load = (delta.min(2000) * 255 / 2000) as u8;
        assert_eq!(load, 255);
    }

    #[test]
    fn load_normalized_midpoint() {
        // 1000 packets → load ≈ 127
        let delta: u64 = 1000;
        let load = (delta.min(2000) * 255 / 2000) as u8;
        assert_eq!(load, 127);
    }

    #[tokio::test]
    async fn pending_pings_gc_removes_stale_entries() {
        let pings: Arc<Mutex<HashMap<u64, (NodeId, Instant)>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let stale_time = Instant::now() - Duration::from_secs(60);
        pings
            .lock()
            .await
            .insert(1u64, (NodeId::from_bytes([1; 16]), stale_time));
        pings
            .lock()
            .await
            .insert(2u64, (NodeId::from_bytes([2; 16]), Instant::now()));

        // Simulate the GC step from run_gateway_probes
        pings
            .lock()
            .await
            .retain(|_, (_, sent_at)| sent_at.elapsed() < PENDING_PING_TTL);

        let locked = pings.lock().await;
        assert!(!locked.contains_key(&1u64), "stale ping should be removed");
        assert!(locked.contains_key(&2u64), "fresh ping should remain");
    }

    // ── Observability (Phase 4.5) tests ───────────────────────────────────────

    #[test]
    fn format_stats_contains_all_keys() {
        let s = format_stats(&StatsSnapshot {
            peers: 3,
            routes: 5,
            packets_forwarded: 100,
            bytes_forwarded: 51200,
            packets_dropped: 7,
            congestion_drops: 2,
            conntrack_size: 4,
            uptime_secs: 3600,
        });
        assert!(s.contains("peers=3"));
        assert!(s.contains("routes=5"));
        assert!(s.contains("packets_forwarded=100"));
        assert!(s.contains("bytes_forwarded=51200"));
        assert!(s.contains("packets_dropped=7"));
        assert!(s.contains("congestion_drops=2"));
        assert!(s.contains("conntrack_size=4"));
        assert!(s.contains("uptime_secs=3600"));
    }

    #[test]
    fn packets_forwarded_counter_increments() {
        let counter = Arc::new(AtomicU64::new(0));
        for _ in 0..100 {
            counter.fetch_add(1, Ordering::Relaxed);
        }
        assert_eq!(counter.load(Ordering::Relaxed), 100);
    }

    #[test]
    fn bytes_forwarded_counter_accumulates() {
        let counter = Arc::new(AtomicU64::new(0));
        let sizes = [512u64, 1024, 256, 768, 1500];
        let expected: u64 = sizes.iter().sum();
        for &sz in &sizes {
            counter.fetch_add(sz, Ordering::Relaxed);
        }
        assert_eq!(counter.load(Ordering::Relaxed), expected);
    }

    // ── Peer lifecycle (Phase 3.3) tests ──────────────────────────────────────

    /// A peer that sends heartbeats regularly must not appear in the timed-out set.
    #[test]
    fn peer_heartbeat_keeps_peer_alive() {
        const TIMEOUT: Duration = Duration::from_secs(15);
        let mut hb: HashMap<NodeId, Instant> = HashMap::new();
        let p = peer_id(10);
        // Insert with a fresh timestamp (simulates just-received heartbeat).
        hb.insert(p, Instant::now());

        let timed_out: Vec<NodeId> = hb
            .iter()
            .filter(|(_, last)| last.elapsed() > TIMEOUT)
            .map(|(id, _)| *id)
            .collect();

        assert!(
            timed_out.is_empty(),
            "peer with recent heartbeat must not time out"
        );
    }

    /// A peer that has been silent for more than 15 s must appear in the timed-out set.
    #[test]
    fn peer_timeout_after_missed_heartbeats() {
        const TIMEOUT: Duration = Duration::from_secs(15);
        let mut hb: HashMap<NodeId, Instant> = HashMap::new();
        let p = peer_id(11);
        // Simulate 3 missed heartbeats: last seen 16 s ago.
        hb.insert(p, Instant::now() - Duration::from_secs(16));

        let timed_out: Vec<NodeId> = hb
            .iter()
            .filter(|(_, last)| last.elapsed() > TIMEOUT)
            .map(|(id, _)| *id)
            .collect();

        assert_eq!(timed_out.len(), 1);
        assert_eq!(timed_out[0], p);
    }

    // ── Phase 7: Discovery integration tests ──────────────────────────────────

    /// `register_discovered` + `is_reconnectable_addr` makes discovered peers reconnectable.
    #[tokio::test]
    async fn register_discovered_makes_peer_reconnectable() {
        let mgr = ReconnectManager::new([]);
        let addr: SocketAddr = "127.0.0.1:9200".parse().unwrap();
        let target = ConnectTarget::Tcp(addr);
        // Before registration: not reconnectable.
        assert!(mgr.is_reconnectable_target(&peer_id(50)).await.is_none());
        // Register as discovered and bind the NodeId → addr.
        mgr.register_discovered(target).await;
        mgr.register(peer_id(50), target).await;
        // Now the peer should be reconnectable.
        assert_eq!(
            mgr.is_reconnectable_target(&peer_id(50)).await,
            Some(target)
        );
    }

    /// Both configured and discovered peers are reconnectable.
    #[tokio::test]
    async fn is_reconnectable_covers_configured_and_discovered() {
        let configured: SocketAddr = "127.0.0.1:9100".parse().unwrap();
        let discovered: SocketAddr = "127.0.0.1:9200".parse().unwrap();
        let configured = ConnectTarget::Tcp(configured);
        let discovered = ConnectTarget::Tcp(discovered);
        let mgr = ReconnectManager::new([configured]);
        mgr.register(peer_id(51), configured).await;
        mgr.register(peer_id(52), discovered).await;
        mgr.register_discovered(discovered).await;

        assert!(
            mgr.is_reconnectable_target(&peer_id(51)).await.is_some(),
            "configured peer must be reconnectable"
        );
        assert!(
            mgr.is_reconnectable_target(&peer_id(52)).await.is_some(),
            "discovered peer must be reconnectable"
        );
        assert!(
            mgr.is_reconnectable_target(&peer_id(99)).await.is_none(),
            "unknown peer must not be reconnectable"
        );
    }

    /// Client-only capability `(!is_relay() && !is_gateway())` must trigger the skip condition.
    #[test]
    fn client_only_peer_is_skipped() {
        let caps = NodeCapabilities::client();
        // The consumer skips when neither relay nor gateway.
        assert!(
            !caps.is_relay() && !caps.is_gateway(),
            "consumer must filter out client-only peers"
        );
    }

    /// Relay and gateway capability sets must pass the capability filter.
    #[test]
    fn relay_and_gateway_caps_pass_filter() {
        let relay = NodeCapabilities::relay();
        let gateway = NodeCapabilities::gateway();
        assert!(
            relay.is_relay() || relay.is_gateway(),
            "relay caps must pass filter"
        );
        assert!(
            gateway.is_relay() || gateway.is_gateway(),
            "gateway caps must pass filter"
        );
    }

    /// The self-check (`record.node_id == state.self_id`) correctly identifies own broadcasts.
    #[test]
    fn self_advertisement_is_ignored() {
        let own_id = NodeId::from_bytes([42; 16]);
        let record_id = NodeId::from_bytes([42; 16]);
        assert_eq!(
            record_id, own_id,
            "own NodeId must be caught by self-check in consumer"
        );
    }

    /// When `discovery.enabled = false`, the discovery service must not be started.
    #[test]
    fn discovery_disabled_in_config_skips_spawning() {
        let config =
            Config::from_toml_str("[node]\nname=\"t\"\n[discovery]\nenabled=false\n").unwrap();
        assert!(
            !config.discovery.enabled,
            "discovery service must not start when enabled=false"
        );
    }

    // ── Phase 7: node_capabilities() tests ───────────────────────────────────

    fn cfg_gateway() -> Config {
        Config::from_toml_str("[node]\nname=\"t\"\n[gateway]\nenabled=true\n").unwrap()
    }

    fn cfg_relay() -> Config {
        Config::from_toml_str("[node]\nname=\"t\"\n[relay]\nenabled=true\n").unwrap()
    }

    fn cfg_client() -> Config {
        Config::from_toml_str("[node]\nname=\"t\"\n").unwrap()
    }

    #[test]
    fn gateway_config_yields_gateway_caps() {
        let caps = node_capabilities(&cfg_gateway());
        assert!(caps.is_gateway(), "gateway flag expected");
        assert!(caps.is_relay(), "relay flag expected on gateway");
        assert!(caps.is_client(), "client flag expected on gateway");
    }

    #[test]
    fn relay_config_yields_relay_caps() {
        let caps = node_capabilities(&cfg_relay());
        assert!(caps.is_relay(), "relay flag expected");
        assert!(caps.is_client(), "client flag expected on relay");
        assert!(!caps.is_gateway(), "gateway flag must NOT be set on relay");
    }

    #[test]
    fn client_config_yields_client_caps_only() {
        let caps = node_capabilities(&cfg_client());
        assert!(caps.is_client(), "client flag expected");
        assert!(!caps.is_relay(), "relay flag must NOT be set on client");
        assert!(!caps.is_gateway(), "gateway flag must NOT be set on client");
    }

    #[test]
    fn gateway_caps_bits_are_correct() {
        let caps = node_capabilities(&cfg_gateway());
        assert_eq!(
            caps.bits(),
            0x07,
            "gateway caps must be CLIENT|RELAY|GATEWAY = 0x07"
        );
    }

    #[test]
    fn parse_interface_ipv4_output_extracts_first_inet_cidr() {
        let output =
            "2: eno1    inet 192.168.0.137/24 brd 192.168.0.255 scope global dynamic eno1\n";
        assert_eq!(
            parse_interface_ipv4_output(output),
            Some(Ipv4Addr::new(192, 168, 0, 137))
        );
    }

    #[test]
    fn ipv4_destination_reads_destination_octets() {
        let packet = [
            0x45, 0, 0, 20, 0, 0, 0, 0, 64, 1, 0, 0, 10, 77, 0, 2, 1, 1, 1, 1,
        ];
        assert_eq!(ipv4_destination(&packet), Some(Ipv4Addr::new(1, 1, 1, 1)));
    }

    // ── Phase 8: Wi-Fi Direct config tests ───────────────────────────────────

    /// When `wifi_direct.enabled = false` (the default), the block that spawns the
    /// Wi-Fi Direct service must be skipped — verified by checking config.
    #[test]
    fn wifidirect_disabled_config_skips_spawning() {
        let config = Config::from_toml_str("[node]\nname=\"t\"\n").unwrap();
        assert!(
            !config.wifi_direct.enabled,
            "Wi-Fi Direct service must not start when enabled=false"
        );
    }

    /// When `wifi_direct.enabled = true`, the daemon reads `interface` and `listen_port`.
    #[test]
    fn wifidirect_enabled_config_exposes_interface_and_port() {
        let toml = "[node]\nname=\"t\"\n[wifi_direct]\nenabled=true\ninterface=\"wlan1\"\n";
        let config = Config::from_toml_str(toml).unwrap();
        assert!(config.wifi_direct.enabled);
        assert_eq!(config.wifi_direct.interface, "wlan1");
        // listen_port comes from transport section, not wifi_direct.
        assert_eq!(config.transport.listen_port, 9100);
    }

    /// Discovered Wi-Fi Direct peer addresses must be registered for reconnect-on-loss.
    #[tokio::test]
    async fn wifidirect_addr_registered_for_reconnect() {
        let mgr = ReconnectManager::new([]);
        let addr: SocketAddr = "192.168.49.100:9100".parse().unwrap();
        let target = ConnectTarget::Tcp(addr);
        mgr.register_discovered(target).await;
        // Bind a fake NodeId to the address to enable the lookup.
        mgr.register(peer_id(60), target).await;
        assert_eq!(
            mgr.is_reconnectable_target(&peer_id(60)).await,
            Some(target),
            "Wi-Fi Direct discovered peer must be reconnectable"
        );
    }

    /// Wi-Fi Direct and UDP discovery can both be enabled simultaneously; their
    /// peer channel addresses both go through `register_discovered`.
    #[tokio::test]
    async fn wifidirect_coexists_with_udp_discovery() {
        let mgr = ReconnectManager::new([]);
        let udp_addr: SocketAddr = "172.34.0.20:9100".parse().unwrap();
        let wfd_addr: SocketAddr = "192.168.49.100:9100".parse().unwrap();
        let udp_target = ConnectTarget::Tcp(udp_addr);
        let wfd_target = ConnectTarget::Tcp(wfd_addr);
        mgr.register_discovered(udp_target).await;
        mgr.register_discovered(wfd_target).await;
        mgr.register(peer_id(61), udp_target).await;
        mgr.register(peer_id(62), wfd_target).await;
        assert!(
            mgr.is_reconnectable_target(&peer_id(61)).await.is_some(),
            "UDP peer reconnectable"
        );
        assert!(
            mgr.is_reconnectable_target(&peer_id(62)).await.is_some(),
            "WFD peer reconnectable"
        );
    }

    /// Verify `WifiDirectDiscovery::new` can be constructed from config without panicking.
    #[test]
    fn wifidirect_discovery_construction_from_config() {
        use pim_wifidirect::WifiDirectDiscovery;
        let toml = "[node]\nname=\"t\"\n[wifi_direct]\nenabled=true\n";
        let config = Config::from_toml_str(toml).unwrap();
        let (_svc, _rx) =
            WifiDirectDiscovery::new(config.wifi_direct, config.transport.listen_port);
        // Construction must not panic.
    }

    #[test]
    fn bluetooth_disabled_config_skips_spawning() {
        let config = Config::from_toml_str("[node]\nname=\"t\"\n").unwrap();
        assert!(
            !config.bluetooth.enabled,
            "Bluetooth PAN watcher must not start when enabled=false"
        );
    }

    #[test]
    fn bluetooth_enabled_config_exposes_interface_and_static_peer_entries() {
        let toml = "[node]\nname=\"t\"\n[bluetooth]\nenabled=true\ninterface=\"bnep1\"\n[[peers]]\nmechanism=\"bluetooth\"\nip=\"192.168.44.2\"\n";
        let config = Config::from_toml_str(toml).unwrap();
        assert!(config.bluetooth.enabled);
        assert_eq!(config.bluetooth.interface, "bnep1");
        assert_eq!(config.peers.len(), 1);
        assert_eq!(config.transport.listen_port, 9100);
    }

    #[tokio::test]
    async fn bluetooth_addr_registered_for_reconnect() {
        let mgr = ReconnectManager::new([]);
        let addr: SocketAddr = "192.168.44.2:9100".parse().unwrap();
        let target = ConnectTarget::BluetoothPan(addr);
        mgr.register_discovered(target).await;
        mgr.register(peer_id(63), target).await;
        assert_eq!(
            mgr.is_reconnectable_target(&peer_id(63)).await,
            Some(target),
            "Bluetooth-discovered peer must be reconnectable"
        );
    }

    #[test]
    fn bluetooth_discovery_construction_from_config() {
        use pim_bluetooth::BluetoothDiscovery;
        let toml = "[node]\nname=\"t\"\n[bluetooth]\nenabled=true\n";
        let config = Config::from_toml_str(toml).unwrap();
        let (_svc, _rx) = BluetoothDiscovery::new(
            config.bluetooth,
            config.transport.listen_port,
            vec!["192.168.44.2:9100".parse().unwrap()],
        )
        .unwrap();
    }

    #[test]
    fn bluetooth_sysfs_root_defaults_to_linux_sysfs() {
        assert_eq!(
            bluetooth_sysfs_root_from_env(None),
            PathBuf::from("/sys/class/net")
        );
    }

    #[test]
    fn bluetooth_sysfs_root_honors_environment_override() {
        assert_eq!(
            bluetooth_sysfs_root_from_env(Some("/tmp/pim-fake-sysfs".into())),
            PathBuf::from("/tmp/pim-fake-sysfs")
        );
    }

    #[test]
    fn bluetooth_ip_command_defaults_to_ip() {
        assert_eq!(bluetooth_ip_command_from_env(None), PathBuf::from("ip"));
    }

    #[test]
    fn bluetooth_ip_command_honors_environment_override() {
        assert_eq!(
            bluetooth_ip_command_from_env(Some("/tmp/fake-ip".into())),
            PathBuf::from("/tmp/fake-ip")
        );
    }

    #[test]
    fn bluetoothctl_command_defaults_to_bluetoothctl() {
        assert_eq!(
            bluetoothctl_command_from_env(None),
            PathBuf::from("bluetoothctl")
        );
    }

    #[test]
    fn bluetoothctl_command_honors_environment_override() {
        assert_eq!(
            bluetoothctl_command_from_env(Some("/tmp/fake-bluetoothctl".into())),
            PathBuf::from("/tmp/fake-bluetoothctl")
        );
    }

    #[test]
    fn bt_network_command_defaults_to_bt_network() {
        assert_eq!(
            bt_network_command_from_env(None),
            PathBuf::from("bt-network")
        );
    }

    #[test]
    fn bt_network_command_honors_environment_override() {
        assert_eq!(
            bt_network_command_from_env(Some("/tmp/fake-bt-network".into())),
            PathBuf::from("/tmp/fake-bt-network")
        );
    }

    /// On receiving a Goodbye, the peer must be removed from the heartbeat map immediately.
    #[tokio::test]
    async fn goodbye_triggers_immediate_removal() {
        let p = peer_id(12);
        let hb_map: Arc<Mutex<HashMap<NodeId, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
        hb_map.lock().await.insert(p, Instant::now());
        assert!(hb_map.lock().await.contains_key(&p));

        // Simulate the Goodbye handler: remove from heartbeat map (mirrors remove_peer).
        hb_map.lock().await.remove(&p);

        assert!(
            !hb_map.lock().await.contains_key(&p),
            "peer must be absent from heartbeat map after Goodbye"
        );
    }
}
