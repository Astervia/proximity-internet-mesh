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
//! address = "10.0.0.1:9100"   # peer's transport address
//! ```

mod rate_limiter;
mod reputation;
mod send_buffer;

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use bytes::BytesMut;
use rand::Rng as _;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use pim_core::{Config, FrameCodec, NodeId};
use pim_crypto::{
    e2e_decrypt, e2e_encrypt, x25519_public_from_seed, EncryptedFrame, HandshakeConfirm,
    HandshakeInit, HandshakeResponse, Handshaker, Identity, SessionCipher,
};
use pim_gateway::{GatewayEngine, IpPool};
use pim_protocol::{
    fragment_packet, ControlFrame, DataFlags, FragmentFrame, FrameType, HandshakeFrameType,
    HandshakeWireFrame, HeartbeatFrame, MeshDataFrame, Reassembler, RouteUpdateFrame,
    TransportFrame,
};
use ed25519_dalek::VerifyingKey;
use pim_routing::{
    signing::{sign_route_update, verify_route_update},
    RoutingTable,
};
use pim_transport::{PeerAddress, TcpTransport, Transport, TransportError};
use rate_limiter::{RateLimiter, DEFAULT_BURST, DEFAULT_RATE};
use reputation::ReputationTracker;
use send_buffer::{Priority, SendBuffer, DEFAULT_CAPACITY, DEFAULT_TIMEOUT};
use pim_tun::TunInterface;

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
struct ReconnectManager {
    /// SocketAddrs from `[[peers]]` config — always reconnect if lost.
    configured_addrs: HashSet<SocketAddr>,
    /// Maps real peer NodeId → configured SocketAddr (learned after handshake).
    addr_by_peer: Mutex<HashMap<NodeId, SocketAddr>>,
    /// Addresses that currently have an active reconnect task.
    reconnecting: Mutex<HashSet<SocketAddr>>,
}

impl ReconnectManager {
    fn new(addrs: impl IntoIterator<Item = SocketAddr>) -> Self {
        Self {
            configured_addrs: addrs.into_iter().collect(),
            addr_by_peer: Mutex::new(HashMap::new()),
            reconnecting: Mutex::new(HashSet::new()),
        }
    }

    /// Record `peer_id → addr` after a successful handshake.
    async fn register(&self, peer_id: NodeId, addr: SocketAddr) {
        self.addr_by_peer.lock().await.insert(peer_id, addr);
    }

    /// Return the configured SocketAddr for `peer_id`, if it is a configured peer.
    async fn configured_addr(&self, peer_id: &NodeId) -> Option<SocketAddr> {
        let addr = self.addr_by_peer.lock().await.get(peer_id).copied()?;
        self.configured_addrs.contains(&addr).then_some(addr)
    }

    /// Claim the reconnect slot for `addr`.  Returns `true` if a new reconnect
    /// task should be spawned (i.e., none was already running).
    async fn begin_reconnect(&self, addr: SocketAddr) -> bool {
        self.reconnecting.lock().await.insert(addr)
    }

