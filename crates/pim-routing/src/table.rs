//! Routing table implementation.
//!
//! # Algorithm
//!
//! Each node maintains a routing table that maps destination `NodeId` to a
//! `RouteEntry` containing the next-hop peer, the hop count, and metadata.
//!
//! Route advertisements are exchanged between directly connected peers.  On
//! receiving an update, the node applies Bellman-Ford: if the advertised path
//! is shorter than the current best, the entry is updated and a triggered
//! update is flagged.
//!
//! ## Split Horizon with Poison Reverse
//!
//! When generating an advertisement for peer P, any route that was *learned
//! from* P is advertised back to P with `hops = INFINITY` (16), preventing
//! two-node loops.  Routes learned from other peers (or self) are advertised
//! normally.
//!
//! ## Infinity / Unreachable
//!
//! Following RIP convention, `INFINITY = 16` means unreachable.  A route
//! with `hops >= INFINITY` is never installed.

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tracing::{debug, info, trace, warn};

use pim_core::{derive_mesh_ipv4, verify_mesh_ipv4, Ipv4Prefix, NodeId};
use pim_protocol::{RouteEntry, RouteUpdateFrame};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Hop count considered "unreachable" (RIP convention).
pub const INFINITY: u8 = 16;

// ── Data structures ───────────────────────────────────────────────────────────

/// A single entry in the routing table.
#[derive(Debug, Clone)]
pub struct RouteTableEntry {
    /// Which directly-connected peer to forward packets through.
    pub next_hop: NodeId,
    /// Hop count to the destination.
    pub hops: u8,
    /// True if the destination is a gateway node.
    pub is_gateway: bool,
    /// When this entry was last refreshed.
    pub last_seen: Instant,
    /// Which peer this route was learned from (for split horizon).
    pub learned_from: NodeId,
    /// Sequence number of the last update that installed this entry.
    pub sequence: u64,
    /// Forwarding load reported by the gateway (0 = idle, 255 = saturated).
    /// Only meaningful when `is_gateway` is true; zero for non-gateway entries.
    pub gateway_load: u8,
    /// Round-trip time to this gateway measured via Ping/Pong (milliseconds).
    /// `None` until the first probe completes.
    pub rtt_ms: Option<u32>,
    /// Mesh IPv4 address advertised for this destination.
    pub mesh_ip: Option<Ipv4Addr>,
}

// ── Gateway selection ─────────────────────────────────────────────────────────

/// Composite score used to rank gateway routes.
///
/// Lower is better.  The formula balances hop count, load, and measured latency:
///
/// - Each hop contributes 100 points.
/// - Full load (255) adds 127 points — roughly equivalent to 1 extra hop.
/// - 1 000 ms RTT adds 100 points — equivalent to 1 extra hop.
///
/// So a lightly-loaded, low-latency nearby gateway always wins over a distant one,
/// while a heavily-loaded 1-hop gateway may lose to a clean 2-hop gateway.
pub fn gateway_score(hops: u8, load: u8, rtt_ms: Option<u32>) -> u32 {
    hops as u32 * 100 + load as u32 / 2 + rtt_ms.unwrap_or(0) / 10
}

impl RouteTableEntry {
    fn is_expired(&self, max_age: Duration) -> bool {
        self.last_seen.elapsed() > max_age
    }
}

fn decode_mesh_ip(octets: [u8; 4]) -> Option<Ipv4Addr> {
    let ip = Ipv4Addr::from(octets);
    (ip != Ipv4Addr::UNSPECIFIED).then_some(ip)
}

/// Whether the routing table changed and a triggered update should be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateResult {
    /// Routing table changed; send a triggered update.
    Changed,
    /// No change.
    Unchanged,
}

// ── RoutingTable ──────────────────────────────────────────────────────────────

