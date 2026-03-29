//! Distance-vector routing engine for the proximity mesh.
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

pub mod signing;

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use tracing::{debug, info, trace, warn};

use pim_core::NodeId;
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
}

impl RoutingTable {
    /// Create a routing table for `self_id`.
    pub fn new(self_id: NodeId, is_gateway: bool) -> Self {
        Self {
            self_id,
            is_gateway,
            routes: HashMap::new(),
            direct_peers: HashSet::new(),
            sequence: 0,
            peer_max_seq: HashMap::new(),
            blacklisted_peers: HashSet::new(),
            self_mesh_ip: None,
        }
    }

    /// Record our own mesh IP so advertisements can publish it.
    pub fn set_self_mesh_ip(&mut self, mesh_ip: Ipv4Addr) {
        self.self_mesh_ip = Some(mesh_ip);
    }

    /// Register a directly-connected peer (called when a transport connection is established).
    pub fn add_peer(&mut self, peer_id: NodeId) {
        self.direct_peers.insert(peer_id);
        // A direct peer is 1 hop away; install/refresh its route immediately.
        self.routes.insert(
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
        let suspicious = update.entries.iter()
            .any(|e| e.hops == 0 && e.destination != update.origin_id);
        if suspicious {
            warn!(%from_peer, "route update claims hops=0 for non-self destination; rejecting");
            return UpdateResult::Unchanged;
        }

        let mut changed = false;
        let advertised_self_mesh_ip = update
            .entries
            .iter()
            .find(|entry| entry.destination == from_peer && entry.hops == 0)
            .and_then(|entry| decode_mesh_ip(entry.mesh_ip));

        // Update last_seen for the advertising peer itself
        if let Some(entry) = self.routes.get_mut(&from_peer) {
            entry.last_seen = Instant::now();
        } else {
            // Heard from a previously unknown peer — add it at 1 hop
            self.routes.insert(
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
                    mesh_ip: advertised_self_mesh_ip,
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
                        self.routes.remove(&dst);
                        info!(%dst, via = %from_peer, "route poisoned");
                        changed = true;
                    }
                }
                continue;
            }

            let is_gw = advertised.is_gateway();

            match self.routes.get(&dst) {
                None => {
                    // New route
                    self.routes.insert(
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
                            mesh_ip: decode_mesh_ip(advertised.mesh_ip),
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
                        // Preserve load/rtt when refreshing an existing gateway entry
                        let (prev_load, prev_rtt, prev_mesh_ip) = if existing.is_gateway && !better_path {
                            (existing.gateway_load, existing.rtt_ms, existing.mesh_ip)
                        } else {
                            (0, None, None)
                        };
                        let mesh_ip = decode_mesh_ip(advertised.mesh_ip).or(prev_mesh_ip);
                        self.routes.insert(
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
                                mesh_ip,
                            },
                        );
                        if better_path {
                            debug!(%dst, old = old_hops, new = new_hops, "route improved");
                            changed = true;
                        }
                    }
                }
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

        let mut entries: Vec<RouteEntry> = Vec::new();

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
        self.routes
            .iter()
            .find_map(|(dst, entry)| {
                if entry.mesh_ip == Some(mesh_ip)
                    && !self.blacklisted_peers.contains(&entry.next_hop)
                {
                    Some((*dst, entry.next_hop))
                } else {
                    None
                }
            })
    }