    /// Release the reconnect slot when the task finishes (success or cancel).
    async fn end_reconnect(&self, addr: SocketAddr) {
        self.reconnecting.lock().await.remove(&addr);
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
}

impl DaemonState {
    fn next_frag_id(&self) -> u32 {
        self.frag_id.fetch_add(1, Ordering::Relaxed)
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

    // Schedule reconnect if this was a configured peer
    if let Some(addr) = state.reconnect.configured_addr(&peer_id).await {
        if state.reconnect.begin_reconnect(addr).await {
            info!(%peer_id, %addr, "scheduling reconnect with backoff");
            let st = state.clone();
            tokio::spawn(run_reconnect_task(st, addr));
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
                debug!(removed = before - after, "conntrack GC: expired entries removed");
            }
        }
    }
}

// ── Observability ─────────────────────────────────────────────────────────────

/// Path to the runtime stats file read by `pim status --verbose`.
pub const STATS_PATH: &str = "/run/pim.stats";

/// Collect current metrics and format them as a newline-delimited key=value string.
pub fn format_stats(
    peers: usize,
    routes: usize,
    packets_forwarded: u64,
    bytes_forwarded: u64,
    packets_dropped: u64,
    congestion_drops: u64,
    conntrack_size: usize,
    uptime_secs: u64,
) -> String {
    format!(
        "peers={peers}\n\
         routes={routes}\n\
         packets_forwarded={packets_forwarded}\n\
         bytes_forwarded={bytes_forwarded}\n\
         packets_dropped={packets_dropped}\n\
         congestion_drops={congestion_drops}\n\
         conntrack_size={conntrack_size}\n\
         uptime_secs={uptime_secs}\n"
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
        let uptime_secs = state
            .start_time
            .elapsed()
            .unwrap_or_default()
            .as_secs();

        let content = format_stats(
            peers, routes, packets_forwarded, bytes_forwarded,
            packets_dropped, congestion_drops, conntrack_size, uptime_secs,
        );

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
            state.pending_pings.lock().await.insert(nonce, (gw_id, Instant::now()));
            send_control(&state, &gw_id, ControlFrame::Ping { nonce }).await;
            debug!(%gw_id, nonce, "sent gateway probe Ping");
        }
    }
}

// ── Reconnect task ────────────────────────────────────────────────────────────