/// Distance-vector routing table.
///
/// All methods take `&mut self`; the caller is responsible for external
/// synchronisation (e.g. wrap in `tokio::sync::Mutex`).
pub struct RoutingTable {
    self_id: NodeId,
    is_gateway: bool,
    /// Routes to all known destinations.
    routes: HashMap<NodeId, RouteTableEntry>,
    /// Set of directly-connected peer NodeIds (updated externally by the daemon).
    direct_peers: HashSet<NodeId>,
    /// Monotonically increasing sequence number for our own advertisements.
    sequence: u64,
    /// Highest accepted sequence number per peer — used to reject replays.
    peer_max_seq: HashMap<NodeId, u64>,
    /// Peers whose routes must not be used for forwarding (reputation blacklist).
    blacklisted_peers: HashSet<NodeId>,
    self_mesh_ip: Option<Ipv4Addr>,
    /// Reverse index: mesh IPv4 → destination NodeId for O(1) `lookup_mesh_ip`.
    ///
    /// First-writer-wins: if two NodeIds derive to the same IPv4
    /// address inside the configured prefix (a birthday collision in
    /// small prefixes), the second insert is dropped and a counter
    /// increments. The colliding peer's traffic falls back to the v6
    /// path, which is collision-free at `/64`.
    mesh_ip_index: HashMap<Ipv4Addr, NodeId>,
    /// Mesh IPv4 prefix used to derive every peer's mesh IP from its
    /// `NodeId`. Read on every `add_route` / `apply_update` to
    /// populate the reverse index.
    ipv4_prefix: Ipv4Prefix,
    /// Lifetime count of `(ip, dst != existing)` rejections from the
    /// reverse index. Surfaced via the observability snapshot so a
    /// spike is operator-visible.
    mesh_ip_collisions_total: AtomicU64,
}

impl RoutingTable {
    /// Create a routing table for `self_id` inside `ipv4_prefix`.
    ///
    /// The prefix is used to derive every peer's mesh IPv4 from its
    /// `NodeId` (see [`pim_core::derive_mesh_ipv4`]). Daemons that
    /// share a mesh must agree on the prefix.
    pub fn new(self_id: NodeId, is_gateway: bool, ipv4_prefix: Ipv4Prefix) -> Self {
        Self {
            self_id,
            is_gateway,
            routes: HashMap::new(),
            direct_peers: HashSet::new(),
            sequence: 0,
            peer_max_seq: HashMap::new(),
            blacklisted_peers: HashSet::new(),
            self_mesh_ip: None,
            mesh_ip_index: HashMap::new(),
            ipv4_prefix,
            mesh_ip_collisions_total: AtomicU64::new(0),
        }
    }

    /// Replace the IPv4 prefix used for derivation. Provided for
    /// boot-ordering convenience (when the prefix is built later than
    /// the routing table). Safe to call any time; existing entries
    /// keep their previously-derived `mesh_ip` until refreshed.
    pub fn set_ipv4_prefix(&mut self, prefix: Ipv4Prefix) {
        self.ipv4_prefix = prefix;
    }

    /// Lifetime number of mesh-IP reverse-index collisions observed.
    pub fn mesh_ip_collisions_total(&self) -> u64 {
        self.mesh_ip_collisions_total.load(Ordering::Relaxed)
    }

    /// Insert or update a route, keeping `mesh_ip_index` in sync. The
    /// stored mesh IP is **derived** from `dst` plus the routing
    /// table's IPv4 prefix; any peer-advertised value is verified
    /// elsewhere and never trusted.
    fn insert_route(&mut self, dst: NodeId, mut entry: RouteTableEntry) {
        let derived = derive_mesh_ipv4(&dst, self.ipv4_prefix);
        entry.mesh_ip = Some(derived);
        if let Some(old) = self.routes.get(&dst) {
            if old.mesh_ip != Some(derived) {
                if let Some(old_ip) = old.mesh_ip {
                    self.mesh_ip_index.remove(&old_ip);
                }
            }
        }
        match self.mesh_ip_index.get(&derived) {
            Some(existing) if *existing != dst => {
                self.mesh_ip_collisions_total
                    .fetch_add(1, Ordering::Relaxed);
                info!(
                    %dst,
                    existing = %existing,
                    ip = %derived,
                    "mesh-IP derivation collision; first writer wins"
                );
            }
            _ => {
                self.mesh_ip_index.insert(derived, dst);
            }
        }
        self.routes.insert(dst, entry);
    }