    /// Internal helper: pick the best gateway entry by composite score,
    /// skipping entries whose next-hop is blacklisted.
    fn best_gateway_entry(&self) -> Option<(NodeId, &RouteTableEntry)> {
        self.routes
            .iter()
            .filter(|(_, e)| {
                e.is_gateway
                    && e.hops < INFINITY
                    && !self.blacklisted_peers.contains(&e.next_hop)
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
        self.best_gateway_entry()
            .map(|(_, e)| (e.next_hop, e.hops))
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
        self.best_gateway_entry()
            .map(|(dst, e)| (dst, e.next_hop))
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
        let before = self.routes.len();
        self.routes.retain(|dst, entry| {
            let keep = !entry.is_expired(max_age);
            if !keep {
                debug!(%dst, "route expired");
            }
            keep
        });
        if self.routes.len() < before {
            UpdateResult::Changed
        } else {
            UpdateResult::Unchanged
        }
    }

    /// Invalidate all routes whose `next_hop` is `peer`. Returns `Changed` if
    /// anything was removed.
    pub fn remove_routes_via(&mut self, peer: NodeId) -> UpdateResult {
        let before = self.routes.len();
        self.routes.retain(|dst, entry| {
            let keep = entry.next_hop != peer;
            if !keep {
                debug!(%dst, via = %peer, "route invalidated (peer down)");
            }
            keep
        });
        if self.routes.len() < before {
            UpdateResult::Changed
        } else {
            UpdateResult::Unchanged
        }
    }

    // ── Accessors ─────────────────────────────────────────────────────────────

    pub fn self_id(&self) -> NodeId {
        self.self_id
    }

    pub fn is_gateway(&self) -> bool {
        self.is_gateway
    }

    pub fn route_count(&self) -> usize {
        self.routes.len()
    }

    pub fn direct_peers(&self) -> &HashSet<NodeId> {
        &self.direct_peers
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> NodeId {
        NodeId::from_bytes([n; 16])
    }

    fn mesh_ip(n: u8) -> [u8; 4] {
        [10, 77, 0, n]
    }

    /// Build a RouteUpdateFrame advertising `entries` from `origin`.
    fn advertisement(origin: NodeId, seq: u64, entries: Vec<(NodeId, u8, bool)>) -> RouteUpdateFrame {
        RouteUpdateFrame {
            origin_id: origin,
            sequence: seq,
            entries: entries
                .into_iter()
                .map(|(dst, hops, is_gw)| RouteEntry {
                    destination: dst,
                    hops,
                    flags: if is_gw { 0x01 } else { 0x00 },
                    mesh_ip: mesh_ip(dst.as_bytes()[0]),
                })
                .collect(),
            signature: [0u8; 64],
        }
    }

    // ── Basic convergence ─────────────────────────────────────────────────────

    #[test]
    fn direct_peer_route_installed_at_1_hop() {
        let mut rt = RoutingTable::new(id(1), false);
        rt.add_peer(id(2));

        assert_eq!(rt.lookup(id(2)), Some(id(2)));
        assert_eq!(rt.routes[&id(2)].hops, 1);
    }

    #[test]
    fn three_node_chain_converges() {
        // A (self) — B (direct) — C (via B)
        let a = id(1);
        let b = id(2);
        let c = id(3);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);

        // B advertises C at 1 hop
        let upd = advertisement(b, 1, vec![(c, 1, false)]);
        rt.apply_update(&upd, b);

        assert_eq!(rt.lookup(c), Some(b));
        assert_eq!(rt.routes[&c].hops, 2); // A→B (1) + B→C (1) = 2
    }

    #[test]
    fn better_route_replaces_existing() {
        let a = id(1);
        let b = id(2);
        let c = id(3);
        let d = id(4);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);
        rt.add_peer(c);

        // B advertises D at 3 hops
        rt.apply_update(&advertisement(b, 1, vec![(d, 3, false)]), b);
        assert_eq!(rt.routes[&d].hops, 4);

        // C advertises D at 1 hop → better
        rt.apply_update(&advertisement(c, 1, vec![(d, 1, false)]), c);
        assert_eq!(rt.routes[&d].hops, 2);
        assert_eq!(rt.lookup(d), Some(c));
    }

    // ── Split horizon / poison reverse ────────────────────────────────────────

    #[test]
    fn split_horizon_does_not_advertise_back() {
        let a = id(1);
        let b = id(2);
        let c = id(3);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);

        // A learned C via B
        rt.apply_update(&advertisement(b, 1, vec![(c, 1, false)]), b);

        // Advertisement to B: C should have hops=INFINITY (poison reverse)
        let adv = rt.generate_advertisement(b);
        let c_entry = adv.entries.iter().find(|e| e.destination == c).unwrap();
        assert_eq!(c_entry.hops, INFINITY, "poison reverse should set hops=INFINITY");
    }

    #[test]
    fn non_poisoned_routes_advertised_normally() {
        let a = id(1);
        let b = id(2);
        let c = id(3);
        let d = id(4);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);
        rt.add_peer(c);

        // A learned D via C
        rt.apply_update(&advertisement(c, 1, vec![(d, 1, false)]), c);