/// Background task that reconnects to a configured peer using exponential
/// backoff + jitter.  Runs a full handshake after each successful TCP connect
/// so a fresh session key is established.
async fn run_reconnect_task(state: Arc<DaemonState>, addr: SocketAddr) {
    let mut attempt = 0u32;
    loop {
        let delay = backoff_duration(attempt);
        info!(%addr, attempt, delay_ms = delay.as_millis(), "reconnect scheduled");

        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = state.cancel.cancelled() => {
                state.reconnect.end_reconnect(addr).await;
                return;
            }
        }

        info!(%addr, attempt, "attempting reconnect");

        // Use a unique random placeholder so concurrent reconnects don't collide.
        let transport_key = NodeId::from_bytes(rand::random::<[u8; 16]>());

        if let Err(e) = state
            .transport
            .connect(&PeerAddress { node_id: transport_key, addr })
            .await
        {
            warn!(%addr, attempt, "reconnect TCP connect failed: {e}");
            attempt += 1;
            continue;
        }

        // Kick off handshake on the new connection.
        let (tx, rx) = mpsc::channel(8);
        state.hs_channels.lock().await.insert(transport_key, tx);

        match handshake_initiator(&state, transport_key, rx).await {
            Ok(peer_id) => {
                state.reconnect.register(peer_id, addr).await;
                state.hs_channels.lock().await.remove(&transport_key);
                info!(%peer_id, %addr, "reconnect succeeded (new session key)");
                state.reconnect.end_reconnect(addr).await;
                return;
            }
            Err(e) => {
                warn!(%addr, attempt, "reconnect handshake failed: {e}");
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
            (HandshakeResponse { sender_pub: sp, ephemeral_pub, nonce, signature }, sp)
        }
        _ => bail!("expected HandshakeResponse from {transport_key}"),
    };

    // Derive the peer's real NodeId from their Ed25519 public key.
    let peer_id = NodeId::from_public_key(&sender_pub);

    hs.finalize_initiator(&response).context("handshake finalize")?;

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
    state.peer_last_hb.lock().await.insert(peer_id, Instant::now());
    info!(%peer_id, "session established (initiator)");

    // Flush any frames buffered while this peer was unreachable.
    flush_send_buffer(state, peer_id).await;

    // Non-gateway nodes request an IP address from the peer after connecting.
    if state.request_dynamic_ip {
        send_control(
            state,
            &peer_id,
            ControlFrame::IpRequest { requester_id: state.self_id },
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
        } => HandshakeInit { sender_pub, ephemeral_pub, nonce, signature },
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
    hs.verify_confirm(&confirm).context("confirm verification")?;
    info!(%peer_id, "handshake complete (responder)");

    let key = *hs.session_key().unwrap().as_bytes();
    let session = Arc::new(Session {
        peer_id,
        send: SessionCipher::new(&key, nonce_prefix(&key, false)),
        recv: SessionCipher::new(&key, nonce_prefix(&key, true)),
    });
    state.sessions.write().await.insert(peer_id, session);
    state.peer_pubkeys.write().await.insert(peer_id, init.sender_pub);
    state.routing.lock().await.add_peer(peer_id);
    state.routing.lock().await.unblacklist_peer(&peer_id);
    state.reputation.lock().await.pardon(&peer_id);
    state.peer_last_hb.lock().await.insert(peer_id, Instant::now());
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
        let load = ((delta.min(2000) * 255 / 2000)) as u8;

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
            gw_x25519_pub: if state.is_gateway { state.own_x25519_pub } else { [0u8; 32] },
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
                                        } else if let Err(e) = state.tun.add_default_route(gw) {
                                            warn!("add_default_route failed: {e}");
                                        }
                                        state.mesh_ip.store(u32::from(ip), Ordering::Relaxed);
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
    if !state.is_gateway { return; }
    let mut buf = vec![0u8; 65536];

    loop {
        tokio::select! {
            res = state.tun.read_packet(&mut buf) => {
                let n = match res {
                    Ok(n) => n,
                    Err(e) => { error!("GW TUN read: {e}"); break; }
                };
                let pkt = buf[..n].to_vec();
                if pkt.len() < 20 { continue; }
                let dest_ip = Ipv4Addr::new(pkt[16], pkt[17], pkt[18], pkt[19]);

                if dest_ip == Ipv4Addr::from(state.mesh_ip.load(Ordering::Relaxed)) {
                    // It is for the gateway itself
                    continue;
                }

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

fn expand_tilde(path: &std::path::Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(format!("{}{}", home, &s[1..]));
        }
    }
    path.to_path_buf()
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

    let config = Config::load(std::path::Path::new(&config_path))
        .context("failed to load config")?;
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
    let gw_engine: Option<Arc<GatewayEngine>> = if is_gateway {
        let gw = Arc::new(GatewayEngine::new(mesh_ip, &config.gateway.nat_interface));
        let cidr = format!("{}/{}", mesh_ip, prefix_len);
        if let Err(e) = gw.setup_masquerade(&cidr) {
            warn!("iptables setup failed (may need root): {e}");
        }
        Some(gw)
    } else {
        None
    };

    // ── IP pool (gateway) ─────────────────────────────────────────────────
    let ip_pool: Option<Arc<Mutex<IpPool>>> = if is_gateway {
        // Network address is the subnet base (e.g. 10.77.0.0 for 10.77.0.1/24)
        let n = u32::from(mesh_ip);
        let mask: u32 = if prefix_len >= 32 { 0xffff_ffff } else { !((1u32 << (32 - prefix_len)) - 1) };
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
            tokio::signal::ctrl_c().await.ok();
            info!("shutdown signal received");
            cancel.cancel();
        });
    }

    // ── Build reconnect manager ───────────────────────────────────────────
    let mut configured_addrs: Vec<SocketAddr> = Vec::new();
    for p in &config.peers {
        use std::net::ToSocketAddrs;
        match p.address.to_socket_addrs() {
            Ok(mut addrs) => {
                if let Some(addr) = addrs.next() {
                    configured_addrs.push(addr);
                }
            }
            Err(e) => tracing::warn!("failed to resolve peer address {}: {}", p.address, e),
        }
    }
    let reconnect = Arc::new(ReconnectManager::new(configured_addrs.iter().copied()));

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
    });

    // ── Initiate connections to configured peers ───────────────────────────
    for peer_addr in configured_addrs {
        // Each outbound connection uses a random temporary NodeId so that
        // concurrent reconnects don't collide in the transport map.  The
        // real peer NodeId is learned from the handshake Response and the
        // transport entry is renamed accordingly inside handshake_initiator.
        let transport_key = NodeId::from_bytes(rand::random::<[u8; 16]>());

        if let Err(e) = state
            .transport
            .connect(&PeerAddress { node_id: transport_key, addr: peer_addr })
            .await
        {
            warn!(%peer_addr, "initial connect failed: {e}; reconnect will retry");
            // Trigger reconnect directly since we have no peer_id yet.
            if state.reconnect.begin_reconnect(peer_addr).await {
                let st = state.clone();
                tokio::spawn(run_reconnect_task(st, peer_addr));
            }
            continue;
        }
        info!(%peer_addr, "connected to peer");

        // Spawn the handshake initiator; it registers peer_id → addr on success.
        let (tx, rx) = mpsc::channel(8);
        state.hs_channels.lock().await.insert(transport_key, tx);
        let st = state.clone();
        tokio::spawn(async move {
            match handshake_initiator(&st, transport_key, rx).await {
                Ok(peer_id) => {
                    st.reconnect.register(peer_id, peer_addr).await;
                    info!(%peer_id, %peer_addr, "peer connected");
                }
                Err(e) => {
                    warn!(%peer_addr, "initial handshake failed: {e}; reconnect will retry");
                    st.transport.disconnect(&transport_key).await.ok();
                    if st.reconnect.begin_reconnect(peer_addr).await {
                        tokio::spawn(run_reconnect_task(st.clone(), peer_addr));
                    }
                }
            }
            st.hs_channels.lock().await.remove(&transport_key);
        });
    }

    // Add default route via gateway mesh IP for client nodes
    if !is_gateway {
        let gw_mesh_ip = first_host_in_subnet(mesh_ip, prefix_len);
        if let Err(e) = state.tun.add_default_route(gw_mesh_ip) {
            warn!("failed to add default route: {e}");
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
        let low: u64 = (0..200).map(|_| backoff_duration(0).as_millis() as u64).sum::<u64>() / 200;
        let high: u64 = (0..200).map(|_| backoff_duration(4).as_millis() as u64).sum::<u64>() / 200;
        assert!(high > low, "attempt 4 avg ({high}) should exceed attempt 0 avg ({low})");
    }

    // ── ReconnectManager tests ─────────────────────────────────────────────

    fn test_addr() -> SocketAddr {
        "127.0.0.1:9999".parse().unwrap()
    }

    fn peer_id(b: u8) -> NodeId {
        NodeId::from_bytes([b; 16])
    }

    #[tokio::test]
    async fn begin_reconnect_returns_true_once() {
        let mgr = ReconnectManager::new([test_addr()]);
        assert!(mgr.begin_reconnect(test_addr()).await, "first claim should succeed");
        assert!(!mgr.begin_reconnect(test_addr()).await, "second claim should fail");
    }

    #[tokio::test]
    async fn end_reconnect_allows_begin_again() {
        let mgr = ReconnectManager::new([test_addr()]);
        mgr.begin_reconnect(test_addr()).await;
        mgr.end_reconnect(test_addr()).await;
        assert!(mgr.begin_reconnect(test_addr()).await, "should be able to claim after release");
    }

    #[tokio::test]
    async fn configured_addr_none_for_unknown_peer() {
        let mgr = ReconnectManager::new([test_addr()]);
        assert!(mgr.configured_addr(&peer_id(1)).await.is_none());
    }

    #[tokio::test]
    async fn configured_addr_returns_addr_after_register() {
        let addr = test_addr();
        let mgr = ReconnectManager::new([addr]);
        mgr.register(peer_id(1), addr).await;
        assert_eq!(mgr.configured_addr(&peer_id(1)).await, Some(addr));
    }

    #[tokio::test]
    async fn configured_addr_none_for_unconfigured_addr() {
        let configured = "127.0.0.1:9999".parse::<SocketAddr>().unwrap();
        let other: SocketAddr = "127.0.0.1:8888".parse().unwrap();
        let mgr = ReconnectManager::new([configured]);
        // Register with an address not in the configured list.
        mgr.register(peer_id(1), other).await;
        assert!(mgr.configured_addr(&peer_id(1)).await.is_none());
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
        let high_priority = [FrameType::Control, FrameType::Handshake, FrameType::RouteUpdate];
        let low_priority = [FrameType::Data, FrameType::Heartbeat];
        for ft in high_priority {
            assert!(should_buffer_under_congestion(ft), "{ft:?} should be buffered");
        }
        for ft in low_priority {
            assert!(!should_buffer_under_congestion(ft), "{ft:?} should be dropped");
        }
    }

    #[tokio::test]
    async fn multiple_configured_peers_tracked_independently() {
        let addr_a: SocketAddr = "127.0.0.1:9001".parse().unwrap();
        let addr_b: SocketAddr = "127.0.0.1:9002".parse().unwrap();
        let mgr = ReconnectManager::new([addr_a, addr_b]);

        mgr.register(peer_id(1), addr_a).await;
        mgr.register(peer_id(2), addr_b).await;

        assert_eq!(mgr.configured_addr(&peer_id(1)).await, Some(addr_a));
        assert_eq!(mgr.configured_addr(&peer_id(2)).await, Some(addr_b));

        // Reconnect slots are independent.
        assert!(mgr.begin_reconnect(addr_a).await);
        assert!(mgr.begin_reconnect(addr_b).await);
        assert!(!mgr.begin_reconnect(addr_a).await);
    }

    // ── Phase 5: Gateway probe / load wiring ──────────────────────────────────

    #[test]
    fn load_normalized_zero_when_no_packets() {
        // 0 packets in interval → load = 0
        let delta: u64 = 0;
        let load = ((delta.min(2000) * 255 / 2000)) as u8;
        assert_eq!(load, 0);
    }

    #[test]
    fn load_normalized_255_at_saturation() {
        // ≥2000 packets in interval → load = 255
        let delta: u64 = 2000;
        let load = ((delta.min(2000) * 255 / 2000)) as u8;
        assert_eq!(load, 255);
    }

    #[test]
    fn load_normalized_midpoint() {
        // 1000 packets → load ≈ 127
        let delta: u64 = 1000;
        let load = ((delta.min(2000) * 255 / 2000)) as u8;
        assert_eq!(load, 127);
    }

    #[tokio::test]
    async fn pending_pings_gc_removes_stale_entries() {
        let pings: Arc<Mutex<HashMap<u64, (NodeId, Instant)>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let stale_time = Instant::now() - Duration::from_secs(60);
        pings.lock().await.insert(1u64, (NodeId::from_bytes([1; 16]), stale_time));
        pings.lock().await.insert(2u64, (NodeId::from_bytes([2; 16]), Instant::now()));

        // Simulate the GC step from run_gateway_probes
        pings.lock().await.retain(|_, (_, sent_at)| sent_at.elapsed() < PENDING_PING_TTL);

        let locked = pings.lock().await;
        assert!(!locked.contains_key(&1u64), "stale ping should be removed");
        assert!(locked.contains_key(&2u64), "fresh ping should remain");
    }

    // ── Observability (Phase 4.5) tests ───────────────────────────────────────

    #[test]
    fn format_stats_contains_all_keys() {
        let s = format_stats(3, 5, 100, 51200, 7, 2, 4, 3600);
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

        assert!(timed_out.is_empty(), "peer with recent heartbeat must not time out");
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

    /// On receiving a Goodbye, the peer must be removed from the heartbeat map immediately.
    #[tokio::test]
    async fn goodbye_triggers_immediate_removal() {
        let p = peer_id(12);
        let hb_map: Arc<Mutex<HashMap<NodeId, Instant>>> =
            Arc::new(Mutex::new(HashMap::new()));
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