    /// Remove a route, keeping `mesh_ip_index` in sync. Only removes
    /// the index entry if it still points at this `dst` — protects
    /// against losing the index for a peer that won a collision race.
    fn remove_route(&mut self, dst: &NodeId) -> Option<RouteTableEntry> {
        let entry = self.routes.remove(dst)?;
        if let Some(ip) = entry.mesh_ip {
            if self.mesh_ip_index.get(&ip) == Some(dst) {
                self.mesh_ip_index.remove(&ip);
            }
        }
        Some(entry)
    }

    /// Record our own mesh IP so advertisements can publish it.
    pub fn set_self_mesh_ip(&mut self, mesh_ip: Ipv4Addr) {
        self.self_mesh_ip = Some(mesh_ip);
    }

    /// Register a directly-connected peer (called when a transport connection is established).
    pub fn add_peer(&mut self, peer_id: NodeId) {
        self.direct_peers.insert(peer_id);
        // A direct peer is 1 hop away; install/refresh its route immediately.
        // Going through `insert_route` keeps the derived `mesh_ip`
        // and the reverse index in sync — direct peers must be
        // reachable via `lookup_mesh_ip` for the TUN ingress path.
        self.insert_route(
            peer_id,
            RouteTableEntry {
                next_hop: peer_id,
                hops: 1,
                is_gateway: false, // updated when we receive their advertisement
                last_seen: Instant::now(),
                learned_from: peer_id,
                sequence: 0,
                gateway_load: 0,
                rtt_ms: None,
                mesh_ip: None,
            },
        );
        debug!(%peer_id, "peer added to routing table");
    }

    /// Remove a directly-connected peer (called on disconnect).
    pub fn remove_peer(&mut self, peer_id: NodeId) -> UpdateResult {
        self.direct_peers.remove(&peer_id);
        self.peer_max_seq.remove(&peer_id);
        self.remove_routes_via(peer_id)
    }

    // ── Route advertisement processing ────────────────────────────────────────