        // Advertisement to B: D was NOT learned from B → advertise normally
        let adv = rt.generate_advertisement(b);
        let d_entry = adv.entries.iter().find(|e| e.destination == d).unwrap();
        assert!(d_entry.hops < INFINITY, "D should be advertised normally to B");
    }

    // ── Invalidation ──────────────────────────────────────────────────────────

    #[test]
    fn remove_routes_via_dead_peer() {
        let a = id(1);
        let b = id(2);
        let c = id(3);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);
        rt.apply_update(&advertisement(b, 1, vec![(c, 1, false)]), b);

        assert!(rt.lookup(c).is_some());

        let result = rt.remove_peer(b);
        assert_eq!(result, UpdateResult::Changed);
        assert!(rt.lookup(b).is_none());
        assert!(rt.lookup(c).is_none());
    }

    #[test]
    fn poison_reverse_invalidates_route() {
        let a = id(1);
        let b = id(2);
        let c = id(3);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);
        rt.apply_update(&advertisement(b, 1, vec![(c, 1, false)]), b);
        assert!(rt.lookup(c).is_some());

        // B now sends hops=INFINITY for C (poison)
        let poison = advertisement(b, 2, vec![(c, INFINITY, false)]);
        let result = rt.apply_update(&poison, b);
        assert_eq!(result, UpdateResult::Changed);
        assert!(rt.lookup(c).is_none(), "poisoned route should be removed");
    }

    // ── Stale expiry ──────────────────────────────────────────────────────────

    #[test]
    fn stale_routes_expired() {
        let a = id(1);
        let b = id(2);
        let c = id(3);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);
        rt.apply_update(&advertisement(b, 1, vec![(c, 1, false)]), b);

        // Age the entries manually
        for entry in rt.routes.values_mut() {
            entry.last_seen = Instant::now() - Duration::from_secs(400);
        }

        let result = rt.expire_stale(Duration::from_secs(300));
        assert_eq!(result, UpdateResult::Changed);
        assert!(rt.lookup(c).is_none());
    }

    #[test]
    fn fresh_routes_not_expired() {
        let a = id(1);
        let b = id(2);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);

        let result = rt.expire_stale(Duration::from_secs(300));
        assert_eq!(result, UpdateResult::Unchanged);
        assert!(rt.lookup(b).is_some());
    }

    // ── Gateway selection ─────────────────────────────────────────────────────

    #[test]
    fn nearest_gateway_selected() {
        let a = id(1);
        let b = id(2); // relay, 1 hop
        let gw1 = id(10); // gateway via b at 2 hops
        let c = id(3); // relay, 1 hop
        let gw2 = id(11); // gateway via c at 3 hops

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);
        rt.add_peer(c);

        rt.apply_update(&advertisement(b, 1, vec![(gw1, 1, true)]), b);
        rt.apply_update(&advertisement(c, 1, vec![(gw2, 2, true)]), c);

        let (next_hop, hops) = rt.nearest_gateway().unwrap();
        assert_eq!(next_hop, b);
        assert_eq!(hops, 2);
    }

    #[test]
    fn gateway_node_has_no_nearest_gateway() {
        let rt = RoutingTable::new(id(1), true);
        assert!(rt.nearest_gateway().is_none());
    }

    // ── Advertisement sequence ────────────────────────────────────────────────

    #[test]
    fn own_node_included_in_advertisement() {
        let a = id(1);
        let b = id(2);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);

        let adv = rt.generate_advertisement(b);
        let self_entry = adv.entries.iter().find(|e| e.destination == a);
        assert!(self_entry.is_some(), "advertisement should include self");
        assert_eq!(self_entry.unwrap().hops, 0);
    }

    #[test]
    fn sequence_increments_per_advertisement() {
        let a = id(1);
        let b = id(2);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);

        let s1 = rt.generate_advertisement(b).sequence;
        let s2 = rt.generate_advertisement(b).sequence;
        assert!(s2 > s1);
    }

    #[test]
    fn no_loop_route_self() {
        let a = id(1);
        let b = id(2);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);

        // B tries to advertise A back to A (shouldn't be installed)
        let upd = advertisement(b, 1, vec![(a, 1, false)]);
        rt.apply_update(&upd, b);

        // There's no route to self in the table
        assert!(rt.routes.get(&a).is_none());
        assert!(rt.lookup(a).is_none());
    }

    // ── Phase 6: Routing security ─────────────────────────────────────────────

    #[test]
    fn replay_rejected_same_sequence() {
        let a = id(1);
        let b = id(2);
        let c = id(3);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);

        let upd = advertisement(b, 5, vec![(c, 1, false)]);
        assert_eq!(rt.apply_update(&upd, b), UpdateResult::Changed);

        // Same seq again — replay
        let upd2 = advertisement(b, 5, vec![(c, 1, false)]);
        assert_eq!(rt.apply_update(&upd2, b), UpdateResult::Unchanged);
    }

    #[test]
    fn replay_rejected_lower_sequence() {
        let a = id(1);
        let b = id(2);
        let c = id(3);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);

        rt.apply_update(&advertisement(b, 10, vec![(c, 1, false)]), b);
        // Seq 5 < 10 — replay
        let result = rt.apply_update(&advertisement(b, 5, vec![(c, 1, false)]), b);
        assert_eq!(result, UpdateResult::Unchanged);
    }

    #[test]
    fn higher_sequence_accepted() {
        let a = id(1);
        let b = id(2);
        let c = id(3);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);

        rt.apply_update(&advertisement(b, 1, vec![(c, 2, false)]), b);
        let result = rt.apply_update(&advertisement(b, 2, vec![(c, 1, false)]), b);
        assert_eq!(result, UpdateResult::Changed);
        assert_eq!(rt.routes[&c].hops, 2); // 1 advertised + 1 for the hop to b
    }

    #[test]
    fn origin_mismatch_rejected() {
        let a = id(1);
        let b = id(2);
        let c = id(3);
        let impostor = id(99);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);

        // Frame claims origin=impostor but is sent by b
        let upd = RouteUpdateFrame {
            origin_id: impostor,
            sequence: 1,
            entries: vec![RouteEntry { destination: c, hops: 1, flags: 0, mesh_ip: mesh_ip(3) }],
            signature: [0u8; 64],
        };
        let result = rt.apply_update(&upd, b);
        assert_eq!(result, UpdateResult::Unchanged, "mismatched origin must be rejected");
        assert!(rt.lookup(c).is_none());
    }

    #[test]
    fn zero_hop_to_non_self_rejected() {
        let a = id(1);
        let b = id(2);
        let victim = id(42);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);

        // b claims to be 0 hops from victim (impossible unless b == victim)
        let upd = RouteUpdateFrame {
            origin_id: b,
            sequence: 1,
            entries: vec![RouteEntry { destination: victim, hops: 0, flags: 0, mesh_ip: mesh_ip(42) }],
            signature: [0u8; 64],
        };
        let result = rt.apply_update(&upd, b);
        assert_eq!(result, UpdateResult::Unchanged, "0-hop claim for non-self must be rejected");
    }

    #[test]
    fn zero_hop_self_entry_allowed() {
        let a = id(1);
        let b = id(2);
        let c = id(3);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);

        // b claims 0 hops to itself (normal — every advertisement includes this)
        let upd = RouteUpdateFrame {
            origin_id: b,
            sequence: 1,
            entries: vec![
                RouteEntry { destination: b, hops: 0, flags: 0, mesh_ip: mesh_ip(2) }, // self-entry — OK
                RouteEntry { destination: c, hops: 1, flags: 0, mesh_ip: mesh_ip(3) },
            ],
            signature: [0u8; 64],
        };
        let result = rt.apply_update(&upd, b);
        assert_eq!(result, UpdateResult::Changed, "self zero-hop entry should be accepted");
        assert!(rt.lookup(c).is_some());
    }

    #[test]
    fn blacklisted_peer_route_invisible() {
        let a = id(1);
        let b = id(2);
        let c = id(3);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);
        rt.apply_update(&advertisement(b, 1, vec![(c, 1, false)]), b);

        assert!(rt.lookup(c).is_some());

        rt.blacklist_peer(b);
        assert!(rt.lookup(b).is_none(), "blacklisted peer itself invisible");
        assert!(rt.lookup(c).is_none(), "routes via blacklisted peer invisible");
    }

    #[test]
    fn blacklisted_gateway_excluded_from_selection() {
        let a = id(1);
        let gw1 = id(10);
        let gw2 = id(11);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(gw1);
        rt.add_peer(gw2);
        rt.apply_update(&advertisement(gw1, 1, vec![(gw1, 0, true)]), gw1);
        rt.apply_update(&advertisement(gw2, 1, vec![(gw2, 0, true)]), gw2);

        // Normally gw1 is picked (same hops, comes first)
        assert!(rt.nearest_gateway_route().is_some());

        rt.blacklist_peer(gw1);
        let (dst, _) = rt.nearest_gateway_route().unwrap();
        assert_eq!(dst, gw2, "blacklisted gateway must be skipped");
    }

    #[test]
    fn unblacklist_restores_visibility() {
        let a = id(1);
        let b = id(2);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);
        rt.blacklist_peer(b);
        assert!(rt.lookup(b).is_none());

        rt.unblacklist_peer(&b);
        // Route was removed by blacklist_peer; re-add it
        rt.add_peer(b);
        assert!(rt.lookup(b).is_some());
    }

    // ── Phase 5: Multi-gateway scoring ───────────────────────────────────────

    #[test]
    fn gateway_score_zero_for_1hop_idle() {
        // 1 hop, 0 load, no RTT → score = 100
        assert_eq!(gateway_score(1, 0, None), 100);
    }

    #[test]
    fn gateway_score_load_adds_penalty() {
        // 1 hop + full load (255) → 100 + 127 = 227
        assert_eq!(gateway_score(1, 255, None), 227);
    }

    #[test]
    fn gateway_score_rtt_adds_penalty() {
        // 1 hop + 1000ms RTT → 100 + 0 + 100 = 200
        assert_eq!(gateway_score(1, 0, Some(1000)), 200);
    }

    #[test]
    fn gateway_score_combined() {
        // 2 hops + load 100 + rtt 200ms → 200 + 50 + 20 = 270
        assert_eq!(gateway_score(2, 100, Some(200)), 270);
    }

    #[test]
    fn load_aware_selection_prefers_less_loaded_gateway() {
        // relay_a → gw1: 1 advertised hop → 2 total hops, heavy load (220) → score=200+110=310
        // relay_b → gw2: 2 advertised hops → 3 total hops, no load         → score=300
        // gw2 should win despite being farther
        let self_id = id(1);
        let relay_a = id(2);
        let relay_b = id(3);
        let gw1 = id(10);
        let gw2 = id(11);

        let mut rt = RoutingTable::new(self_id, false);
        rt.add_peer(relay_a);
        rt.add_peer(relay_b);

        rt.apply_update(&advertisement(relay_a, 1, vec![(gw1, 1, true)]), relay_a);
        rt.apply_update(&advertisement(relay_b, 1, vec![(gw2, 2, true)]), relay_b);

        rt.update_gateway_load(gw1, 220);

        // gw1: hops=2, load=220 → score=200+110=310
        // gw2: hops=3, load=0   → score=300
        let (dst, _next_hop) = rt.nearest_gateway_route().unwrap();
        assert_eq!(dst, gw2, "less-loaded 3-hop gateway should beat saturated 2-hop gateway");
    }

    #[test]
    fn rtt_aware_selection_prefers_lower_latency_gateway() {
        // gw1: 2 hops, no load, rtt=800ms → score=200+0+80=280
        // gw2: 3 hops, no load, no rtt    → score=300
        // gw1 wins (280 < 300)
        let self_id = id(1);
        let relay_a = id(2);
        let relay_b = id(3);
        let gw1 = id(10);
        let gw2 = id(11);

        let mut rt = RoutingTable::new(self_id, false);
        rt.add_peer(relay_a);
        rt.add_peer(relay_b);

        rt.apply_update(&advertisement(relay_a, 1, vec![(gw1, 1, true)]), relay_a);
        rt.apply_update(&advertisement(relay_b, 1, vec![(gw2, 2, true)]), relay_b);

        rt.update_gateway_rtt(gw1, 800);
        // gw1 score = 200+0+80 = 280, gw2 score = 300
        let (dst, _) = rt.nearest_gateway_route().unwrap();
        assert_eq!(dst, gw1, "lower-RTT 2-hop gateway should beat clean 3-hop gateway at 800ms");
    }

    #[test]
    fn rtt_aware_selection_switches_at_high_latency() {
        // gw1: 2 hops, no load, rtt=3000ms → score=200+0+300=500
        // gw2: 3 hops, no load, no rtt     → score=300
        // gw2 wins
        let self_id = id(1);
        let relay_a = id(2);
        let relay_b = id(3);
        let gw1 = id(10);
        let gw2 = id(11);

        let mut rt = RoutingTable::new(self_id, false);
        rt.add_peer(relay_a);
        rt.add_peer(relay_b);

        rt.apply_update(&advertisement(relay_a, 1, vec![(gw1, 1, true)]), relay_a);
        rt.apply_update(&advertisement(relay_b, 1, vec![(gw2, 2, true)]), relay_b);

        rt.update_gateway_rtt(gw1, 3000);
        // gw1 score = 500, gw2 score = 300
        let (dst, _) = rt.nearest_gateway_route().unwrap();
        assert_eq!(dst, gw2, "3-hop gateway should beat 2-hop with 3s RTT");
    }

    #[test]
    fn failover_to_second_gateway_when_first_removed() {
        // gw1: 2 hops (clearly better); gw2: 3 hops
        let self_id = id(1);
        let relay_a = id(2);
        let relay_b = id(3);
        let gw1 = id(10);
        let gw2 = id(11);

        let mut rt = RoutingTable::new(self_id, false);
        rt.add_peer(relay_a);
        rt.add_peer(relay_b);

        rt.apply_update(&advertisement(relay_a, 1, vec![(gw1, 1, true)]), relay_a);
        rt.apply_update(&advertisement(relay_b, 1, vec![(gw2, 2, true)]), relay_b);

        // Initially gw1 is preferred (2 hops < 3 hops)
        let (dst, _) = rt.nearest_gateway_route().unwrap();
        assert_eq!(dst, gw1);

        // gw1's relay goes down
        rt.remove_peer(relay_a);

        // Failover to gw2
        let (dst, _) = rt.nearest_gateway_route().unwrap();
        assert_eq!(dst, gw2, "should fail over to second gateway");
    }

    #[test]
    fn all_gateways_sorted_by_score() {
        let self_id = id(1);
        let relay_a = id(2);
        let relay_b = id(3);
        let gw1 = id(10); // 2 hops, load=200 → score=200+100=300
        let gw2 = id(11); // 3 hops, load=0   → score=300

        let mut rt = RoutingTable::new(self_id, false);
        rt.add_peer(relay_a);
        rt.add_peer(relay_b);

        rt.apply_update(&advertisement(relay_a, 1, vec![(gw1, 1, true)]), relay_a);
        rt.apply_update(&advertisement(relay_b, 1, vec![(gw2, 2, true)]), relay_b);

        rt.update_gateway_load(gw1, 200);
        // gw1: score=200+100=300; gw2: score=300+0=300 → tie broken by map iteration (any order)
        // Just verify both appear
        let gws = rt.all_gateways();
        assert_eq!(gws.len(), 2);
        let ids: Vec<NodeId> = gws.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&gw1));
        assert!(ids.contains(&gw2));
    }

    #[test]
    fn update_gateway_load_only_applies_to_gateway_entries() {
        let mut rt = RoutingTable::new(id(1), false);
        rt.add_peer(id(2));
        // id(2) is not a gateway — update_gateway_load should be a no-op
        rt.update_gateway_load(id(2), 200);
        assert_eq!(rt.routes[&id(2)].gateway_load, 0);
    }

    #[test]
    fn update_gateway_rtt_only_applies_to_gateway_entries() {
        let mut rt = RoutingTable::new(id(1), false);
        rt.add_peer(id(2));
        rt.update_gateway_rtt(id(2), 50);
        assert_eq!(rt.routes[&id(2)].rtt_ms, None);
    }

    #[test]
    fn update_returns_changed_only_on_change() {
        let a = id(1);
        let b = id(2);
        let c = id(3);

        let mut rt = RoutingTable::new(a, false);
        rt.add_peer(b);

        let upd = advertisement(b, 1, vec![(c, 1, false)]);
        assert_eq!(rt.apply_update(&upd, b), UpdateResult::Changed);

        // Same advertisement again — not changed (same seq, same hops)
        // In our impl: same seq + same peer → no update
        let upd2 = advertisement(b, 1, vec![(c, 1, false)]);
        assert_eq!(rt.apply_update(&upd2, b), UpdateResult::Unchanged);
    }
}
