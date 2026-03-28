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

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use tracing::{debug, info, trace};

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
}

impl RouteTableEntry {
    fn is_expired(&self, max_age: Duration) -> bool {
        self.last_seen.elapsed() > max_age
    }
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
        }
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
        let mut changed = false;

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
                        self.routes.insert(
                            dst,
                            RouteTableEntry {
                                next_hop: from_peer,
                                hops: new_hops,
                                is_gateway: is_gw,
                                last_seen: Instant::now(),
                                learned_from: from_peer,
                                sequence: update.sequence,
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

    /// Find the next hop for `dst`.
    ///
    /// Returns `None` if no route is known or if `dst == self_id`.
    pub fn lookup(&self, dst: NodeId) -> Option<NodeId> {
        if dst == self.self_id {
            return None; // deliver locally
        }
        self.routes.get(&dst).map(|e| e.next_hop)
    }

    /// Find the next hop and hop count to the nearest gateway.
    pub fn nearest_gateway(&self) -> Option<(NodeId, u8)> {
        if self.is_gateway {
            return None; // we are the gateway
        }
        self.routes
            .values()
            .filter(|e| e.is_gateway && e.hops < INFINITY)
            .min_by_key(|e| e.hops)
            .map(|e| (e.next_hop, e.hops))
    }

    /// Returns `(gateway_id, next_hop)` for the nearest gateway.
    ///
    /// Unlike [`nearest_gateway`], this returns the gateway's own `NodeId`
    /// (the final destination) as well as the immediate next-hop peer to
    /// forward packets through.
    pub fn nearest_gateway_route(&self) -> Option<(NodeId, NodeId)> {
        if self.is_gateway {
            return None;
        }
        self.routes
            .iter()
            .filter(|(_, e)| e.is_gateway && e.hops < INFINITY)
            .min_by_key(|(_, e)| e.hops)
            .map(|(dst, e)| (*dst, e.next_hop))
    }

    /// All known gateways sorted by hop count (closest first).
    pub fn all_gateways(&self) -> Vec<(NodeId, u8)> {
        let mut gateways: Vec<(NodeId, u8)> = self
            .routes
            .iter()
            .filter(|(_, e)| e.is_gateway && e.hops < INFINITY)
            .map(|(dst, e)| (*dst, e.hops))
            .collect();
        gateways.sort_by_key(|(_, hops)| *hops);
        gateways
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