    /// Process an incoming `RouteUpdateFrame` from `from_peer`.
    ///
    /// Returns `Changed` if any route was updated and a triggered advertisement
    /// should be sent to other peers.
    pub fn apply_update(&mut self, update: &RouteUpdateFrame, from_peer: NodeId) -> UpdateResult {
        // ── Security checks ───────────────────────────────────────────────────

        // The origin_id in the frame must match the direct peer sending it.
        // Relayed route advertisements are not supported; reject mismatches.
        if update.origin_id != from_peer {
            warn!(%from_peer, origin = %update.origin_id, "route update origin_id mismatch; rejecting");
            return UpdateResult::Unchanged;
        }

        // Replay protection: reject frames whose sequence number is not strictly
        // greater than the last accepted sequence from this peer.
        let last_seq = self.peer_max_seq.get(&from_peer).copied().unwrap_or(0);
        if update.sequence <= last_seq {
            debug!(%from_peer, seq = update.sequence, last_seq, "rejecting replayed route update");
            return UpdateResult::Unchanged;
        }
        self.peer_max_seq.insert(from_peer, update.sequence);

        // Anomaly detection: any entry claiming hops=0 for a destination other
        // than the sender is impossible and indicates a forged/malformed update.
        let suspicious = update
            .entries
            .iter()
            .any(|e| e.hops == 0 && e.destination != update.origin_id);
        if suspicious {
            warn!(%from_peer, "route update claims hops=0 for non-self destination; rejecting");
            return UpdateResult::Unchanged;
        }

        let mut changed = false;
        let advertised_destinations: HashSet<NodeId> = update
            .entries
            .iter()
            .map(|entry| entry.destination)
            .collect();

        // Cross-check any peer-advertised mesh_ip against derivation.
        // Mismatches surface old daemons / misconfigured labs / spoof
        // attempts at WARN — never trusted, never fatal.
        for entry in &update.entries {
            if let Some(claimed) = decode_mesh_ip(entry.mesh_ip) {
                if !verify_mesh_ipv4(&entry.destination, claimed, self.ipv4_prefix) {
                    let derived = derive_mesh_ipv4(&entry.destination, self.ipv4_prefix);
                    warn!(
                        from = %from_peer, dst = %entry.destination,
                        claimed = %claimed, derived = %derived,
                        "advertised mesh_ip does not match NodeId derivation; ignoring"
                    );
                }
            }
        }

        // Update last_seen for the advertising peer itself
        if let Some(entry) = self.routes.get_mut(&from_peer) {
            entry.last_seen = Instant::now();
        } else {
            // Heard from a previously unknown peer — add it at 1 hop
            self.insert_route(
                from_peer,
                RouteTableEntry {
                    next_hop: from_peer,
                    hops: 1,
                    is_gateway: false,
                    last_seen: Instant::now(),
                    learned_from: from_peer,
                    sequence: update.sequence,
                    gateway_load: 0,
                    rtt_ms: None,
                    mesh_ip: None, // overwritten by insert_route from derivation
                },
            );
            changed = true;
        }

        for advertised in &update.entries {
            let dst = advertised.destination;

            // Never install a route to ourselves
            if dst == self.self_id {
                continue;
            }

            // Compute new hop count: from_peer is 1 hop, plus advertised hops
            let new_hops = (advertised.hops as u16 + 1).min(INFINITY as u16) as u8;

            // Poison reverse: if from_peer sends us hops=INFINITY it means
            // they can no longer reach dst — invalidate our route if it goes via from_peer
            if new_hops >= INFINITY {
                if let Some(existing) = self.routes.get(&dst) {
                    if existing.next_hop == from_peer {
                        self.remove_route(&dst);
                        info!(%dst, via = %from_peer, "route poisoned");
                        changed = true;
                    }
                }
                continue;
            }

            let is_gw = advertised.is_gateway();

            match self.routes.get(&dst) {
                None => {
                    // New route — `mesh_ip` is overwritten by
                    // insert_route from `derive_mesh_ipv4(dst, prefix)`.
                    self.insert_route(
                        dst,
                        RouteTableEntry {
                            next_hop: from_peer,
                            hops: new_hops,
                            is_gateway: is_gw,
                            last_seen: Instant::now(),
                            learned_from: from_peer,
                            sequence: update.sequence,
                            gateway_load: 0,
                            rtt_ms: None,
                            mesh_ip: None,
                        },
                    );
                    debug!(%dst, hops = new_hops, via = %from_peer, "new route");
                    changed = true;
                }
                Some(existing) => {
                    let better_path = new_hops < existing.hops;
                    let same_peer_refresh = existing.next_hop == from_peer;
                    let newer_seq = update.sequence > existing.sequence && same_peer_refresh;

                    if better_path || newer_seq {
                        let old_hops = existing.hops;
                        let gateway_flag_flipped = existing.is_gateway != is_gw;
                        // Preserve load/rtt when refreshing an existing
                        // gateway entry. mesh_ip is re-derived by
                        // insert_route — no need to thread it through.
                        let (prev_load, prev_rtt) = if existing.is_gateway && !better_path {
                            (existing.gateway_load, existing.rtt_ms)
                        } else {
                            (0, None)
                        };
                        self.insert_route(
                            dst,
                            RouteTableEntry {
                                next_hop: from_peer,
                                hops: new_hops,
                                is_gateway: is_gw,
                                last_seen: Instant::now(),
                                learned_from: from_peer,
                                sequence: update.sequence,
                                gateway_load: prev_load,
                                rtt_ms: prev_rtt,
                                mesh_ip: None,
                            },
                        );
                        if better_path {
                            debug!(%dst, old = old_hops, new = new_hops, "route improved");
                            changed = true;
                        }
                        if gateway_flag_flipped {
                            debug!(
                                %dst, via = %from_peer,
                                gateway_flag_flipped,
                                "route attributes changed"
                            );
                            changed = true;
                        }
                    }
                }
            }
        }

        let withdrawn: Vec<NodeId> = self
            .routes
            .iter()
            .filter(|(dst, entry)| {
                entry.learned_from == from_peer
                    && **dst != from_peer
                    && !advertised_destinations.contains(*dst)
            })
            .map(|(dst, _)| *dst)
            .collect();
        if !withdrawn.is_empty() {
            changed = true;
            for dst in withdrawn {
                debug!(%dst, via = %from_peer, "route withdrawn (missing from full update)");
                self.remove_route(&dst);
            }
        }

        if changed {
            UpdateResult::Changed
        } else {
            UpdateResult::Unchanged
        }
    }

    // ── Advertisement generation ──────────────────────────────────────────────

    /// Generate a route advertisement to send to `to_peer`.
    ///
    /// Applies split horizon with poison reverse: routes learned from
    /// `to_peer` are advertised back with `hops = INFINITY`.
    pub fn generate_advertisement(&mut self, to_peer: NodeId) -> RouteUpdateFrame {
        self.sequence += 1;

        // OPTIMIZATION: Pre-allocate capacity to prevent multiple vector reallocations
        // when dealing with large numbers of route entries.
        let mut entries: Vec<RouteEntry> = Vec::with_capacity(self.routes.len() + 1);

        // Advertise our own node (0 hops, possibly as gateway)
        entries.push(RouteEntry {
            destination: self.self_id,
            hops: 0,
            flags: if self.is_gateway { 0x01 } else { 0x00 },
            mesh_ip: self.self_mesh_ip.unwrap_or(Ipv4Addr::UNSPECIFIED).octets(),
        });

        for (dst, entry) in &self.routes {
            if *dst == to_peer {
                // Don't tell a peer about itself
                continue;
            }

            let advertised_hops = if entry.learned_from == to_peer {
                // Poison reverse
                INFINITY
            } else {
                entry.hops
            };

            entries.push(RouteEntry {
                destination: *dst,
                hops: advertised_hops,
                flags: if entry.is_gateway { 0x01 } else { 0x00 },
                mesh_ip: entry.mesh_ip.unwrap_or(Ipv4Addr::UNSPECIFIED).octets(),
            });
        }

        trace!(
            to = %to_peer, entries = entries.len(), seq = self.sequence,
            "generating route advertisement"
        );

        RouteUpdateFrame {
            origin_id: self.self_id,
            sequence: self.sequence,
            entries,
            signature: [0u8; 64], // Phase 6: will be signed
        }
    }

    /// Generate advertisements for all directly connected peers.
    /// Returns a list of `(peer_id, frame)` pairs.
    pub fn generate_all_advertisements(&mut self) -> Vec<(NodeId, RouteUpdateFrame)> {
        let peers: Vec<NodeId> = self.direct_peers.iter().copied().collect();
        peers
            .into_iter()
            .map(|peer| {
                let frame = self.generate_advertisement(peer);
                (peer, frame)
            })
            .collect()
    }

    // ── Lookups ───────────────────────────────────────────────────────────────

    // ── Blacklist ─────────────────────────────────────────────────────────────

    /// Blacklist `peer_id` and remove all routes that route through it.
    ///
    /// Blacklisted peers' routes are invisible to [`lookup`] and
    /// [`nearest_gateway_route`], effectively forcing traffic onto alternate
    /// paths.
    pub fn blacklist_peer(&mut self, peer_id: NodeId) -> UpdateResult {
        self.blacklisted_peers.insert(peer_id);
        // Remove all routes whose next-hop is this peer
        self.remove_routes_via(peer_id)
    }

    /// Pardon a previously blacklisted peer.
    pub fn unblacklist_peer(&mut self, peer_id: &NodeId) {
        self.blacklisted_peers.remove(peer_id);
    }

    /// Return `true` if `peer_id` is currently blacklisted for forwarding.
    pub fn is_blacklisted(&self, peer_id: &NodeId) -> bool {
        self.blacklisted_peers.contains(peer_id)
    }

    // ── Lookups ───────────────────────────────────────────────────────────────

    /// Find the next hop for `dst`.
    ///
    /// Returns `None` if no route is known, `dst == self_id`, or the only
    /// known next-hop is blacklisted.
    pub fn lookup(&self, dst: NodeId) -> Option<NodeId> {
        if dst == self.self_id {
            return None; // deliver locally
        }
        self.routes
            .get(&dst)
            .filter(|e| !self.blacklisted_peers.contains(&e.next_hop))
            .map(|e| e.next_hop)
    }

    /// Resolve a mesh IPv4 destination to `(destination_id, next_hop)`.
    pub fn lookup_mesh_ip(&self, mesh_ip: Ipv4Addr) -> Option<(NodeId, NodeId)> {
        let dst = self.mesh_ip_index.get(&mesh_ip)?;
        let entry = self.routes.get(dst)?;
        if self.blacklisted_peers.contains(&entry.next_hop) {
            return None;
        }
        Some((*dst, entry.next_hop))
    }

    /// Internal helper: pick the best gateway entry by composite score,
    /// skipping entries whose next-hop is blacklisted.
    fn best_gateway_entry(&self) -> Option<(NodeId, &RouteTableEntry)> {
        self.routes
            .iter()
            .filter(|(_, e)| {
                e.is_gateway && e.hops < INFINITY && !self.blacklisted_peers.contains(&e.next_hop)
            })
            .min_by_key(|(_, e)| gateway_score(e.hops, e.gateway_load, e.rtt_ms))
            .map(|(id, e)| (*id, e))
    }

    /// Find the next hop and hop count to the best gateway.
    ///
    /// "Best" is determined by [`gateway_score`]: a mix of hop count, load,
    /// and measured RTT.  Returns `None` if this node is itself a gateway or
    /// no gateway route is known.
    pub fn nearest_gateway(&self) -> Option<(NodeId, u8)> {
        if self.is_gateway {
            return None; // we are the gateway
        }
        self.best_gateway_entry().map(|(_, e)| (e.next_hop, e.hops))
    }

    /// Returns `(gateway_id, next_hop)` for the best gateway.
    ///
    /// Unlike [`nearest_gateway`], this returns the gateway's own `NodeId`
    /// (the final destination) as well as the immediate next-hop peer to
    /// forward packets through.  Selection uses [`gateway_score`].
    pub fn nearest_gateway_route(&self) -> Option<(NodeId, NodeId)> {
        if self.is_gateway {
            return None;
        }
        self.best_gateway_entry().map(|(dst, e)| (dst, e.next_hop))
    }

    /// Mesh IPv4 of the currently-selected best gateway, or `None` when
    /// this node is itself a gateway / no usable gateway is known / the
    /// gateway entry doesn't yet carry an advertised `mesh_ip`.
    ///
    /// This is the `via <ip>` that the daemon's route installer should
    /// hand to `ip route replace 0.0.0.0/1 via <ip> dev pim0 onlink`,
    /// so traffic for the split-default routes lands on the elected
    /// gateway. Re-reading this on every reconciliation tick is what
    /// lets the installer follow gateway-selection swings (load /
    /// RTT changes, hop count drift, gateway disappearance) without
    /// extra plumbing — no event channel needed because gateway moves
    /// are rare and the polling cost is one HashMap scan.
    pub fn nearest_gateway_mesh_ip(&self) -> Option<Ipv4Addr> {
        if self.is_gateway {
            return None;
        }
        self.best_gateway_entry().and_then(|(_, e)| e.mesh_ip)
    }

    /// All known gateways sorted by composite score (best first).
    pub fn all_gateways(&self) -> Vec<(NodeId, u8)> {
        let mut gateways: Vec<(NodeId, u8)> = self
            .routes
            .iter()
            .filter(|(_, e)| e.is_gateway && e.hops < INFINITY)
            .map(|(dst, e)| (*dst, e.hops))
            .collect();
        gateways.sort_by_key(|(dst, _)| {
            let e = &self.routes[dst];
            gateway_score(e.hops, e.gateway_load, e.rtt_ms)
        });
        gateways
    }

    // ── Gateway metric updates ────────────────────────────────────────────────

    /// Record the forwarding load reported by a gateway in its heartbeat.
    ///
    /// No-op if `gw_id` is not a known gateway entry.
    pub fn update_gateway_load(&mut self, gw_id: NodeId, load: u8) {
        if let Some(entry) = self.routes.get_mut(&gw_id) {
            if entry.is_gateway {
                entry.gateway_load = load;
                trace!(%gw_id, load, "gateway load updated");
            }
        }
    }

    /// Record a measured round-trip time (ms) to a gateway via Ping/Pong.
    ///
    /// No-op if `gw_id` is not a known gateway entry.
    pub fn update_gateway_rtt(&mut self, gw_id: NodeId, rtt_ms: u32) {
        if let Some(entry) = self.routes.get_mut(&gw_id) {
            if entry.is_gateway {
                entry.rtt_ms = Some(rtt_ms);
                trace!(%gw_id, rtt_ms, "gateway RTT updated");
            }
        }
    }

    // ── Maintenance ───────────────────────────────────────────────────────────

    /// Remove all routes older than `max_age`. Returns `Changed` if anything
    /// was removed.
    pub fn expire_stale(&mut self, max_age: Duration) -> UpdateResult {
        let expired: Vec<NodeId> = self
            .routes
            .iter()
            .filter(|(_, e)| e.is_expired(max_age))
            .map(|(k, _)| *k)
            .collect();
        if expired.is_empty() {
            return UpdateResult::Unchanged;
        }
        for dst in expired {
            debug!(%dst, "route expired");
            self.remove_route(&dst);
        }
        UpdateResult::Changed
    }

    /// Invalidate all routes whose `next_hop` is `peer`. Returns `Changed` if
    /// anything was removed.
    pub fn remove_routes_via(&mut self, peer: NodeId) -> UpdateResult {
        let to_remove: Vec<NodeId> = self
            .routes
            .iter()
            .filter(|(_, e)| e.next_hop == peer)
            .map(|(k, _)| *k)
            .collect();
        if to_remove.is_empty() {
            return UpdateResult::Unchanged;
        }
        for dst in to_remove {
            debug!(%dst, via = %peer, "route invalidated (peer down)");
            self.remove_route(&dst);
        }
        UpdateResult::Changed
    }

    // ── Accessors ─────────────────────────────────────────────────────────────

    /// Return the local node identifier this table was created for.
    pub fn self_id(&self) -> NodeId {
        self.self_id
    }

    /// Return whether the local node itself acts as a gateway.
    pub fn is_gateway(&self) -> bool {
        self.is_gateway
    }

    /// Return the number of currently installed routes.
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }

    /// Return a cloned snapshot of all installed routes.
    pub fn routes_snapshot(&self) -> Vec<(NodeId, RouteTableEntry)> {
        self.routes
            .iter()
            .map(|(dst, entry)| (*dst, entry.clone()))
            .collect()
    }

    /// Return the set of directly connected peers.
    pub fn direct_peers(&self) -> &HashSet<NodeId> {
        &self.direct_peers
    }

    /// Return the current outbound route advertisement sequence number.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
